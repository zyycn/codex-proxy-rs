use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::upstream::openai::protocol::responses::{CodexResponsesRequest, transport_requirement};
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PayloadAuditSnapshot {
    /// 按构造顺序记录的顶层字段。
    pub top_level_keys: Vec<String>,
    /// 红action后的 JSON payload。
    pub body: Value,
}

/// WebSocket audit artifact.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Deserialize)]
struct ResponseCompleted {
    #[serde(rename = "id")]
    id: String,
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
    websocket_event_type(raw).as_deref() == Some("response.metadata")
}

/// 提取 `response.metadata` 帧中的字符串响应头。
pub fn websocket_metadata_headers(raw: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    if value.get("type").and_then(Value::as_str) != Some("response.metadata") {
        return Vec::new();
    }
    value
        .get("headers")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|headers| headers.iter())
        .filter_map(|(name, value)| json_value_as_string(value).map(|value| (name.clone(), value)))
        .collect()
}

/// 从 `response.metadata` 帧中提取 `x-codex-turn-state`。
pub fn websocket_metadata_turn_state(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    websocket_metadata_headers(raw)
        .into_iter()
        .find_map(|(name, value)| {
            name.eq_ignore_ascii_case("x-codex-turn-state")
                .then_some(value)
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

/// 校验 `response.completed` 的官方响应形状并提取 response ID。
pub fn websocket_response_completed_id(raw: &str) -> Result<Option<String>, String> {
    let value = serde_json::from_str::<Value>(raw).map_err(|error| error.to_string())?;
    if value.get("type").and_then(Value::as_str) != Some("response.completed") {
        return Ok(None);
    }
    let response = value
        .get("response")
        .filter(|response| !response.is_null())
        .ok_or_else(|| "response.completed is missing response".to_string())?;
    let completed = serde_json::from_value::<ResponseCompleted>(response.clone())
        .map_err(|error| format!("failed to parse ResponseCompleted: {error}"))?;
    if completed.id.trim().is_empty() {
        return Err("response.completed contains an empty response id".to_string());
    }
    Ok(Some(completed.id))
}

/// 判断 WebSocket 事件是否会结束当前响应流。
pub fn is_terminal_websocket_event(event: &str) -> bool {
    matches!(
        event,
        "response.completed" | "response.incomplete" | "response.failed" | "error"
    )
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
        transport_mode: transport_requirement(request).as_str().to_string(),
        fallback_allowed: transport_requirement(request).allows_pre_send_http_fallback(),
        opening: Some(opening),
        payload: Some(payload),
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
