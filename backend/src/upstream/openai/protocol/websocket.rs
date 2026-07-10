use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::upstream::openai::protocol::responses::{
    http_sse_fallback_allowed, transport_for_request, CodexResponsesRequest, CodexTransport,
};
use crate::upstream::openai::protocol::sse::encode_sse_event;

const REDACTED_PAYLOAD_VALUE: &str = "<redacted>";

/// WebSocket 握手审计快照。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpeningAuditSnapshot {
    /// 请求行。
    pub request_line: String,
    /// 请求头顺序。
    pub header_order: Vec<String>,
    /// 红action后的请求头。
    pub headers: Vec<OpeningAuditHeader>,
}

/// WebSocket 握手审计请求头。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpeningAuditHeader {
    /// 请求头名。
    pub name: String,
    /// 请求头值。
    pub value: String,
}

/// WebSocket payload 审计快照。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PayloadAuditSnapshot {
    /// 按构造顺序记录的顶层字段。
    pub top_level_keys: Vec<String>,
    /// 红action后的 JSON payload。
    pub body: Value,
}

/// WebSocket audit artifact.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WebSocketAuditArtifact {
    /// 实际选择的传输模式。
    pub transport_mode: String,
    /// 当前请求是否允许 HTTP/SSE fallback。
    pub fallback_allowed: bool,
    /// 打开握手快照。
    pub opening: Option<OpeningAuditSnapshot>,
    /// 首个 `response.create` payload 快照。
    pub payload: Option<PayloadAuditSnapshot>,
}

/// 从 WebSocket 错误帧推导出的上游错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifiedWebSocketError {
    /// 应映射给 HTTP 层的状态码。
    pub status_code: u16,
}

#[derive(Debug, Deserialize)]
struct ResponseCompleted {
    #[serde(rename = "id")]
    _id: String,
    #[serde(default, rename = "usage")]
    _usage: Option<ResponseCompletedUsage>,
    #[serde(default, rename = "end_turn")]
    _end_turn: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ResponseCompletedUsage {
    #[serde(rename = "input_tokens")]
    _input_tokens: i64,
    #[serde(default, rename = "input_tokens_details")]
    _input_tokens_details: Option<ResponseCompletedInputTokensDetails>,
    #[serde(rename = "output_tokens")]
    _output_tokens: i64,
    #[serde(default, rename = "output_tokens_details")]
    _output_tokens_details: Option<ResponseCompletedOutputTokensDetails>,
    #[serde(rename = "total_tokens")]
    _total_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct ResponseCompletedInputTokensDetails {
    #[serde(rename = "cached_tokens")]
    _cached_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct ResponseCompletedOutputTokensDetails {
    #[serde(rename = "reasoning_tokens")]
    _reasoning_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct ResponsesStreamEventShape {
    #[serde(rename = "type")]
    _kind: String,
    #[serde(default, rename = "headers")]
    _headers: Option<Value>,
    #[serde(default, rename = "metadata")]
    _metadata: Option<Value>,
    #[serde(default, rename = "response")]
    _response: Option<Value>,
    #[serde(default, rename = "item")]
    _item: Option<Value>,
    #[serde(default, rename = "item_id")]
    _item_id: Option<String>,
    #[serde(default, rename = "call_id")]
    _call_id: Option<String>,
    #[serde(default, rename = "delta")]
    _delta: Option<String>,
    #[serde(default, rename = "summary_index")]
    _summary_index: Option<i64>,
    #[serde(default, rename = "content_index")]
    _content_index: Option<i64>,
}

/// 将一条公开 WebSocket JSON 事件编码为 SSE 帧。
pub fn websocket_event_to_sse_frame(raw: &str) -> Option<String> {
    let event = websocket_event_type(raw)?;
    if is_internal_websocket_event(&event) {
        return None;
    }
    if websocket_event_should_skip(raw) {
        return None;
    }
    Some(encode_sse_event(&event, raw))
}

pub(crate) fn websocket_event_type(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw).ok().and_then(|value| {
        value
            .get("type")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn is_internal_websocket_event(event: &str) -> bool {
    event == "codex.rate_limits"
}

/// 判断 WebSocket 事件是否应在转发前剥离。
///
/// 仅剥离传输层内部帧：`response.metadata` 承载 turn_state，由上层提取转存到
/// 会话状态，不下发客户端。业务事件一律原样转发，不做 schema 校验。
/// （`codex.rate_limits` 内部事件由 `is_internal_websocket_event` 单独剥离。）
fn websocket_event_should_skip(raw: &str) -> bool {
    websocket_metadata_turn_state(raw).is_some()
}

/// 从 `response.metadata` 帧中提取 `x-codex-turn-state`。
pub fn websocket_metadata_turn_state(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("response.metadata") {
        return None;
    }
    value
        .get("headers")
        .and_then(Value::as_object)
        .and_then(|headers| {
            headers.iter().find_map(|(name, value)| {
                if name.eq_ignore_ascii_case("x-codex-turn-state") {
                    json_value_as_string(value)
                } else {
                    None
                }
            })
        })
        .or_else(|| {
            value
                .pointer("/metadata/turn_state")
                .or_else(|| value.get("turn_state"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn json_value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Array(items) => items.first().and_then(json_value_as_string),
        _ => None,
    }
}

/// 校验 `response.completed` 的官方响应形状。
pub fn websocket_response_completed_parse_error(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("response.completed") {
        return None;
    }
    let response = value.get("response")?;
    if response.is_null() {
        return None;
    }
    match serde_json::from_value::<ResponseCompleted>(response.clone()) {
        Ok(_) => None,
        Err(error) => Some(format!("failed to parse ResponseCompleted: {error}")),
    }
}

/// 从 `response.incomplete` 中提取 incomplete reason。
pub fn websocket_incomplete_response_reason(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("response.incomplete") {
        return None;
    }
    value
        .pointer("/response/incomplete_details/reason")
        .or_else(|| value.pointer("/incomplete_details/reason"))
        .or_else(|| value.get("reason"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

#[path = "websocket_errors.rs"]
mod errors;
pub use errors::*;

/// 判断 WebSocket 事件是否会结束当前响应流。
pub fn is_terminal_websocket_event(event: &str) -> bool {
    matches!(event, "response.completed" | "response.failed" | "error")
}

/// 将 WebSocket 错误帧映射为上游 HTTP 状态分类。
pub fn classify_websocket_error_frame(raw: &str) -> Option<ClassifiedWebSocketError> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let event_type = value.get("type").and_then(Value::as_str)?;
    if event_type != "error" && event_type != "response.failed" {
        return None;
    }

    let code = websocket_error_code(&value);
    if code.as_deref() == Some("websocket_connection_limit_reached") {
        return Some(ClassifiedWebSocketError { status_code: 503 });
    }

    if event_type == "error" {
        if let Some(status_code) = explicit_error_status_code(&value) {
            if (200..=299).contains(&status_code) {
                return None;
            }
            return Some(ClassifiedWebSocketError { status_code });
        }
    }

    if let Some(code) = code {
        if let Some(status_code) = rotatable_error_status_code(&code) {
            return Some(ClassifiedWebSocketError { status_code });
        }
    }

    None
}

fn websocket_error_code(value: &Value) -> Option<String> {
    value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/response/error/type"))
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.pointer("/error/type"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
}

fn explicit_error_status_code(value: &Value) -> Option<u16> {
    value
        .get("status")
        .or_else(|| value.get("status_code"))
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
}

fn rotatable_error_status_code(code: &str) -> Option<u16> {
    match code {
        "usage_limit_reached" | "rate_limit_exceeded" | "rate_limit_reached" => Some(429),
        "quota_exhausted" | "payment_required" => Some(402),
        "unauthorized" | "token_invalid" | "token_expired" | "account_deactivated" => Some(401),
        "forbidden" | "account_banned" | "banned" => Some(403),
        "previous_response_not_found" => Some(400),
        _ => None,
    }
}

/// 从包裹在 WebSocket `error.headers` 中的 retry-after 值提取秒数。
pub fn retry_after_seconds_from_wrapped_error_headers(raw: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let header_retry_after = value
        .get("headers")
        .and_then(Value::as_object)
        .and_then(|headers| {
            headers.iter().find_map(|(name, value)| {
                if name.eq_ignore_ascii_case("retry-after") {
                    retry_after_seconds_from_json_header_value(value)
                } else {
                    None
                }
            })
        });
    header_retry_after.or_else(|| {
        value
            .pointer("/error/retry_after_seconds")
            .or_else(|| value.get("retry_after_seconds"))
            .and_then(Value::as_u64)
    })
}

fn retry_after_seconds_from_json_header_value(value: &Value) -> Option<u64> {
    let seconds = match value {
        Value::String(value) => value.trim().parse::<u64>().ok()?,
        Value::Number(value) => value.as_u64()?,
        Value::Array(values) => values
            .first()
            .and_then(retry_after_seconds_from_json_header_value)?,
        _ => return None,
    };
    (seconds > 0).then_some(seconds)
}

/// 生成 Responses WebSocket payload 审计快照。
pub fn websocket_payload_audit_snapshot(request: &CodexResponsesRequest) -> PayloadAuditSnapshot {
    let body = websocket_response_create_payload(request);
    PayloadAuditSnapshot {
        top_level_keys: websocket_payload_keys(request),
        body: redact_payload_body(body),
    }
}

/// 为单次 opening 尝试构建 WebSocket 审计 artifact。
pub fn websocket_audit_artifact_from_attempt(
    request: &CodexResponsesRequest,
    opening: OpeningAuditSnapshot,
    payload: PayloadAuditSnapshot,
) -> WebSocketAuditArtifact {
    WebSocketAuditArtifact {
        transport_mode: websocket_transport_mode_name(request).to_string(),
        fallback_allowed: http_sse_fallback_allowed(request),
        opening: Some(opening),
        payload: Some(payload),
    }
}

fn websocket_transport_mode_name(request: &CodexResponsesRequest) -> &'static str {
    match transport_for_request(request) {
        CodexTransport::HttpSse => "http_sse",
        CodexTransport::WebSocketPreferred => "websocket_preferred",
        CodexTransport::WebSocketRequired => "websocket_required",
    }
}

/// 生成 Responses WebSocket `response.create` payload。
///
/// payload = `{"type": "response.create"}` 加上原始上游 body 的全部字段
/// （保持插入顺序，含未知字段），逐字段原样透传。
pub fn websocket_response_create_payload(request: &CodexResponsesRequest) -> Value {
    let mut payload = Map::new();
    payload.insert(
        "type".to_string(),
        Value::String("response.create".to_string()),
    );
    for (key, value) in request.body() {
        payload.insert(key.clone(), value.clone());
    }
    Value::Object(payload)
}

/// 生成 Responses WebSocket `response.create` 文本帧内容。
pub fn websocket_response_create_payload_text(
    request: &CodexResponsesRequest,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&websocket_response_create_payload(request))
}

fn websocket_payload_keys(request: &CodexResponsesRequest) -> Vec<String> {
    let mut keys = Vec::with_capacity(request.body().len() + 1);
    keys.push("type".to_string());
    keys.extend(request.body().keys().cloned());
    keys
}

fn redact_payload_body(body: Value) -> Value {
    let Value::Object(body) = body else {
        return body;
    };

    Value::Object(
        body.into_iter()
            .map(|(key, value)| {
                let value = if is_sensitive_payload_key(&key) {
                    Value::String(REDACTED_PAYLOAD_VALUE.to_string())
                } else {
                    value
                };
                (key, value)
            })
            .collect(),
    )
}

fn is_sensitive_payload_key(key: &str) -> bool {
    matches!(
        key,
        "instructions"
            | "input"
            | "previous_response_id"
            | "prompt_cache_key"
            | "client_metadata"
            | "tools"
    )
}
