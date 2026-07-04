//! Dispatch 使用记录事件辅助。

use std::time::Instant;

use serde_json::Value;

use crate::{
    admin::monitoring::{
        usage_record_model::{ResponseUsageRecord, UsageRecord, UsageRecordLevel},
        usage_record_service::AdminUsageRecordService,
    },
    infra::time::elapsed_millis_i64,
    upstream::protocol::responses::{CodexCompactRequest, CodexResponsesRequest},
};

pub(super) struct DispatchErrorUsageRecord<'a> {
    pub usage_records: &'a AdminUsageRecordService,
    pub request_id: &'a str,
    pub account_id: Option<&'a str>,
    pub route: &'a str,
    pub model: &'a str,
    pub started_at: Instant,
    pub status_code: i64,
    pub message: &'a str,
    pub metadata: Value,
}

pub(super) async fn record_dispatch_error_event(record: DispatchErrorUsageRecord<'_>) {
    let mut metadata = record.metadata;
    enrich_event_route_metadata(&mut metadata, record.route);

    let mut event = UsageRecord::new(
        event_kind(record.route),
        UsageRecordLevel::Error,
        record.message,
    );
    event.request_id = Some(record.request_id.to_string());
    event.account_id = record.account_id.map(ToString::to_string);
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(record.status_code);
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    event.metadata = metadata;

    if let Err(error) = record.usage_records.record(event).await {
        tracing::warn!(
            account_id = record.account_id.unwrap_or(""),
            error = %error,
            "failed to record dispatch error event"
        );
    }
}

pub(super) async fn record_response_event(record: ResponseUsageRecord<'_>) {
    let mut metadata = record.metadata;
    enrich_event_route_metadata(&mut metadata, record.route);

    let mut event = UsageRecord::new(event_kind(record.route), record.level, record.message);
    event.request_id = Some(record.request_id.to_string());
    event.account_id = Some(record.account_id.to_string());
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(record.status_code);
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    enrich_usage_record_identity(
        &mut metadata,
        record.requested_model,
        record.model,
        record.client_ip,
        record.client_user_agent,
        record.reasoning_effort,
        record.service_tier,
    );
    event.metadata = metadata;

    if !record.rate_limit_headers.is_empty() {
        if let Some(object) = event.metadata.as_object_mut() {
            object.insert(
                "rateLimitHeaders".to_string(),
                serde_json::json!(record.rate_limit_headers),
            );
        }
    }

    if let Err(error) = record.usage_records.record(event).await {
        tracing::warn!(account_id = %record.account_id, error = %error, "failed to record response event");
    }
}

pub(super) fn event_kind(route: &str) -> &'static str {
    let _ = route;
    "v1.response"
}

pub(super) fn api_kind(route: &str) -> &'static str {
    let _ = route;
    "responses"
}

pub(super) fn enrich_event_route_metadata(metadata: &mut Value, route: &str) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object
        .entry("route".to_string())
        .or_insert_with(|| Value::String(route.to_string()));
    object
        .entry("apiKind".to_string())
        .or_insert_with(|| Value::String(api_kind(route).to_string()));
}

pub(super) fn enrich_usage_record_identity(
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

pub(super) fn reasoning_effort_from_request(request: &CodexResponsesRequest) -> Option<&str> {
    reasoning_effort_from_value(request.reasoning.as_ref())
}

pub(super) fn reasoning_effort_from_compact_request(request: &CodexCompactRequest) -> Option<&str> {
    reasoning_effort_from_value(request.reasoning.as_ref())
}

fn insert_trimmed_string(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn reasoning_effort_from_value(reasoning: Option<&Value>) -> Option<&str> {
    reasoning?
        .get("effort")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
