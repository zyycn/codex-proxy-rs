//! Dispatch 使用记录事件辅助。

use std::time::Instant;

use serde_json::Value;

use crate::{
    infra::time::elapsed_millis_i64,
    telemetry::{
        ops::store::{PgOpsErrorLogStore, PgOpsErrorLogStoreError},
        ops::types::OpsErrorLog,
        usage::{
            store::{PgUsageRecordStore, PgUsageRecordStoreError},
            types::{
                metadata_i64, metadata_service_tier, metadata_string as metadata_string_any,
                ResponseUsageRecord, UsageRecord,
            },
        },
    },
    upstream::openai::protocol::responses::{CodexCompactRequest, CodexResponsesRequest},
};

/// 成功与失败事实的唯一写入入口。
#[derive(Clone)]
pub struct Recorder {
    usage_records: PgUsageRecordStore,
    ops_errors: PgOpsErrorLogStore,
    usage_enabled: bool,
    capture_body: bool,
}

/// 遥测事实写入错误。
#[derive(Debug, thiserror::Error)]
pub enum RecorderError {
    #[error("invalid success usage fact")]
    InvalidUsageFact,
    #[error(transparent)]
    Usage(#[from] PgUsageRecordStoreError),
    #[error(transparent)]
    Ops(#[from] PgOpsErrorLogStoreError),
}

impl Recorder {
    pub fn new(
        usage_records: PgUsageRecordStore,
        ops_errors: PgOpsErrorLogStore,
        usage_enabled: bool,
        capture_body: bool,
    ) -> Self {
        Self {
            usage_records,
            ops_errors,
            usage_enabled,
            capture_body,
        }
    }

    /// 校验并写入一条成功事实及其时间桶。
    pub async fn record_usage(&self, mut event: UsageRecord) -> Result<(), RecorderError> {
        if !self.usage_enabled {
            return Ok(());
        }
        if !is_usage_fact(&event) {
            tracing::warn!(
                usage_record_id = %event.id,
                request_id = event.request_id.as_deref().unwrap_or(""),
                account_id = event.account_id,
                model = event.model,
                status_code = event.status_code,
                "rejected invalid success usage fact"
            );
            return Err(RecorderError::InvalidUsageFact);
        }
        apply_success_capture_body_policy(&mut event, self.capture_body);
        self.usage_records.append(&event).await?;
        Ok(())
    }

    /// 规范化并写入一条失败事实及其时间桶。
    pub async fn record_error(&self, mut event: OpsErrorLog) -> Result<(), RecorderError> {
        lift_error_fact_fields(&mut event);
        apply_error_capture_body_policy(&mut event, self.capture_body);
        self.ops_errors.append(&event).await?;
        Ok(())
    }
}

pub(crate) struct DispatchErrorLogRecord<'a> {
    pub recorder: &'a Recorder,
    pub request_id: &'a str,
    pub client_api_key_id: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub account_id: Option<&'a str>,
    pub route: &'a str,
    pub model: &'a str,
    pub started_at: Instant,
    pub status_code: i64,
    pub message: &'a str,
    pub metadata: Value,
}

pub(crate) async fn record_dispatch_error_event(record: DispatchErrorLogRecord<'_>) {
    let mut metadata = record.metadata;
    enrich_event_route_metadata(&mut metadata, record.route);

    let mut event = OpsErrorLog::new(event_kind(record.route), record.message);
    event.request_id = Some(record.request_id.to_string());
    event.client_api_key_id = record.client_api_key_id.map(ToString::to_string);
    event.provider = record.provider.map(ToString::to_string);
    event.account_id = record.account_id.map(ToString::to_string);
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(record.status_code);
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    event.metadata = metadata;

    if let Err(error) = record.recorder.record_error(event).await {
        tracing::warn!(
            account_id = record.account_id.unwrap_or(""),
            error = %error,
            "failed to record dispatch error event"
        );
    }
}

pub(crate) async fn record_response_event(record: ResponseUsageRecord<'_>) {
    let mut metadata = record.metadata;
    enrich_event_route_metadata(&mut metadata, record.route);

    let mut event = UsageRecord::new(
        event_kind(record.route),
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
    if !record.rate_limit_headers.is_empty() {
        if let Some(object) = metadata.as_object_mut() {
            object.insert(
                "rateLimitHeaders".to_string(),
                serde_json::json!(record.rate_limit_headers),
            );
        }
    }

    lift_success_fact_fields(&mut event, &mut metadata);
    event.metadata = metadata;

    if let Err(error) = record.recorder.record_usage(event).await {
        tracing::warn!(account_id = %record.account_id, error = %error, "failed to record response event");
    }
}

fn is_usage_fact(event: &UsageRecord) -> bool {
    (200..=399).contains(&event.status_code)
        && !event.provider.trim().is_empty()
        && !event.account_id.trim().is_empty()
        && !event.model.trim().is_empty()
}

fn apply_success_capture_body_policy(event: &mut UsageRecord, capture_body: bool) {
    if capture_body {
        return;
    }
    remove_body_fields(&mut event.metadata);
}

fn apply_error_capture_body_policy(event: &mut OpsErrorLog, capture_body: bool) {
    let Some(metadata) = event.metadata.as_object_mut() else {
        return;
    };
    for key in body_fields() {
        if capture_body {
            if let Some(Value::String(value)) = metadata.get_mut(key) {
                value.truncate(4096);
            }
        } else {
            metadata.remove(key);
        }
    }
}

fn remove_body_fields(metadata: &mut Value) {
    let Some(metadata) = metadata.as_object_mut() else {
        return;
    };
    for key in body_fields() {
        metadata.remove(key);
    }
}

fn body_fields() -> [&'static str; 5] {
    [
        "body",
        "rawBody",
        "requestBody",
        "responseBody",
        "upstreamBody",
    ]
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

fn lift_success_fact_fields(event: &mut UsageRecord, metadata: &mut Value) {
    event.transport = metadata_string(metadata, "transport");
    event.attempt_index = metadata_nonnegative_i64(metadata, "attemptIndex");
    event.response_id = metadata_string(metadata, "responseId");
    event.upstream_request_id = metadata_string(metadata, "upstreamRequestId")
        .or_else(|| metadata_string(metadata, "openaiRequestId"));
    event.first_token_ms = metadata_nonnegative_i64(metadata, "firstTokenMs");
    if let Some(usage) = metadata.get("usage") {
        event.input_tokens = value_nonnegative_i64(usage, "inputTokens");
        event.output_tokens = value_nonnegative_i64(usage, "outputTokens");
        event.cached_tokens = value_nonnegative_i64(usage, "cachedTokens");
        event.reasoning_tokens = value_nonnegative_i64(usage, "reasoningTokens");
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
    value_nonnegative_i64(metadata, field)
}

fn value_nonnegative_i64(value: &Value, field: &str) -> Option<i64> {
    value
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

pub(crate) fn event_kind(route: &str) -> &'static str {
    let _ = route;
    "v1.response"
}

pub(crate) fn api_kind(route: &str) -> &'static str {
    let _ = route;
    "responses"
}

pub(crate) fn enrich_event_route_metadata(metadata: &mut Value, route: &str) {
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

pub(crate) fn enrich_usage_record_identity(
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

pub(crate) fn reasoning_effort_from_request(request: &CodexResponsesRequest) -> Option<&str> {
    reasoning_effort_from_value(request.reasoning())
}

pub(crate) fn reasoning_effort_from_compact_request(request: &CodexCompactRequest) -> Option<&str> {
    reasoning_effort_from_value(request.reasoning())
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
