use std::time::Instant;

use serde_json::{Map, Value, json};

use crate::{
    dispatch::errors::{
        DispatchErrorMetadata, backend_transport_name, insert_dispatch_error_metadata,
        insert_upstream_diagnostics_metadata, upstream_error_http_status,
    },
    infra::time::elapsed_millis_i64,
    telemetry::{
        ops::types::OpsErrorLog,
        recorder::Recorder,
        usage::types::{
            UsageRecord, UsageRecordLevel, metadata_i64, metadata_service_tier,
            metadata_string as metadata_string_any,
        },
    },
    upstream::openai::{
        protocol::events::TokenUsage,
        protocol::responses::{CodexResponsesRequest, ResponsesSseFailure},
        transport::{
            CodexBackendTransport, CodexClientError, CodexResponseMetadata,
            CodexUpstreamDiagnostics, WebSocketPoolDecision,
        },
    },
};

const RESPONSE_EVENT_KIND: &str = "v1.response";
const RESPONSES_API_KIND: &str = "responses";

use crate::dispatch::{
    errors::{
        ResponseDispatchError, dispatch_error_metadata, enrich_response_dispatch_error_metadata,
    },
    failure::sse::stream_failure_metadata,
    lifecycle::trace::{ResponseDispatchAttempt, ResponseDispatchTrace},
};

use super::StreamContext;

pub(super) struct ResponseUpstreamErrorEventRecord<'a> {
    pub(super) recorder: &'a Recorder,
    pub(super) request_id: &'a str,
    pub(super) account_id: &'a str,
    pub(super) account_email: Option<&'a str>,
    pub(super) route: &'a str,
    pub(super) model: &'a str,
    pub(super) started_at: Instant,
    pub(super) stream: bool,
    pub(super) transport: CodexBackendTransport,
    pub(super) request: &'a CodexResponsesRequest,
    pub(super) error: &'a CodexClientError,
    pub(super) trace: &'a ResponseDispatchTrace,
    pub(super) attempt: Option<&'a ResponseDispatchAttempt>,
}

pub(super) struct ResponseStreamFailureEventRecord<'a> {
    pub(super) recorder: &'a Recorder,
    pub(super) request_id: &'a str,
    pub(super) account_id: &'a str,
    pub(super) route: &'a str,
    pub(super) model: &'a str,
    pub(super) requested_model: &'a str,
    pub(super) started_at: Instant,
    pub(super) transport: CodexBackendTransport,
    pub(super) request: &'a CodexResponsesRequest,
    pub(super) failure: &'a ResponsesSseFailure,
    pub(super) status_code: i64,
    pub(super) diagnostics: &'a CodexUpstreamDiagnostics,
    pub(super) rate_limit_headers: &'a [(String, String)],
    pub(super) prefetched: &'a [u8],
    pub(super) trace: &'a ResponseDispatchTrace,
    pub(super) attempt: &'a ResponseDispatchAttempt,
}

pub(super) struct ResponseDispatchErrorEventRecord<'a> {
    pub(super) recorder: &'a Recorder,
    pub(super) request_id: &'a str,
    pub(super) client_api_key_id: Option<&'a str>,
    pub(super) account_id: Option<&'a str>,
    pub(super) route: &'a str,
    pub(super) model: &'a str,
    pub(super) started_at: Instant,
    pub(super) stream: bool,
    pub(super) compact: bool,
    pub(super) transport: Option<&'a str>,
    pub(super) error: &'a ResponseDispatchError,
}

pub(super) struct ResponseUsageEventRecord<'a> {
    pub(super) recorder: &'a Recorder,
    pub(super) request_id: &'a str,
    pub(super) client_api_key_id: Option<&'a str>,
    pub(super) account_id: &'a str,
    pub(super) route: &'a str,
    pub(super) model: &'a str,
    pub(super) requested_model: Option<&'a str>,
    pub(super) client_ip: Option<&'a str>,
    pub(super) client_user_agent: Option<&'a str>,
    pub(super) reasoning_effort: Option<&'a str>,
    pub(super) service_tier: Option<&'a str>,
    pub(super) started_at: Instant,
    pub(super) status_code: i64,
    pub(super) message: &'a str,
    pub(super) usage: Option<TokenUsage>,
    pub(super) metadata: Value,
    pub(super) rate_limit_headers: &'a [(String, String)],
}

pub(super) struct LiveResponseStreamEventRecord<'a> {
    pub(super) context: &'a StreamContext<'a>,
    pub(super) status_code: i64,
    pub(super) level: UsageRecordLevel,
    pub(super) message: &'a str,
    pub(super) usage: Option<TokenUsage>,
    pub(super) metadata: Value,
    pub(super) rate_limit_headers: &'a [(String, String)],
    pub(super) body: &'a str,
}

struct DispatchErrorLogRecord<'a> {
    recorder: &'a Recorder,
    request_id: &'a str,
    client_api_key_id: Option<&'a str>,
    provider: Option<&'a str>,
    account_id: Option<&'a str>,
    route: &'a str,
    model: &'a str,
    started_at: Instant,
    status_code: i64,
    message: &'a str,
    metadata: Value,
}

pub(super) async fn record_response_event(record: ResponseUsageEventRecord<'_>) {
    let mut metadata = record.metadata;
    enrich_event_route_metadata(&mut metadata, record.route);

    let mut event = UsageRecord::new(
        RESPONSE_EVENT_KIND,
        record.message,
        record.account_id,
        record.model,
        record.status_code,
    );
    event.request_id = Some(record.request_id.to_string());
    event.client_api_key_id = record.client_api_key_id.map(ToString::to_string);
    event.route = Some(record.route.to_string());
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    event.requested_model = normalized_string(record.requested_model.or(Some(record.model)));
    event.upstream_model = normalized_string(Some(record.model));
    event.service_tier = normalized_string(record.service_tier);
    enrich_usage_record_identity(
        &mut metadata,
        record.requested_model,
        record.model,
        record.client_ip,
        record.client_user_agent,
        record.reasoning_effort,
        record.service_tier,
    );
    if !record.rate_limit_headers.is_empty()
        && let Some(object) = metadata.as_object_mut()
    {
        object.insert(
            "rateLimitHeaders".to_string(),
            serde_json::json!(record.rate_limit_headers),
        );
    }
    lift_success_fact_fields(&mut event, &mut metadata, record.usage);
    event.metadata = metadata;

    if let Err(error) = record.recorder.record_usage(event).await {
        tracing::error!(account_id = %record.account_id, error = %error, "Failed to record response event");
    }
}

async fn record_dispatch_error_event(record: DispatchErrorLogRecord<'_>) {
    let mut metadata = record.metadata;
    enrich_event_route_metadata(&mut metadata, record.route);

    let mut event = OpsErrorLog::new(RESPONSE_EVENT_KIND, record.message);
    event.request_id = Some(record.request_id.to_string());
    event.client_api_key_id = record.client_api_key_id.map(ToString::to_string);
    event.provider = record.provider.map(ToString::to_string);
    event.account_id = record.account_id.map(ToString::to_string);
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(record.status_code);
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    event.metadata = metadata;
    lift_error_fact_fields(&mut event);

    if let Err(error) = record.recorder.record_error(event).await {
        tracing::error!(
            account_id = record.account_id.unwrap_or(""),
            error = %error,
            "Failed to record dispatch error event"
        );
    }
}

pub(super) async fn record_response_dispatch_error_event(
    record: ResponseDispatchErrorEventRecord<'_>,
) {
    let mut metadata = dispatch_error_metadata(
        record.error,
        record.stream,
        record.compact,
        record.transport,
    );
    enrich_response_dispatch_error_metadata(&mut metadata, record.error);
    if let Some(object) = metadata.as_object_mut() {
        insert_response_status_metadata_object(
            object,
            i64::from(record.error.http_status_code()),
            dispatch_error_client_status_code(record.error),
            dispatch_error_upstream_status_code(record.error),
        );
    }
    record_dispatch_error_event(DispatchErrorLogRecord {
        recorder: record.recorder,
        request_id: record.request_id,
        client_api_key_id: record.client_api_key_id,
        provider: Some("openai"),
        account_id: record.account_id,
        route: record.route,
        model: record.model,
        started_at: record.started_at,
        status_code: i64::from(record.error.http_status_code()),
        message: "v1 responses dispatch failed",
        metadata,
    })
    .await;
}

pub(super) async fn record_response_upstream_error_event(
    record: ResponseUpstreamErrorEventRecord<'_>,
) {
    let event_status_code = i64::from(upstream_error_http_status(record.error));
    let mut metadata = dispatch_error_metadata(
        record.error,
        record.stream,
        false,
        Some(backend_transport_name(record.transport)),
    );
    enrich_event_route_metadata(&mut metadata, record.route);
    let mut event = OpsErrorLog::new("v1.response", "v1 responses upstream request failed");
    event.request_id = Some(record.request_id.to_string());
    event.client_api_key_id = record.request.client_api_key_id.clone();
    event.provider = Some("openai".to_string());
    event.account_id = Some(record.account_id.to_string());
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(event_status_code);
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    if let Some(object) = metadata.as_object_mut() {
        insert_dispatch_error_metadata(object, DispatchErrorMetadata::upstream(record.error));
        insert_response_status_metadata_object(
            object,
            event_status_code,
            upstream_failure_client_status_code(event_status_code),
            Some(event_status_code),
        );
        insert_response_request_summary_object(object, record.request, record.transport);
        insert_response_trace_metadata_object(object, record.trace.attempts(), record.attempt);
        if let Some(account_email) = record
            .account_email
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            object.insert(
                "accountEmail".to_string(),
                Value::String(account_email.to_string()),
            );
        }
    }
    enrich_usage_record_identity(
        &mut metadata,
        Some(record.request.model()),
        record.model,
        record.request.client_ip.as_deref(),
        record.request.client_user_agent.as_deref(),
        reasoning_effort_from_request(record.request),
        record.request.service_tier(),
    );
    enrich_response_request_semantics(&mut metadata, record.request);
    event.metadata = metadata;
    lift_error_fact_fields(&mut event);
    if let Err(error) = record.recorder.record_error(event).await {
        tracing::error!(account_id = %record.account_id, error = %error, "Failed to record upstream error event");
    }
}

pub(super) async fn record_prefetched_response_stream_failure_event(
    record: ResponseStreamFailureEventRecord<'_>,
) {
    let event_status_code = record.status_code;
    let mut metadata = stream_failure_metadata(record.failure, None);
    if let Some(object) = metadata.as_object_mut() {
        if record.transport == CodexBackendTransport::WebSocket {
            object.insert(
                "transport".to_string(),
                Value::String("websocket".to_string()),
            );
        }
        insert_response_status_metadata_object(
            object,
            event_status_code,
            200,
            diagnostics_status_code(record.diagnostics),
        );
        insert_response_request_summary_object(object, record.request, record.transport);
        if record.recorder.captures_body() {
            object.insert("requestBody".to_string(), json!(record.request));
            object.insert(
                "responseBody".to_string(),
                Value::String(String::from_utf8_lossy(record.prefetched).to_string()),
            );
        }
        insert_upstream_diagnostics_metadata(object, record.diagnostics);
        insert_response_trace_metadata_object(
            object,
            record.trace.attempts(),
            Some(record.attempt),
        );
    }
    enrich_event_route_metadata(&mut metadata, record.route);
    enrich_usage_record_identity(
        &mut metadata,
        Some(record.requested_model),
        record.model,
        record.request.client_ip.as_deref(),
        record.request.client_user_agent.as_deref(),
        reasoning_effort_from_request(record.request),
        record.request.service_tier(),
    );
    enrich_response_request_semantics(&mut metadata, record.request);
    let mut event = OpsErrorLog::new(RESPONSE_EVENT_KIND, "v1 responses stream failed");
    event.request_id = Some(record.request_id.to_string());
    event.client_api_key_id = record.request.client_api_key_id.clone();
    event.provider = Some("openai".to_string());
    event.account_id = Some(record.account_id.to_string());
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(event_status_code);
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    if !record.rate_limit_headers.is_empty()
        && let Some(object) = metadata.as_object_mut()
    {
        object.insert(
            "rateLimitHeaders".to_string(),
            serde_json::json!(record.rate_limit_headers),
        );
    }
    event.metadata = metadata;
    lift_error_fact_fields(&mut event);
    if let Err(error) = record.recorder.record_error(event).await {
        tracing::error!(account_id = %record.account_id, error = %error, "Failed to record prefetched stream error event");
    }
}

fn lift_error_fact_fields(event: &mut OpsErrorLog) {
    event.client_status_code = event
        .client_status_code
        .or_else(|| metadata_i64(&event.metadata, &["clientStatusCode", "client_status_code"]));
    event.upstream_status_code = event
        .upstream_status_code
        .or_else(|| metadata_i64(&event.metadata, &["upstreamStatusCode", "upstreamStatus"]));
    event.transport = event
        .transport
        .take()
        .or_else(|| metadata_string_any(&event.metadata, &["transport"]));
    event.attempt_index = event
        .attempt_index
        .or_else(|| metadata_i64(&event.metadata, &["attemptIndex", "attempt_index"]));
    event.failure_class = event
        .failure_class
        .take()
        .or_else(|| metadata_string_any(&event.metadata, &["failureClass", "failure_class"]));
    event.response_id = event
        .response_id
        .take()
        .or_else(|| metadata_string_any(&event.metadata, &["responseId", "response_id"]));
    event.upstream_request_id = event.upstream_request_id.take().or_else(|| {
        metadata_string_any(
            &event.metadata,
            &[
                "upstreamRequestId",
                "upstream_request_id",
                "openaiRequestId",
            ],
        )
    });
    event.service_tier = event
        .service_tier
        .take()
        .or_else(|| metadata_service_tier(&event.metadata).map(ToString::to_string));

    let Some(metadata) = event.metadata.as_object_mut() else {
        return;
    };
    for key in [
        "clientStatusCode",
        "client_status_code",
        "upstreamStatusCode",
        "upstreamStatus",
        "transport",
        "attemptIndex",
        "attempt_index",
        "failureClass",
        "failure_class",
        "responseId",
        "response_id",
        "upstreamRequestId",
        "upstream_request_id",
        "openaiRequestId",
        "serviceTier",
    ] {
        metadata.remove(key);
    }
}

fn lift_success_fact_fields(
    event: &mut UsageRecord,
    metadata: &mut Value,
    usage: Option<TokenUsage>,
) {
    event.transport = metadata_string(metadata, "transport");
    event.attempt_index = metadata_nonnegative_i64(metadata, "attemptIndex");
    event.response_id = metadata_string(metadata, "responseId");
    event.upstream_request_id = metadata_string(metadata, "upstreamRequestId")
        .or_else(|| metadata_string(metadata, "openaiRequestId"));
    event.first_token_ms = metadata_nonnegative_i64(metadata, "firstTokenMs");
    if let Some(usage) = usage {
        event.input_tokens = i64::try_from(usage.input_tokens).ok();
        event.output_tokens = i64::try_from(usage.output_tokens).ok();
        event.cached_tokens = i64::try_from(usage.cached_tokens).ok();
        event.cache_write_tokens = i64::try_from(usage.cache_write_tokens).ok();
        event.reasoning_tokens = i64::try_from(usage.reasoning_tokens).ok();
    }

    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    for key in [
        "usage",
        "route",
        "apiKind",
        "requestedModel",
        "upstreamModel",
        "serviceTier",
        "statusCode",
        "transport",
        "attemptIndex",
        "responseId",
        "upstreamRequestId",
        "openaiRequestId",
        "firstTokenMs",
    ] {
        object.remove(key);
    }
}

fn metadata_string(metadata: &Value, field: &str) -> Option<String> {
    metadata
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn metadata_nonnegative_i64(metadata: &Value, field: &str) -> Option<i64> {
    metadata
        .get(field)
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
}

fn normalized_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn enrich_event_route_metadata(metadata: &mut Value, route: &str) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object
        .entry("route".to_string())
        .or_insert_with(|| Value::String(route.to_string()));
    object
        .entry("apiKind".to_string())
        .or_insert_with(|| Value::String(RESPONSES_API_KIND.to_string()));
}

fn enrich_usage_record_identity(
    metadata: &mut Value,
    requested_model: Option<&str>,
    upstream_model: &str,
    client_ip: Option<&str>,
    client_user_agent: Option<&str>,
    reasoning_effort: Option<&str>,
    service_tier: Option<&str>,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    let upstream_model = upstream_model.trim();
    let requested_model = requested_model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(upstream_model);
    object.insert(
        "requestedModel".to_string(),
        Value::String(requested_model.to_string()),
    );
    object.insert(
        "upstreamModel".to_string(),
        Value::String(upstream_model.to_string()),
    );
    insert_trimmed_string(object, "clientIp", client_ip);
    insert_trimmed_string(object, "userAgent", client_user_agent);
    insert_trimmed_string(object, "reasoningEffort", reasoning_effort);
    insert_trimmed_string(object, "serviceTier", service_tier);
}

pub(super) fn enrich_response_request_semantics(
    metadata: &mut Value,
    request: &CodexResponsesRequest,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    let semantics = request.semantics();
    object.insert("compact".to_string(), Value::Bool(semantics.compact));
    insert_trimmed_string(object, "requestKind", semantics.request_kind.as_deref());
    insert_trimmed_string(object, "subagentKind", semantics.subagent_kind.as_deref());
    insert_trimmed_string(object, "reasoningPreset", semantics.reasoning_preset);
}

pub(super) fn reasoning_effort_from_request(request: &CodexResponsesRequest) -> Option<&str> {
    request
        .reasoning()?
        .get("effort")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn insert_trimmed_string(object: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn ensure_stream_metadata_flag(metadata: &mut Value) {
    let Some(object) = metadata.as_object_mut() else {
        *metadata = serde_json::json!({ "stream": true });
        return;
    };
    object
        .entry("stream".to_string())
        .or_insert(Value::Bool(true));
}

fn insert_optional_i64(object: &mut serde_json::Map<String, Value>, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        object
            .entry(key.to_string())
            .or_insert_with(|| Value::Number(value.into()));
    }
}

fn enrich_live_response_stream_metadata(
    context: &StreamContext<'_>,
    rate_limit_headers: &[(String, String)],
    metadata: &mut Value,
    status_code: i64,
    body: &str,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object
        .entry("transport".to_string())
        .or_insert_with(|| Value::String(backend_transport_name(context.transport).to_string()));
    if !rate_limit_headers.is_empty() {
        object
            .entry("rateLimitHeaders".to_string())
            .or_insert_with(|| serde_json::json!(rate_limit_headers));
    }
    if let Some(decision) = context.websocket_pool_decision {
        object
            .entry("websocketPool".to_string())
            .or_insert_with(|| decision.metadata_value());
    }
    if let Some(decision) = context.transport_metrics.decision {
        object
            .entry("transportDecision".to_string())
            .or_insert_with(|| Value::String(decision.as_str().to_string()));
    }
    insert_optional_i64(
        object,
        "wsConnectMs",
        context.transport_metrics.ws_connect_ms,
    );
    insert_optional_i64(
        object,
        "transportDecisionWaitMs",
        context.transport_metrics.transport_decision_wait_ms,
    );
    insert_optional_i64(
        object,
        "upstreamHeadersMs",
        context.transport_metrics.upstream_headers_ms,
    );
    insert_optional_i64(
        object,
        "firstEventMs",
        context.transport_metrics.first_event_ms,
    );
    insert_optional_i64(
        object,
        "openaiProcessingMs",
        openai_processing_ms(context.response_metadata),
    );
    if let Some(http_version) = context.transport_metrics.http_version.as_deref() {
        object
            .entry("httpVersion".to_string())
            .or_insert_with(|| Value::String(http_version.to_string()));
    }
    insert_response_status_metadata_object(
        object,
        status_code,
        200,
        diagnostics_status_code(context.diagnostics),
    );
    insert_response_request_summary_object(object, context.request, context.transport);
    insert_upstream_diagnostics_metadata(object, context.diagnostics);
    insert_response_trace_metadata_object(object, context.attempts, Some(context.attempt));
    if context.recorder.captures_body() {
        object
            .entry("requestBody".to_string())
            .or_insert_with(|| serde_json::json!(context.request));
        object
            .entry("responseBody".to_string())
            .or_insert_with(|| Value::String(body.to_string()));
    }
}

pub(super) fn insert_response_upstream_diagnostics(
    metadata: &mut Value,
    diagnostics: &CodexUpstreamDiagnostics,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    insert_upstream_diagnostics_metadata(object, diagnostics);
}

pub(super) fn insert_response_status_metadata(
    metadata: &mut Value,
    event_status_code: i64,
    client_status_code: i64,
    upstream_status_code: Option<i64>,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    insert_response_status_metadata_object(
        object,
        event_status_code,
        client_status_code,
        upstream_status_code,
    );
}

pub(super) fn insert_response_trace_metadata(
    metadata: &mut Value,
    trace: &ResponseDispatchTrace,
    current_attempt: Option<&ResponseDispatchAttempt>,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    insert_response_trace_metadata_object(object, trace.attempts(), current_attempt);
}

fn insert_response_attempt_metadata_object(
    object: &mut serde_json::Map<String, Value>,
    attempt: Option<&ResponseDispatchAttempt>,
) {
    let Some(attempt) = attempt else {
        return;
    };
    object.insert("attemptIndex".to_string(), json!(attempt.index()));
    object.insert(
        "attemptAccountId".to_string(),
        Value::String(attempt.account_id().to_string()),
    );
}

fn insert_response_trace_metadata_object(
    object: &mut Map<String, Value>,
    attempts: &[ResponseDispatchAttempt],
    current_attempt: Option<&ResponseDispatchAttempt>,
) {
    insert_response_attempt_metadata_object(object, current_attempt);
    object.insert("attemptCount".to_string(), json!(attempts.len()));
    object.insert(
        "attempts".to_string(),
        Value::Array(
            attempts
                .iter()
                .map(|attempt| {
                    json!({
                        "index": attempt.index(),
                        "accountId": attempt.account_id(),
                    })
                })
                .collect(),
        ),
    );
}

fn insert_response_status_metadata_object(
    object: &mut Map<String, Value>,
    event_status_code: i64,
    client_status_code: i64,
    upstream_status_code: Option<i64>,
) {
    object.insert("eventStatusCode".to_string(), json!(event_status_code));
    object.insert("clientStatusCode".to_string(), json!(client_status_code));
    if let Some(upstream_status_code) = upstream_status_code {
        object.insert(
            "upstreamStatusCode".to_string(),
            json!(upstream_status_code),
        );
    }
}

fn insert_response_request_summary_object(
    object: &mut Map<String, Value>,
    request: &CodexResponsesRequest,
    transport: CodexBackendTransport,
) {
    object.insert(
        "requestSummary".to_string(),
        response_request_summary(request, transport),
    );
}

fn response_request_summary(
    request: &CodexResponsesRequest,
    transport: CodexBackendTransport,
) -> Value {
    let body = request.body();
    let input = body.get("input");
    let tools = body.get("tools");
    let semantics = request.semantics();
    json!({
        "model": request.model(),
        "stream": request.stream(),
        "store": request.store(),
        "compact": semantics.compact,
        "requestKind": semantics.request_kind,
        "subagentKind": semantics.subagent_kind,
        "reasoningPreset": semantics.reasoning_preset,
        "transport": backend_transport_name(transport),
        "inputType": json_value_kind(input),
        "inputItemsCount": input.and_then(Value::as_array).map(Vec::len),
        "toolsType": json_value_kind(tools),
        "toolsCount": tools.and_then(Value::as_array).map(Vec::len),
        "topLevelFields": body.keys().cloned().collect::<Vec<_>>(),
        "previousResponseIdPresent": request.previous_response_id().is_some(),
        "serviceTier": request.service_tier(),
        "localTransport": {
            "useWebsocket": request.use_websocket,
            "forceHttpSse": request.force_http_sse,
        },
    })
}

fn json_value_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn diagnostics_status_code(diagnostics: &CodexUpstreamDiagnostics) -> Option<i64> {
    diagnostics.status_code.map(i64::from)
}

fn dispatch_error_upstream_status_code(error: &ResponseDispatchError) -> Option<i64> {
    match error {
        ResponseDispatchError::Upstream(error) => {
            Some(i64::from(upstream_error_http_status(error)))
        }
        _ => None,
    }
}

fn dispatch_error_client_status_code(error: &ResponseDispatchError) -> i64 {
    i64::from(error.client_http_status_code())
}

fn upstream_failure_client_status_code(upstream_status: i64) -> i64 {
    i64::from(crate::dispatch::errors::client_upstream_http_status_code(
        u16::try_from(upstream_status).unwrap_or(502),
    ))
}

pub(super) async fn record_live_response_stream_event(record: LiveResponseStreamEventRecord<'_>) {
    let LiveResponseStreamEventRecord {
        context,
        status_code,
        level,
        message,
        usage,
        mut metadata,
        rate_limit_headers,
        body,
    } = record;
    let effective_model = context
        .response_metadata
        .effective_model
        .as_deref()
        .unwrap_or(context.display_model);
    ensure_stream_metadata_flag(&mut metadata);
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "effectiveModel".to_string(),
            Value::String(effective_model.to_string()),
        );
        object.insert(
            "modelsEtag".to_string(),
            context
                .response_metadata
                .models_etag
                .as_ref()
                .map_or(Value::Null, |etag| Value::String(etag.clone())),
        );
        object.insert(
            "reasoningIncluded".to_string(),
            Value::Bool(context.response_metadata.reasoning_included),
        );
    }
    enrich_event_route_metadata(&mut metadata, context.route);
    enrich_live_response_stream_metadata(
        context,
        rate_limit_headers,
        &mut metadata,
        status_code,
        body,
    );
    if !is_success_usage_event(level, status_code) {
        ensure_stream_failure_class(&mut metadata);
    }
    log_live_response_stream_finalized(context, status_code, level, message, &metadata);
    enrich_usage_record_identity(
        &mut metadata,
        Some(context.requested_model),
        effective_model,
        context.request.client_ip.as_deref(),
        context.request.client_user_agent.as_deref(),
        reasoning_effort_from_request(context.request),
        context.request.service_tier(),
    );
    enrich_response_request_semantics(&mut metadata, context.request);

    if is_success_usage_event(level, status_code) {
        record_response_event(ResponseUsageEventRecord {
            recorder: context.recorder,
            request_id: context.request_id,
            client_api_key_id: context.request.client_api_key_id.as_deref(),
            account_id: context.account_id,
            route: context.route,
            model: effective_model,
            requested_model: Some(context.requested_model),
            client_ip: context.request.client_ip.as_deref(),
            client_user_agent: context.request.client_user_agent.as_deref(),
            reasoning_effort: reasoning_effort_from_request(context.request),
            service_tier: context.request.service_tier(),
            started_at: context.started_at,
            status_code,
            message,
            usage,
            metadata,
            rate_limit_headers,
        })
        .await;
    } else {
        let mut event = OpsErrorLog::new(RESPONSE_EVENT_KIND, message);
        event.request_id = Some(context.request_id.to_string());
        event.client_api_key_id = context.request.client_api_key_id.clone();
        event.provider = Some("openai".to_string());
        event.account_id = Some(context.account_id.to_string());
        event.route = Some(context.route.to_string());
        event.model = Some(effective_model.to_string());
        event.status_code = Some(status_code);
        event.latency_ms = Some(elapsed_millis_i64(context.started_at));
        event.metadata = metadata;
        lift_error_fact_fields(&mut event);
        if let Err(error) = context.recorder.record_error(event).await {
            tracing::error!(account_id = %context.account_id, error = %error, "Failed to record live response stream error event");
        }
    }
}

fn is_success_usage_event(level: UsageRecordLevel, status_code: i64) -> bool {
    level != UsageRecordLevel::Error && status_code < 400
}

fn ensure_stream_failure_class(metadata: &mut Value) {
    let failure_class = ["failureClass", "upstreamCode", "terminal"]
        .into_iter()
        .find_map(|field| metadata_string_field(metadata, field))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let (Some(failure_class), Some(object)) = (failure_class, metadata.as_object_mut()) else {
        return;
    };
    object.insert("failureClass".to_string(), Value::String(failure_class));
}

fn log_live_response_stream_finalized(
    context: &StreamContext<'_>,
    status_code: i64,
    level: UsageRecordLevel,
    message: &str,
    metadata: &Value,
) {
    let response_id = metadata_string_field(metadata, "responseId").map(ToString::to_string);
    let first_token_ms = metadata.get("firstTokenMs").and_then(Value::as_i64);
    let websocket_pool_kind = context
        .websocket_pool_decision
        .map(WebSocketPoolDecision::kind);
    let completed = metadata.get("completed").and_then(Value::as_bool);
    let failed = metadata.get("failed").and_then(Value::as_bool);
    let upstream_code = metadata_string_field(metadata, "upstreamCode");
    let failure_class = metadata_string_field(metadata, "failureClass");
    let failure_source = metadata_string_field(metadata, "failureSource");
    let failure_detail = metadata_string_field(metadata, "failureDetail");

    macro_rules! emit_stream_finalized_log {
        ($level:expr) => {
            tracing::event!(
                $level,
                account_id = %context.account_id,
                request_id = %context.request_id,
                route = %context.route,
                model = %context.display_model,
                status_code,
                usage_level = ?level,
                event_message = %message,
                transport = %backend_transport_name(context.transport),
                websocket_pool_kind = ?websocket_pool_kind,
                response_id = response_id.as_deref().unwrap_or(""),
                first_token_ms = ?first_token_ms,
                latency_ms = elapsed_millis_i64(context.started_at),
                completed = ?completed,
                failed = ?failed,
                upstream_code = ?upstream_code,
                failure_class = ?failure_class,
                failure_source = ?failure_source,
                failure_detail = ?failure_detail,
                "Live response stream finalized"
            );
        };
    }

    match level {
        UsageRecordLevel::Debug => {
            emit_stream_finalized_log!(tracing::Level::DEBUG);
        }
        UsageRecordLevel::Info => {
            emit_stream_finalized_log!(tracing::Level::INFO);
        }
        UsageRecordLevel::Warn => {
            emit_stream_finalized_log!(tracing::Level::WARN);
        }
        UsageRecordLevel::Error => {
            emit_stream_finalized_log!(tracing::Level::ERROR);
        }
    }
}

fn metadata_string_field<'a>(metadata: &'a Value, field: &str) -> Option<&'a str> {
    metadata.get(field).and_then(Value::as_str)
}

pub(super) fn insert_output_timing_ms(
    metadata: &mut Value,
    first_token_ms: Option<i64>,
    first_reasoning_ms: Option<i64>,
    first_text_ms: Option<i64>,
) {
    if let Some(object) = metadata.as_object_mut() {
        insert_optional_i64(object, "firstTokenMs", first_token_ms);
        insert_optional_i64(object, "firstReasoningMs", first_reasoning_ms);
        insert_optional_i64(object, "firstTextMs", first_text_ms);
    }
}

pub(super) fn insert_openai_processing_ms(
    metadata: &mut Value,
    response_metadata: &CodexResponseMetadata,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    insert_optional_i64(
        object,
        "openaiProcessingMs",
        openai_processing_ms(response_metadata),
    );
}

fn openai_processing_ms(response_metadata: &CodexResponseMetadata) -> Option<i64> {
    response_metadata
        .client_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("openai-processing-ms"))
        .and_then(|(_, value)| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
}

pub(super) fn insert_websocket_pool_decision(
    metadata: &mut Value,
    decision: Option<WebSocketPoolDecision>,
) {
    let Some(decision) = decision else {
        return;
    };
    if let Some(object) = metadata.as_object_mut() {
        object.insert("websocketPool".to_string(), decision.metadata_value());
    }
}
