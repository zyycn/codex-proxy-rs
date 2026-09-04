use gateway_protocol::openai::sse::encode_sse_event;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::transport::protocol::responses::{
    CodexResponsesRequest, PREVIOUS_RESPONSE_NOT_FOUND_CODE,
    is_bare_invalid_previous_response_id_error, transport_requirement,
};

const REDACTED_PAYLOAD_VALUE: &str = "<redacted>";

/// WebSocket 握手审计快照。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpeningAuditSnapshot {
    /// 请求行。
    pub request_line: String,
    /// 请求头顺序。
    pub header_order: Vec<String>,
    /// 脱敏后的请求头。
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
    /// 脱敏后的 JSON payload。
    pub body: Value,
}

/// WebSocket 审计产物。
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

/// 将一条公开 WebSocket JSON 事件编码为 SSE 帧。
pub fn websocket_event_to_sse_frame(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    websocket_event_frame(&value, raw)
}

/// 同上，复用已解析的事件 JSON；`data` 段通常逐字节内嵌 `raw` 原文。
///
/// 仅剥离传输层内部帧：`codex.rate_limits` 与承载 turn_state 的
/// `response.metadata` 不下发客户端。唯一的业务帧适配是为上游省略错误码的
/// `Invalid previous_response_id` 补齐官方客户端用于完整历史重放的稳定错误码；
/// 原始 status、type 与 message 均保持不变。
pub(crate) fn websocket_event_frame(value: &Value, raw: &str) -> Option<String> {
    let event = websocket_event_type(value)?;
    if is_internal_websocket_event(event) || event == "response.metadata" {
        return None;
    }
    let normalized = previous_response_not_found_event(value);
    Some(encode_sse_event(
        event,
        normalized.as_deref().unwrap_or(raw),
    ))
}

fn previous_response_not_found_event(value: &Value) -> Option<String> {
    if websocket_event_type(value) != Some("error")
        || value
            .get("status")
            .or_else(|| value.get("status_code"))
            .and_then(Value::as_u64)
            != Some(400)
    {
        return None;
    }
    let error = value.get("error")?.as_object()?;
    if !is_bare_invalid_previous_response_id_error(
        error.get("code"),
        error.get("type").and_then(Value::as_str),
        error.get("message").and_then(Value::as_str),
    ) {
        return None;
    }

    let mut normalized = value.clone();
    normalized.get_mut("error")?.as_object_mut()?.insert(
        "code".to_owned(),
        Value::String(PREVIOUS_RESPONSE_NOT_FOUND_CODE.to_owned()),
    );
    serde_json::to_string(&normalized).ok()
}

pub(crate) fn websocket_event_type(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

fn is_internal_websocket_event(event: &str) -> bool {
    event == "codex.rate_limits"
}

/// 提取 Responses WebSocket metadata 帧中的字符串响应头。
pub fn websocket_metadata_headers(value: &Value) -> Vec<(String, String)> {
    if !is_websocket_metadata_event(websocket_event_type(value)) {
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

/// 从 Responses WebSocket metadata 帧中提取 `x-codex-turn-state`。
pub fn websocket_metadata_turn_state(value: &Value) -> Option<String> {
    websocket_metadata_headers(value)
        .into_iter()
        .find_map(|(name, value)| {
            name.eq_ignore_ascii_case("x-codex-turn-state")
                .then_some(value)
        })
}

fn is_websocket_metadata_event(event: Option<&str>) -> bool {
    matches!(event, Some("response.metadata" | "codex.response.metadata"))
}

/// 上游 WebSocket 连接寿命限制错误码。
pub(crate) const WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE: &str =
    "websocket_connection_limit_reached";

/// 若首个可投递 SSE 帧是上游连接寿命限制错误帧，返回其 message。
pub(crate) fn websocket_connection_limit_message(frame: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(frame).ok()?;
    let event = gateway_protocol::openai::sse::parse_sse_events(text)
        .ok()?
        .into_iter()
        .next()?;
    if event.event.as_deref() != Some("error") {
        return None;
    }
    let value = serde_json::from_str::<Value>(&event.data).ok()?;
    let code = value.pointer("/error/code").and_then(Value::as_str)?;
    (code == WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE).then(|| {
        value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("websocket connection limit reached")
            .to_owned()
    })
}

fn json_value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Array(items) => items.first().and_then(json_value_as_string),
        _ => None,
    }
}

/// 从 `response.completed` 旁路提取 response ID，供连接池记录续接能力。
///
/// 该值不参与客户端 wire 的可交付性判断；无法读取时只是不记录连接内续接状态。
pub fn websocket_response_completed_id(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("response.completed") {
        return None;
    }
    value
        .pointer("/response/id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
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

/// 借用原始 body 序列化 `response.create` 帧，不复制字段。
///
/// 输出与序列化 [`websocket_response_create_payload`] 的合并 Map 逐字节一致：
/// body 自带 `type` 键时以 body 值置于首位，其余字段按插入顺序原样透传。
struct ResponseCreateFrame<'a> {
    body: &'a Map<String, Value>,
}

impl serde::Serialize for ResponseCreateFrame<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = serializer.serialize_map(Some(self.body.len() + 1))?;
        match self.body.get("type") {
            Some(body_type) => map.serialize_entry("type", body_type)?,
            None => map.serialize_entry("type", "response.create")?,
        }
        for (key, value) in self.body {
            if key == "type" {
                continue;
            }
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// 生成 Responses WebSocket `response.create` 文本帧内容。
pub fn websocket_response_create_payload_text(
    request: &CodexResponsesRequest,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&ResponseCreateFrame {
        body: request.body(),
    })
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
