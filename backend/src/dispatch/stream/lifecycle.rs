use std::{sync::Arc, time::Instant};

use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    accounts::pool::RuntimeAccountPoolService,
    dispatch::{
        affinity::{
            resolve::{evict_reasoning_replay, record_response_affinity},
            RuntimeSessionAffinityService,
        },
        errors::backend_transport_name,
        recording::{
            insert_first_token_ms, live_response_rate_limit_headers, live_response_turn_state,
            record_live_response_stream_event,
        },
        recovery::reasoning_replay::ReasoningReplayCache,
    },
    infra::time::elapsed_millis_i64,
    telemetry::{recorder::Recorder, usage::types::UsageRecordLevel},
    upstream::openai::{
        protocol::{
            events::extract_sse_usage,
            responses::{response_from_codex_sse, CodexResponsesRequest, CollectedResponse},
            sse::{parse_sse_events, response_failed_sse_event_with_id},
        },
        transport::{
            CodexBackendTransport, CodexRateLimitHeaderUpdates, CodexTurnStateUpdate,
            CodexUpstreamDiagnostics, WebSocketPoolDecision,
        },
    },
};

use super::{
    sse_failure::{
        sse_failure_invalid_reasoning_replay, status_code_for_stream_failure,
        stream_failure_metadata, stream_failure_source, synthetic_stream_disconnected_detail,
        STREAM_DISCONNECTED_CODE, STREAM_DISCONNECTED_MESSAGE,
    },
    trace::ResponseDispatchAttempt,
};
use crate::dispatch::recovery::cloudflare::CloudflareRecovery;

pub(in crate::dispatch) struct LiveResponseStreamContext {
    pub(in crate::dispatch) account_pool: Arc<RuntimeAccountPoolService>,
    pub(in crate::dispatch) session_affinity: Arc<RuntimeSessionAffinityService>,
    pub(in crate::dispatch) reasoning_replay: Arc<Mutex<ReasoningReplayCache>>,
    pub(in crate::dispatch) recorder: Arc<Recorder>,
    pub(in crate::dispatch) cloudflare: CloudflareRecovery,
    pub(in crate::dispatch) account_id: String,
    pub(in crate::dispatch) account_plan_type: Option<String>,
    pub(in crate::dispatch) request_id: String,
    pub(in crate::dispatch) route: String,
    pub(in crate::dispatch) model: String,
    pub(in crate::dispatch) display_model: String,
    pub(in crate::dispatch) requested_model: String,
    pub(in crate::dispatch) client_ip: Option<String>,
    pub(in crate::dispatch) request: CodexResponsesRequest,
    pub(in crate::dispatch) tuple_schema: Option<Value>,
    pub(in crate::dispatch) transport: CodexBackendTransport,
    pub(in crate::dispatch) rate_limit_headers: Vec<(String, String)>,
    pub(in crate::dispatch) rate_limit_header_updates: Option<CodexRateLimitHeaderUpdates>,
    pub(in crate::dispatch) turn_state_update: Option<CodexTurnStateUpdate>,
    pub(in crate::dispatch) websocket_pool_decision: Option<WebSocketPoolDecision>,
    pub(in crate::dispatch) turn_state: Option<String>,
    pub(in crate::dispatch) diagnostics: CodexUpstreamDiagnostics,
    pub(in crate::dispatch) attempt: ResponseDispatchAttempt,
    pub(in crate::dispatch) attempts: Vec<ResponseDispatchAttempt>,
    pub(in crate::dispatch) started_at: Instant,
}

pub(in crate::dispatch) fn latest_response_id(body: &str) -> Option<String> {
    parse_sse_events(body).ok().and_then(|events| {
        events.iter().rev().find_map(|event| {
            serde_json::from_str::<Value>(&event.data)
                .ok()
                .and_then(|data| {
                    data.pointer("/response/id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                        .map(ToString::to_string)
                })
        })
    })
}

pub(in crate::dispatch) fn premature_close_failed_event(
    response_id: Option<&str>,
    detail: Option<&str>,
) -> String {
    let message = match detail.filter(|value| !value.trim().is_empty()) {
        Some(detail) => format!("{STREAM_DISCONNECTED_MESSAGE}: {detail}"),
        None => STREAM_DISCONNECTED_MESSAGE.to_string(),
    };
    response_failed_sse_event_with_id(
        response_id,
        "server_error",
        STREAM_DISCONNECTED_CODE,
        &message,
    )
}

pub(in crate::dispatch) async fn finalize_live_response_stream(
    context: LiveResponseStreamContext,
    body: String,
    first_token_ms: Option<i64>,
) {
    let rate_limit_headers = live_response_rate_limit_headers(&context).await;
    context
        .account_pool
        .sync_passive_rate_limit_headers_for_account(
            &context.account_id,
            context.account_plan_type.as_deref(),
            &rate_limit_headers,
        )
        .await;
    let turn_state = live_response_turn_state(&context).await;
    let usage = match extract_sse_usage(&body) {
        Ok(Some(usage)) => {
            context
                .account_pool
                .record_token_usage(&context.account_id, &context.model, &usage)
                .await;
            Some(usage)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(account_id = %context.account_id, error = %error, "failed to extract streaming token usage");
            None
        }
    };

    match response_from_codex_sse(&body, context.tuple_schema.as_ref()) {
        Ok(CollectedResponse::Completed(completed)) => {
            context
                .cloudflare
                .reset_account_recovery(&context.account_id)
                .await;
            let response_id = completed.get("id").and_then(Value::as_str);
            record_response_affinity(
                &context.session_affinity,
                &context.reasoning_replay,
                &context.request,
                &context.account_id,
                &body,
                turn_state,
                usage,
            )
            .await;
            record_live_response_stream_event(
                &context,
                200,
                UsageRecordLevel::Info,
                "v1 responses stream completed",
                serde_json::json!({
                    "stream": true,
                    "completed": true,
                    "responseId": response_id,
                    "firstTokenMs": first_token_ms,
                    "usage": usage,
                }),
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Ok(CollectedResponse::Failed(failure)) => {
            if sse_failure_invalid_reasoning_replay(&failure) {
                evict_reasoning_replay(
                    &context.reasoning_replay,
                    &context.request,
                    &context.account_id,
                )
                .await;
            }
            let response_id = latest_response_id(&body);
            let latency_ms = elapsed_millis_i64(context.started_at);
            let failure_source = stream_failure_source(&failure);
            let failure_detail = synthetic_stream_disconnected_detail(&failure);
            let websocket_pool_kind = context
                .websocket_pool_decision
                .map(|decision| decision.kind());
            let websocket_pool_reason = context
                .websocket_pool_decision
                .and_then(|decision| decision.reason());
            tracing::warn!(
                account_id = %context.account_id,
                request_id = %context.request_id,
                response_id = response_id.as_deref().unwrap_or(""),
                transport = %backend_transport_name(context.transport),
                websocket_pool_kind = ?websocket_pool_kind,
                websocket_pool_reason = ?websocket_pool_reason,
                first_token_ms = ?first_token_ms,
                latency_ms,
                event = %failure.event,
                code = ?failure.upstream_code.as_deref(),
                failure_source = %failure_source,
                failure_detail = ?failure_detail.as_deref(),
                "live upstream stream ended with response.failed"
            );
            let mut metadata = stream_failure_metadata(&failure, usage);
            insert_first_token_ms(&mut metadata, first_token_ms);
            record_live_response_stream_event(
                &context,
                status_code_for_stream_failure(&failure),
                UsageRecordLevel::Error,
                "v1 responses stream failed",
                metadata,
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Ok(CollectedResponse::MissingCompleted | CollectedResponse::Empty) => {
            tracing::warn!(
                account_id = %context.account_id,
                "live upstream stream ended without response.completed"
            );
            let mut metadata = serde_json::json!({
                "stream": true,
                "failed": true,
                "upstreamCode": "missing_completed",
                "usage": usage,
            });
            insert_first_token_ms(&mut metadata, first_token_ms);
            record_live_response_stream_event(
                &context,
                502,
                UsageRecordLevel::Error,
                "v1 responses stream ended without response.completed",
                metadata,
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Err(error) => {
            tracing::warn!(account_id = %context.account_id, error = %error, "failed to parse completed live stream");
            let mut metadata = serde_json::json!({
                "stream": true,
                "sseParseError": error.to_string(),
                "usage": usage,
            });
            insert_first_token_ms(&mut metadata, first_token_ms);
            record_live_response_stream_event(
                &context,
                502,
                UsageRecordLevel::Warn,
                "v1 responses stream SSE response invalid",
                metadata,
                &rate_limit_headers,
                &body,
            )
            .await;
        }
    }

    context.account_pool.release(&context.account_id).await;
}
