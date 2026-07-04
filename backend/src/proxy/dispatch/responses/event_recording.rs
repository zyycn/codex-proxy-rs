use std::time::Instant;

use serde_json::{json, Value};

use crate::{
    admin::monitoring::{
        usage_record_model::{ResponseUsageRecord, UsageRecord, UsageRecordLevel},
        usage_record_service::AdminUsageRecordService,
    },
    infra::time::elapsed_millis_i64,
    proxy::dispatch::{
        errors::{backend_transport_name, upstream_error_http_status},
        usage_events::{
            enrich_event_route_metadata, enrich_usage_record_identity,
            event_kind as response_event_kind, reasoning_effort_from_request,
            record_dispatch_error_event, record_response_event, DispatchErrorUsageRecord,
        },
    },
    upstream::{
        protocol::responses::{CodexResponsesRequest, ResponsesSseFailure},
        transport::{CodexBackendTransport, CodexClientError, WebSocketPoolDecision},
    },
};

use super::{
    errors::{
        dispatch_error_metadata, enrich_response_dispatch_error_metadata, ResponseDispatchError,
    },
    sse_failure::{status_code_for_stream_failure, stream_failure_metadata},
    stream_lifecycle::{latest_response_id, LiveResponseStreamContext},
};

pub(super) struct ResponseUpstreamErrorEventRecord<'a> {
    pub(super) usage_records: &'a AdminUsageRecordService,
    pub(super) request_id: &'a str,
    pub(super) account_id: &'a str,
    pub(super) account_email: Option<&'a str>,
    pub(super) route: &'a str,
    pub(super) model: &'a str,
    pub(super) started_at: Instant,
    pub(super) stream: bool,
    pub(super) transport: CodexBackendTransport,
    pub(super) error: &'a CodexClientError,
}

pub(super) struct ResponseStreamFailureEventRecord<'a> {
    pub(super) usage_records: &'a AdminUsageRecordService,
    pub(super) request_id: &'a str,
    pub(super) account_id: &'a str,
    pub(super) route: &'a str,
    pub(super) model: &'a str,
    pub(super) requested_model: &'a str,
    pub(super) started_at: Instant,
    pub(super) transport: CodexBackendTransport,
    pub(super) request: &'a CodexResponsesRequest,
    pub(super) failure: &'a ResponsesSseFailure,
    pub(super) rate_limit_headers: &'a [(String, String)],
    pub(super) prefetched: &'a [u8],
}

pub(super) struct ResponseDispatchErrorEventRecord<'a> {
    pub(super) usage_records: &'a AdminUsageRecordService,
    pub(super) request_id: &'a str,
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
    record_dispatch_error_event(DispatchErrorUsageRecord {
        usage_records: record.usage_records,
        request_id: record.request_id,
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
    let mut metadata = dispatch_error_metadata(
        record.error,
        record.stream,
        false,
        Some(backend_transport_name(record.transport)),
    );
    enrich_event_route_metadata(&mut metadata, record.route);
    let mut event = UsageRecord::new(
        "v1.response",
        UsageRecordLevel::Error,
        "v1 responses upstream request failed",
    );
    event.request_id = Some(record.request_id.to_string());
    event.account_id = Some(record.account_id.to_string());
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(i64::from(upstream_error_http_status(record.error)));
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    if let Some(object) = metadata.as_object_mut() {
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
    event.metadata = metadata;
    if let Err(error) = record.usage_records.record(event).await {
        tracing::warn!(account_id = %record.account_id, error = %error, "failed to record upstream error event");
    }
}

pub(super) async fn record_prefetched_response_stream_failure_event(
    record: ResponseStreamFailureEventRecord<'_>,
) {
    let mut metadata = stream_failure_metadata(record.failure, None);
    if let Some(object) = metadata.as_object_mut() {
        if record.transport == CodexBackendTransport::WebSocket {
            object.insert(
                "transport".to_string(),
                Value::String("websocket".to_string()),
            );
        }
        object.insert("requestBody".to_string(), json!(record.request));
        object.insert(
            "responseBody".to_string(),
            Value::String(String::from_utf8_lossy(record.prefetched).to_string()),
        );
    }
    record_response_event(ResponseUsageRecord {
        usage_records: record.usage_records,
        request_id: record.request_id,
        account_id: record.account_id,
        route: record.route,
        model: record.model,
        requested_model: Some(record.requested_model),
        client_ip: record.request.client_ip.as_deref(),
        client_user_agent: record.request.client_user_agent.as_deref(),
        reasoning_effort: reasoning_effort_from_request(record.request),
        service_tier: record.request.service_tier(),
        started_at: record.started_at,
        status_code: status_code_for_stream_failure(record.failure),
        level: UsageRecordLevel::Error,
        message: "v1 responses stream failed",
        metadata,
        rate_limit_headers: record.rate_limit_headers,
    })
    .await;
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
    object
        .entry("requestBody".to_string())
        .or_insert_with(|| serde_json::json!(context.request));
    object
        .entry("responseBody".to_string())
        .or_insert_with(|| Value::String(body.to_string()));
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
    ensure_stream_metadata_flag(&mut metadata);
    enrich_event_route_metadata(&mut metadata, &context.route);
    enrich_live_response_stream_metadata(context, rate_limit_headers, &mut metadata, body);
    log_live_response_stream_finalized(context, status_code, level, message, &metadata, body);
    let mut event = UsageRecord::new(response_event_kind(&context.route), level, message);
    event.request_id = Some(context.request_id.clone());
    event.account_id = Some(context.account_id.clone());
    event.route = Some(context.route.clone());
    event.model = Some(context.display_model.clone());
    event.status_code = Some(status_code);
    event.latency_ms = Some(elapsed_millis_i64(context.started_at));
    enrich_usage_record_identity(
        &mut metadata,
        Some(&context.requested_model),
        &context.display_model,
        context.client_ip.as_deref(),
        context.request.client_user_agent.as_deref(),
        reasoning_effort_from_request(&context.request),
        context.request.service_tier(),
    );
    event.metadata = metadata;
    if let Err(error) = context.usage_records.record(event).await {
        tracing::warn!(account_id = %context.account_id, error = %error, "failed to record live response stream event");
    }
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
