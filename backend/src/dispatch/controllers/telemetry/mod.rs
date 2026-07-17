//! 请求终态遥测的唯一 feature owner。

mod events;

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{
    dispatch::{
        controllers::telemetry::events::{
            LiveResponseStreamEventRecord, ResponseUpstreamErrorEventRecord,
            ResponseUsageEventRecord, enrich_response_request_semantics,
            insert_openai_processing_ms, insert_output_timing_ms, insert_response_status_metadata,
            insert_response_trace_metadata, insert_response_upstream_diagnostics,
            insert_websocket_pool_decision, reasoning_effort_from_request,
            record_live_response_stream_event, record_response_event,
            record_response_upstream_error_event,
        },
        controllers::{
            AttemptUpstreamErrorContext, CompleteExit, DispatchErrorObservation,
            PrefetchedStreamFailureObservation,
        },
        errors::{ResponseDispatchError, backend_transport_name},
        failure::sse::{
            STREAM_DISCONNECTED_CODE, STREAM_DISCONNECTED_MESSAGE, stream_failure_metadata,
            stream_failure_source, synthetic_stream_disconnected_detail,
        },
        lifecycle::{
            stream::{StreamSummary, StreamTerminal},
            trace::ResponseDispatchAttempt,
        },
    },
    infra::time::elapsed_millis_i64,
    telemetry::{recorder::Recorder, usage::types::UsageRecordLevel},
    upstream::openai::{
        protocol::responses::{CodexResponsesRequest, ResponsesSseFailure},
        transport::{
            CodexBackendTransport, CodexResponseMetadata, CodexTransportMetrics,
            CodexUpstreamDiagnostics, WebSocketPoolDecision,
        },
    },
};

pub(super) struct TelemetryController;

pub(super) struct StreamContext<'a> {
    pub recorder: &'a Recorder,
    pub account_id: &'a str,
    pub request_id: &'a str,
    pub route: &'a str,
    pub display_model: &'a str,
    pub requested_model: &'a str,
    pub request: &'a CodexResponsesRequest,
    pub transport: CodexBackendTransport,
    pub websocket_pool_decision: Option<WebSocketPoolDecision>,
    pub diagnostics: &'a CodexUpstreamDiagnostics,
    pub response_metadata: &'a CodexResponseMetadata,
    pub transport_metrics: &'a CodexTransportMetrics,
    pub attempt: &'a ResponseDispatchAttempt,
    pub attempts: &'a [ResponseDispatchAttempt],
    pub started_at: std::time::Instant,
}

pub(super) struct StreamExit<'a> {
    pub context: StreamContext<'a>,
    pub summary: &'a StreamSummary,
    pub rate_limit_headers: &'a [(String, String)],
    pub failure_status: Option<i64>,
    pub body: &'a str,
}

impl TelemetryController {
    pub(super) async fn observe_dispatch_error(
        recorder: &Recorder,
        observation: DispatchErrorObservation<'_>,
        error: &ResponseDispatchError,
    ) {
        events::record_response_dispatch_error_event(events::ResponseDispatchErrorEventRecord {
            recorder,
            request_id: observation.request_id,
            client_api_key_id: observation.client_api_key_id,
            account_id: observation.account_id,
            route: observation.route,
            model: observation.model,
            started_at: observation.started_at,
            stream: observation.stream,
            compact: observation.compact,
            transport: observation.transport,
            error,
        })
        .await;
    }

    pub(super) async fn observe_prefetched_stream_failure(
        recorder: &Recorder,
        observation: PrefetchedStreamFailureObservation<'_>,
    ) {
        events::record_prefetched_response_stream_failure_event(
            events::ResponseStreamFailureEventRecord {
                recorder,
                request_id: observation.request_id,
                account_id: observation.account_id,
                route: observation.route,
                model: observation.model,
                requested_model: observation.requested_model,
                started_at: observation.started_at,
                transport: observation.transport,
                request: observation.request,
                failure: observation.failure,
                status_code: i64::from(observation.error.client_http_status_code()),
                diagnostics: observation.diagnostics,
                rate_limit_headers: observation.rate_limit_headers,
                prefetched: observation.prefetched,
                trace: observation.trace,
                attempt: observation.attempt,
            },
        )
        .await;
    }

    pub(super) async fn observe_upstream_error(
        recorder: &Recorder,
        context: &AttemptUpstreamErrorContext<'_>,
    ) {
        record_response_upstream_error_event(ResponseUpstreamErrorEventRecord {
            recorder,
            request_id: context.request_id,
            account_id: &context.account.id,
            account_email: context.account.email.as_deref(),
            route: context.route,
            model: context.model,
            started_at: context.started_at,
            stream: context.stream,
            transport: context.transport,
            request: context.request,
            error: context.error,
            trace: context.trace,
            attempt: Some(context.attempt),
        })
        .await;
    }

    pub(super) async fn leave_complete(exit: &CompleteExit<'_>) {
        let effective_model = exit
            .response
            .response_metadata
            .effective_model
            .as_deref()
            .unwrap_or(exit.display_model);
        let first_token_ms = request_timing_ms(
            exit.started_at,
            exit.attempt.started_at(),
            exit.response.first_token_ms,
        );
        let first_reasoning_ms = request_timing_ms(
            exit.started_at,
            exit.attempt.started_at(),
            exit.response.first_reasoning_ms,
        );
        let first_text_ms = request_timing_ms(
            exit.started_at,
            exit.attempt.started_at(),
            exit.response.first_text_ms,
        );
        let mut metadata = serde_json::json!({
            "responseId": exit.response_id,
            "stream": false,
            "completed": exit.completed,
            "incomplete": !exit.completed,
            "transport": backend_transport_name(exit.response.transport),
            "usage": exit.response.usage,
            "effectiveModel": effective_model,
            "modelsEtag": exit.response.response_metadata.models_etag.as_deref(),
            "reasoningIncluded": exit.response.response_metadata.reasoning_included,
            "transportDecision": exit.response.transport_metrics.decision.map(|decision| decision.as_str()),
            "wsConnectMs": exit.response.transport_metrics.ws_connect_ms,
            "transportDecisionWaitMs": exit.response.transport_metrics.transport_decision_wait_ms,
            "upstreamHeadersMs": exit.response.transport_metrics.upstream_headers_ms,
            "firstEventMs": exit.response.transport_metrics.first_event_ms,
            "httpVersion": exit.response.transport_metrics.http_version.as_deref(),
        });
        insert_output_timing_ms(
            &mut metadata,
            first_token_ms,
            first_reasoning_ms,
            first_text_ms,
        );
        insert_openai_processing_ms(&mut metadata, &exit.response.response_metadata);
        insert_response_status_metadata(
            &mut metadata,
            200,
            200,
            exit.response.diagnostics.status_code.map(i64::from),
        );
        insert_response_upstream_diagnostics(&mut metadata, &exit.response.diagnostics);
        insert_response_trace_metadata(&mut metadata, exit.trace, Some(exit.attempt));
        insert_websocket_pool_decision(&mut metadata, exit.response.websocket_pool_decision);
        enrich_response_request_semantics(&mut metadata, exit.request);
        record_response_event(ResponseUsageEventRecord {
            recorder: exit.recorder,
            request_id: exit.request_id,
            client_api_key_id: exit.request.client_api_key_id.as_deref(),
            account_id: exit.account_id,
            route: exit.route,
            model: effective_model,
            requested_model: Some(exit.requested_model),
            client_ip: exit.request.client_ip.as_deref(),
            client_user_agent: exit.request.client_user_agent.as_deref(),
            reasoning_effort: reasoning_effort_from_request(exit.request),
            service_tier: exit.request.service_tier(),
            started_at: exit.started_at,
            status_code: 200,
            message: if exit.completed {
                "v1 responses completed"
            } else {
                "v1 responses incomplete"
            },
            usage: exit.response.usage,
            metadata,
            rate_limit_headers: &exit.response.rate_limit_headers,
        })
        .await;
    }

    pub(super) async fn leave_stream(exit: StreamExit<'_>) {
        let context = &exit.context;
        let summary = exit.summary;
        let rate_limit_headers = exit.rate_limit_headers;
        let body = exit.body;
        match &summary.terminal {
            StreamTerminal::Completed { response } => {
                record_success(context, summary, response, true, rate_limit_headers, body).await;
            }
            StreamTerminal::Incomplete { response } => {
                record_success(context, summary, response, false, rate_limit_headers, body).await;
            }
            StreamTerminal::Failed { failure } => {
                record_failure(
                    context,
                    summary,
                    failure,
                    exit.failure_status.unwrap_or(502),
                    rate_limit_headers,
                    body,
                )
                .await;
            }
            terminal @ (StreamTerminal::UpstreamClosed
            | StreamTerminal::UpstreamError { .. }
            | StreamTerminal::ProtocolError { .. }
            | StreamTerminal::CaptureLimitExceeded) => {
                let failure = synthetic_failure(terminal);
                record_failure(context, summary, &failure, 502, rate_limit_headers, body).await;
            }
            terminal @ (StreamTerminal::Cancelled
            | StreamTerminal::DownstreamClosed
            | StreamTerminal::Shutdown) => {
                let status_code = if matches!(terminal, StreamTerminal::Shutdown) {
                    503
                } else {
                    499
                };
                let mut metadata = serde_json::json!({
                    "stream": true,
                    "cancelled": true,
                    "terminal": terminal.name(),
                    "responseId": summary.last_response_id,
                    "firstEventMs": summary.first_event_ms,
                    "usage": summary.usage,
                });
                insert_output_timing_ms(
                    &mut metadata,
                    summary.first_token_ms,
                    summary.first_reasoning_ms,
                    summary.first_text_ms,
                );
                record_live_response_stream_event(LiveResponseStreamEventRecord {
                    context,
                    status_code,
                    level: UsageRecordLevel::Warn,
                    message: "v1 responses stream cancelled",
                    usage: summary.usage,
                    metadata,
                    rate_limit_headers,
                    body,
                })
                .await;
            }
        }
    }
}

fn request_timing_ms(
    request_started_at: Instant,
    attempt_started_at: Instant,
    attempt_first_token_ms: Option<i64>,
) -> Option<i64> {
    let attempt_first_token_ms = u64::try_from(attempt_first_token_ms?).ok()?;
    let output_at =
        attempt_started_at.checked_add(Duration::from_millis(attempt_first_token_ms))?;
    i64::try_from(
        output_at
            .saturating_duration_since(request_started_at)
            .as_millis(),
    )
    .ok()
}

async fn record_success(
    context: &StreamContext<'_>,
    summary: &StreamSummary,
    response: &Value,
    completed: bool,
    rate_limit_headers: &[(String, String)],
    body: &str,
) {
    let mut metadata = serde_json::json!({
        "stream": true,
        "completed": completed,
        "incomplete": !completed,
        "responseId": response.get("id").and_then(Value::as_str),
        "firstEventMs": summary.first_event_ms,
        "usage": summary.usage,
    });
    insert_output_timing_ms(
        &mut metadata,
        summary.first_token_ms,
        summary.first_reasoning_ms,
        summary.first_text_ms,
    );
    record_live_response_stream_event(LiveResponseStreamEventRecord {
        context,
        status_code: 200,
        level: UsageRecordLevel::Info,
        message: if completed {
            "v1 responses stream completed"
        } else {
            "v1 responses stream incomplete"
        },
        usage: summary.usage,
        metadata,
        rate_limit_headers,
        body,
    })
    .await;
}

fn synthetic_failure(terminal: &StreamTerminal) -> ResponsesSseFailure {
    let detail = terminal.synthetic_failure_detail().unwrap_or_default();
    ResponsesSseFailure {
        event: "response.failed".to_string(),
        message: if detail.trim().is_empty() {
            STREAM_DISCONNECTED_MESSAGE.to_string()
        } else {
            format!("{STREAM_DISCONNECTED_MESSAGE}: {detail}")
        },
        upstream_code: Some(STREAM_DISCONNECTED_CODE.to_string()),
        upstream_type: Some("server_error".to_string()),
        explicit_status_code: None,
        retry_after_seconds: None,
    }
}

async fn record_failure(
    context: &StreamContext<'_>,
    summary: &StreamSummary,
    failure: &ResponsesSseFailure,
    status_code: i64,
    rate_limit_headers: &[(String, String)],
    body: &str,
) {
    let failure_source = stream_failure_source(failure);
    let failure_detail = synthetic_stream_disconnected_detail(failure);
    tracing::warn!(
        account_id = %context.account_id,
        request_id = %context.request_id,
        response_id = summary.last_response_id.as_deref().unwrap_or(""),
        transport = %backend_transport_name(context.transport),
        websocket_pool_kind = ?context.websocket_pool_decision.map(WebSocketPoolDecision::kind),
        first_token_ms = ?summary.first_token_ms,
        first_event_ms = summary.first_event_ms,
        latency_ms = elapsed_millis_i64(context.started_at),
        event = %failure.event,
        code = ?failure.upstream_code.as_deref(),
        failure_source,
        failure_detail = ?failure_detail.as_deref(),
        "Live upstream stream ended with response.failed"
    );
    let mut metadata = stream_failure_metadata(failure, summary.usage);
    metadata["responseId"] = summary
        .last_response_id
        .as_ref()
        .map_or(Value::Null, |response_id| {
            Value::String(response_id.clone())
        });
    insert_output_timing_ms(
        &mut metadata,
        summary.first_token_ms,
        summary.first_reasoning_ms,
        summary.first_text_ms,
    );
    metadata["firstEventMs"] = Value::Number(summary.first_event_ms.into());
    record_live_response_stream_event(LiveResponseStreamEventRecord {
        context,
        status_code,
        level: UsageRecordLevel::Error,
        message: "v1 responses stream failed",
        usage: summary.usage,
        metadata,
        rate_limit_headers,
        body,
    })
    .await;
}
