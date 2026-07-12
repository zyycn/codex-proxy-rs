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
        recorder::{
            DispatchErrorLogRecord, Recorder, enrich_event_route_metadata,
            enrich_response_request_semantics, enrich_usage_record_identity,
            event_kind as response_event_kind, reasoning_effort_from_request,
            record_dispatch_error_event,
        },
        usage::types::UsageRecordLevel,
    },
    upstream::openai::{
        protocol::responses::{CodexResponsesRequest, ResponsesSseFailure},
        transport::{
            CodexBackendTransport, CodexClientError, CodexUpstreamDiagnostics,
            WebSocketPoolDecision,
        },
    },
};

use super::{
    errors::{
        ResponseDispatchError, dispatch_error_metadata, enrich_response_dispatch_error_metadata,
    },
    stream::{
        live::{LiveResponseStreamContext, latest_response_id},
        sse_failure::{status_code_for_stream_failure, stream_failure_metadata},
        trace::{ResponseDispatchAttempt, ResponseDispatchTrace},
    },
};

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

pub(super) struct ResponseDispatchErrorDetails<'a> {
    pub(super) client_api_key_id: Option<&'a str>,
    pub(super) account_id: Option<&'a str>,
    pub(super) stream: bool,
    pub(super) compact: bool,
    pub(super) transport: Option<&'a str>,
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
    if let Err(error) = record.recorder.record_error(event).await {
        tracing::error!(account_id = %record.account_id, error = %error, "failed to record upstream error event");
    }
}

pub(super) async fn record_prefetched_response_stream_failure_event(
    record: ResponseStreamFailureEventRecord<'_>,
) {
    let event_status_code = status_code_for_stream_failure(record.failure);
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
        object.insert("requestBody".to_string(), json!(record.request));
        object.insert(
            "responseBody".to_string(),
            Value::String(String::from_utf8_lossy(record.prefetched).to_string()),
        );
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
    let mut event = OpsErrorLog::new(
        response_event_kind(record.route),
        "v1 responses stream failed",
    );
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
    if let Err(error) = record.recorder.record_error(event).await {
        tracing::error!(account_id = %record.account_id, error = %error, "failed to record prefetched stream error event");
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

fn enrich_live_response_stream_metadata(
    context: &LiveResponseStreamContext,
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
    insert_response_status_metadata_object(
        object,
        status_code,
        200,
        diagnostics_status_code(&context.diagnostics),
    );
    insert_response_request_summary_object(object, &context.request, context.transport);
    insert_upstream_diagnostics_metadata(object, &context.diagnostics);
    insert_response_trace_metadata_object(object, &context.attempts, Some(&context.attempt));
    object
        .entry("requestBody".to_string())
        .or_insert_with(|| serde_json::json!(context.request));
    object
        .entry("responseBody".to_string())
        .or_insert_with(|| Value::String(body.to_string()));
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

pub(super) async fn record_live_response_stream_event(
    context: &LiveResponseStreamContext,
    status_code: i64,
    level: UsageRecordLevel,
    message: &str,
    mut metadata: Value,
    rate_limit_headers: &[(String, String)],
    body: &str,
) {
    let effective_model = context
        .response_metadata
        .effective_model
        .as_deref()
        .unwrap_or(&context.display_model);
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
    enrich_event_route_metadata(&mut metadata, &context.route);
    enrich_live_response_stream_metadata(
        context,
        rate_limit_headers,
        &mut metadata,
        status_code,
        body,
    );
    log_live_response_stream_finalized(context, status_code, level, message, &metadata, body);
    enrich_usage_record_identity(
        &mut metadata,
        Some(&context.requested_model),
        effective_model,
        context.client_ip.as_deref(),
        context.request.client_user_agent.as_deref(),
        reasoning_effort_from_request(&context.request),
        context.request.service_tier(),
    );
    enrich_response_request_semantics(&mut metadata, &context.request);

    if is_success_usage_event(level, status_code) {
        crate::telemetry::recorder::record_response_event(
            crate::telemetry::usage::types::ResponseUsageRecord {
                recorder: &context.recorder,
                request_id: &context.request_id,
                client_api_key_id: context.request.client_api_key_id.as_deref(),
                account_id: &context.account_id,
                route: &context.route,
                model: effective_model,
                requested_model: Some(&context.requested_model),
                client_ip: context.client_ip.as_deref(),
                client_user_agent: context.request.client_user_agent.as_deref(),
                reasoning_effort: reasoning_effort_from_request(&context.request),
                service_tier: context.request.service_tier(),
                started_at: context.started_at,
                status_code,
                message,
                metadata,
                rate_limit_headers,
            },
        )
        .await;
    } else {
        let mut event = OpsErrorLog::new(response_event_kind(&context.route), message);
        event.request_id = Some(context.request_id.clone());
        event.client_api_key_id = context.request.client_api_key_id.clone();
        event.provider = Some("openai".to_string());
        event.account_id = Some(context.account_id.clone());
        event.route = Some(context.route.clone());
        event.model = Some(effective_model.to_string());
        event.status_code = Some(status_code);
        event.latency_ms = Some(elapsed_millis_i64(context.started_at));
        event.metadata = metadata;
        if let Err(error) = context.recorder.record_error(event).await {
            tracing::error!(account_id = %context.account_id, error = %error, "failed to record live response stream error event");
        }
    }
}

fn is_success_usage_event(level: UsageRecordLevel, status_code: i64) -> bool {
    level != UsageRecordLevel::Error && status_code < 400
}

fn log_live_response_stream_finalized(
    context: &LiveResponseStreamContext,
    status_code: i64,
    level: UsageRecordLevel,
    message: &str,
    metadata: &Value,
    body: &str,
) {
    let response_id = metadata_string_field(metadata, "responseId")
        .map(ToString::to_string)
        .or_else(|| latest_response_id(body));
    let first_token_ms = metadata.get("firstTokenMs").and_then(Value::as_i64);
    let websocket_pool_kind = context
        .websocket_pool_decision
        .map(WebSocketPoolDecision::kind);
    let websocket_pool_reason = context
        .websocket_pool_decision
        .and_then(WebSocketPoolDecision::reason);
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
                websocket_pool_reason = ?websocket_pool_reason,
                response_id = response_id.as_deref().unwrap_or(""),
                first_token_ms = ?first_token_ms,
                latency_ms = elapsed_millis_i64(context.started_at),
                completed = ?completed,
                failed = ?failed,
                upstream_code = ?upstream_code,
                failure_class = ?failure_class,
                failure_source = ?failure_source,
                failure_detail = ?failure_detail,
                "live response stream finalized"
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

pub(super) async fn live_response_rate_limit_headers(
    context: &LiveResponseStreamContext,
) -> Vec<(String, String)> {
    let mut headers = context.rate_limit_headers.clone();
    if let Some(updates) = &context.rate_limit_header_updates {
        headers.extend(updates.lock().await.iter().cloned());
    }
    headers
}

pub(super) async fn live_response_turn_state(
    context: &LiveResponseStreamContext,
) -> Option<String> {
    if let Some(update) = &context.turn_state_update {
        return update.lock().await.clone();
    }
    context.turn_state.clone()
}

pub(super) fn insert_first_token_ms(metadata: &mut Value, first_token_ms: Option<i64>) {
    let Some(first_token_ms) = first_token_ms else {
        return;
    };
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "firstTokenMs".to_string(),
            Value::Number(first_token_ms.into()),
        );
    }
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
