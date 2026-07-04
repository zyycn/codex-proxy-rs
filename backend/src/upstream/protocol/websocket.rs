use serde::{ser::SerializeMap, Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::upstream::protocol::responses::{
    http_sse_fallback_allowed, transport_for_request, CodexResponsesRequest, CodexTransport,
};
use crate::upstream::protocol::sse::encode_sse_event;

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

fn websocket_event_should_skip(raw: &str) -> bool {
    websocket_metadata_turn_state(raw).is_some()
        || websocket_event_shape_parse_error(raw)
        || websocket_response_completed_missing_response(raw)
        || websocket_response_created_missing_response(raw)
        || websocket_response_output_text_delta_missing_delta(raw)
        || websocket_delta_event_missing_official_required_fields(raw)
        || websocket_output_item_event_missing_item(raw)
        || websocket_output_item_event_non_object_item(raw)
        || websocket_output_item_event_invalid_item_type_tag(raw)
        || websocket_output_item_event_invalid_metadata(raw)
        || websocket_message_output_item_event_invalid_required_fields(raw)
        || websocket_agent_message_output_item_event_invalid_required_fields(raw)
        || websocket_reasoning_output_item_event_invalid_required_fields(raw)
        || websocket_function_call_output_item_event_invalid_required_fields(raw)
        || websocket_function_call_output_payload_item_event_invalid_required_fields(raw)
        || websocket_custom_tool_call_output_item_event_invalid_required_fields(raw)
        || websocket_custom_tool_call_output_payload_item_event_invalid_required_fields(raw)
        || websocket_tool_search_call_output_item_event_invalid_required_fields(raw)
        || websocket_tool_search_output_item_event_invalid_required_fields(raw)
        || websocket_local_shell_call_output_item_event_invalid_required_fields(raw)
        || websocket_web_search_call_output_item_event_invalid_required_fields(raw)
        || websocket_image_generation_call_output_item_event_invalid_required_fields(raw)
        || websocket_compaction_output_item_event_invalid_required_fields(raw)
        || websocket_reasoning_summary_part_added_missing_summary_index(raw)
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

/// 判断事件是否不符合官方流事件的基本字段类型。
pub fn websocket_event_shape_parse_error(raw: &str) -> bool {
    serde_json::from_str::<ResponsesStreamEventShape>(raw).is_err()
}

fn json_field_absent_or_null(value: &Value, field: &str) -> bool {
    matches!(value.get(field), None | Some(Value::Null))
}

/// 判断 `response.completed` 是否缺少 `response`。
pub fn websocket_response_completed_missing_response(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.completed")
        && json_field_absent_or_null(&value, "response")
}

/// 判断 `response.created` 是否缺少 `response`。
pub fn websocket_response_created_missing_response(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.created")
        && json_field_absent_or_null(&value, "response")
}

/// 判断 `response.output_text.delta` 是否缺少 `delta`。
pub fn websocket_response_output_text_delta_missing_delta(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
        && json_field_absent_or_null(&value, "delta")
}

/// 判断 delta 事件是否缺少官方必需字段。
pub fn websocket_delta_event_missing_official_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    match value.get("type").and_then(Value::as_str) {
        Some("response.custom_tool_call_input.delta") => {
            value.get("delta").and_then(Value::as_str).is_none()
                || (value.get("item_id").and_then(Value::as_str).is_none()
                    && value.get("call_id").and_then(Value::as_str).is_none())
        }
        Some("response.reasoning_summary_text.delta") => {
            value.get("delta").and_then(Value::as_str).is_none()
                || value.get("summary_index").and_then(Value::as_i64).is_none()
        }
        Some("response.reasoning_text.delta") => {
            value.get("delta").and_then(Value::as_str).is_none()
                || value.get("content_index").and_then(Value::as_i64).is_none()
        }
        _ => false,
    }
}

/// 判断 output item 事件是否缺少 `item`。
pub fn websocket_output_item_event_missing_item(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    is_output_item_event(&value) && json_field_absent_or_null(&value, "item")
}

/// 判断 output item 事件的 `item` 是否不是对象。
pub fn websocket_output_item_event_non_object_item(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    is_output_item_event(&value)
        && value
            .get("item")
            .is_some_and(|item| !item.is_null() && !item.is_object())
}

/// 判断 output item 事件的 `item.type` 是否缺失。
pub fn websocket_output_item_event_invalid_item_type_tag(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| item.get("type").and_then(Value::as_str).is_none())
}

/// 判断 output item 事件的 metadata 是否无效。
pub fn websocket_output_item_event_invalid_metadata(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .and_then(|item| item.get("metadata"))
        .is_some_and(|metadata| {
            if metadata.is_null() {
                return false;
            }
            metadata
                .as_object()
                .is_none_or(|metadata| optional_string_field_invalid(metadata, "turn_id"))
        })
}

fn is_output_item_event(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    )
}

/// 判断 message output item 是否缺少官方必需字段。
pub fn websocket_message_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && (optional_string_field_invalid(item, "id")
                    || optional_message_phase_invalid(item)
                    || item.get("role").and_then(Value::as_str).is_none()
                    || item
                        .get("content")
                        .and_then(Value::as_array)
                        .is_none_or(|content| {
                            content.iter().any(content_item_invalid_required_fields)
                        }))
        })
}

fn optional_string_field_invalid(item: &Map<String, Value>, field: &str) -> bool {
    item.get(field)
        .is_some_and(|value| !value.is_null() && !value.is_string())
}

fn optional_message_phase_invalid(item: &Map<String, Value>) -> bool {
    item.get("phase").is_some_and(|phase| {
        !phase.is_null() && !matches!(phase.as_str(), Some("commentary" | "final_answer"))
    })
}

fn content_item_invalid_required_fields(content_item: &Value) -> bool {
    let Some(content_item) = content_item.as_object() else {
        return true;
    };

    match content_item.get("type").and_then(Value::as_str) {
        Some("input_text" | "output_text") => {
            content_item.get("text").and_then(Value::as_str).is_none()
        }
        Some("input_image") => {
            content_item
                .get("image_url")
                .and_then(Value::as_str)
                .is_none()
                || content_item
                    .get("detail")
                    .is_some_and(|detail| !detail.is_null() && !valid_image_detail(detail))
        }
        _ => true,
    }
}

fn valid_image_detail(detail: &Value) -> bool {
    matches!(detail.as_str(), Some("auto" | "low" | "high" | "original"))
}

/// 判断 agent_message output item 是否缺少官方必需字段。
pub fn websocket_agent_message_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("agent_message")
                && (item.get("author").and_then(Value::as_str).is_none()
                    || item.get("recipient").and_then(Value::as_str).is_none()
                    || item
                        .get("content")
                        .and_then(Value::as_array)
                        .is_none_or(|content| {
                            content
                                .iter()
                                .any(agent_message_content_item_invalid_required_fields)
                        }))
        })
}

fn agent_message_content_item_invalid_required_fields(content_item: &Value) -> bool {
    let Some(content_item) = content_item.as_object() else {
        return true;
    };

    match content_item.get("type").and_then(Value::as_str) {
        Some("input_text") => content_item.get("text").and_then(Value::as_str).is_none(),
        Some("encrypted_content") => content_item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_none(),
        _ => true,
    }
}

/// 判断 reasoning output item 是否缺少官方必需字段。
pub fn websocket_reasoning_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("reasoning")
                && (item
                    .get("summary")
                    .and_then(Value::as_array)
                    .is_none_or(|summary| {
                        summary
                            .iter()
                            .any(reasoning_summary_item_invalid_required_fields)
                    })
                    || item.get("id").is_some_and(|id| !id.is_string())
                    || item.get("content").is_some_and(|content| {
                        !content.is_null()
                            && (!content.is_array()
                                || content.as_array().is_some_and(|content| {
                                    content
                                        .iter()
                                        .any(reasoning_content_item_invalid_required_fields)
                                }))
                    })
                    || item
                        .get("encrypted_content")
                        .is_some_and(|encrypted_content| {
                            !encrypted_content.is_null() && !encrypted_content.is_string()
                        }))
        })
}

fn reasoning_summary_item_invalid_required_fields(summary_item: &Value) -> bool {
    let Some(summary_item) = summary_item.as_object() else {
        return true;
    };

    summary_item.get("type").and_then(Value::as_str) != Some("summary_text")
        || summary_item.get("text").and_then(Value::as_str).is_none()
}

fn reasoning_content_item_invalid_required_fields(content_item: &Value) -> bool {
    let Some(content_item) = content_item.as_object() else {
        return true;
    };

    match content_item.get("type").and_then(Value::as_str) {
        Some("reasoning_text" | "text") => {
            content_item.get("text").and_then(Value::as_str).is_none()
        }
        _ => true,
    }
}

/// 判断 function_call output item 是否缺少官方必需字段。
pub fn websocket_function_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "namespace")
                    || item.get("name").and_then(Value::as_str).is_none()
                    || item.get("arguments").and_then(Value::as_str).is_none()
                    || item.get("call_id").and_then(Value::as_str).is_none())
        })
}

/// 判断 function_call_output output item 是否缺少官方必需字段。
pub fn websocket_function_call_output_payload_item_event_invalid_required_fields(
    raw: &str,
) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && (item.get("call_id").and_then(Value::as_str).is_none()
                    || item
                        .get("output")
                        .is_none_or(function_output_payload_invalid_required_fields))
        })
}

fn function_output_payload_invalid_required_fields(output: &Value) -> bool {
    if output.is_string() {
        return false;
    }

    let Some(output_items) = output.as_array() else {
        return true;
    };

    output_items
        .iter()
        .any(function_output_content_item_invalid_required_fields)
}

fn function_output_content_item_invalid_required_fields(content_item: &Value) -> bool {
    let Some(content_item) = content_item.as_object() else {
        return true;
    };

    match content_item.get("type").and_then(Value::as_str) {
        Some("input_text") => content_item.get("text").and_then(Value::as_str).is_none(),
        Some("input_image") => {
            content_item
                .get("image_url")
                .and_then(Value::as_str)
                .is_none()
                || content_item
                    .get("detail")
                    .is_some_and(|detail| !detail.is_null() && !valid_image_detail(detail))
        }
        Some("encrypted_content") => content_item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_none(),
        _ => true,
    }
}

/// 判断 custom_tool_call output item 是否缺少官方必需字段。
pub fn websocket_custom_tool_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("custom_tool_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "status")
                    || item.get("call_id").and_then(Value::as_str).is_none()
                    || item.get("name").and_then(Value::as_str).is_none()
                    || item.get("input").and_then(Value::as_str).is_none())
        })
}

/// 判断 custom_tool_call_output output item 是否缺少官方必需字段。
pub fn websocket_custom_tool_call_output_payload_item_event_invalid_required_fields(
    raw: &str,
) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("custom_tool_call_output")
                && (optional_string_field_invalid(item, "name")
                    || item.get("call_id").and_then(Value::as_str).is_none()
                    || item
                        .get("output")
                        .is_none_or(function_output_payload_invalid_required_fields))
        })
}

/// 判断 tool_search_call output item 是否缺少官方必需字段。
pub fn websocket_tool_search_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_search_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "call_id")
                    || optional_string_field_invalid(item, "status")
                    || item.get("execution").and_then(Value::as_str).is_none()
                    || !item.contains_key("arguments"))
        })
}

/// 判断 tool_search_output output item 是否缺少官方必需字段。
pub fn websocket_tool_search_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_search_output")
                && (optional_string_field_invalid(item, "call_id")
                    || item.get("status").and_then(Value::as_str).is_none()
                    || item.get("execution").and_then(Value::as_str).is_none()
                    || item.get("tools").and_then(Value::as_array).is_none())
        })
}

/// 判断 local_shell_call output item 是否缺少官方必需字段。
pub fn websocket_local_shell_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("local_shell_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "call_id")
                    || !valid_local_shell_status(item.get("status"))
                    || item
                        .get("action")
                        .and_then(Value::as_object)
                        .is_none_or(local_shell_action_invalid_required_fields))
        })
}

fn valid_local_shell_status(status: Option<&Value>) -> bool {
    matches!(
        status.and_then(Value::as_str),
        Some("completed" | "in_progress" | "incomplete")
    )
}

fn local_shell_action_invalid_required_fields(action: &Map<String, Value>) -> bool {
    action.get("type").and_then(Value::as_str) != Some("exec")
        || action
            .get("command")
            .and_then(Value::as_array)
            .is_none_or(|command| command.iter().any(|part| !part.is_string()))
        || optional_u64_field_invalid(action, "timeout_ms")
        || optional_string_field_invalid(action, "working_directory")
        || optional_string_field_invalid(action, "user")
        || optional_string_map_field_invalid(action, "env")
}

fn optional_u64_field_invalid(item: &Map<String, Value>, field: &str) -> bool {
    item.get(field)
        .is_some_and(|value| !value.is_null() && value.as_u64().is_none())
}

fn optional_string_map_field_invalid(item: &Map<String, Value>, field: &str) -> bool {
    item.get(field).is_some_and(|value| {
        !value.is_null()
            && value
                .as_object()
                .is_none_or(|object| object.values().any(|value| !value.is_string()))
    })
}

/// 判断 web_search_call output item 是否缺少官方必需字段。
pub fn websocket_web_search_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("web_search_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "status")
                    || item
                        .get("action")
                        .is_some_and(web_search_action_invalid_required_fields))
        })
}

fn web_search_action_invalid_required_fields(action: &Value) -> bool {
    if action.is_null() {
        return false;
    }

    let Some(action) = action.as_object() else {
        return true;
    };

    match action.get("type").and_then(Value::as_str) {
        Some("search") => {
            optional_string_field_invalid(action, "query")
                || optional_string_array_field_invalid(action, "queries")
        }
        Some("open_page") => optional_string_field_invalid(action, "url"),
        Some("find_in_page") => {
            optional_string_field_invalid(action, "url")
                || optional_string_field_invalid(action, "pattern")
        }
        Some(_) => false,
        None => true,
    }
}

fn optional_string_array_field_invalid(item: &Map<String, Value>, field: &str) -> bool {
    item.get(field).is_some_and(|value| {
        !value.is_null()
            && value
                .as_array()
                .is_none_or(|items| items.iter().any(|item| !item.is_string()))
    })
}

/// 判断 image_generation_call output item 是否缺少官方必需字段。
pub fn websocket_image_generation_call_output_item_event_invalid_required_fields(
    raw: &str,
) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("image_generation_call")
                && (item.get("id").and_then(Value::as_str).is_none()
                    || item.get("status").and_then(Value::as_str).is_none()
                    || optional_string_field_invalid(item, "revised_prompt")
                    || item.get("result").and_then(Value::as_str).is_none())
        })
}

/// 判断 compaction output item 是否缺少官方必需字段。
pub fn websocket_compaction_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| match item.get("type").and_then(Value::as_str) {
            Some("compaction" | "compaction_summary") => item
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_none(),
            Some("context_compaction") => optional_string_field_invalid(item, "encrypted_content"),
            _ => false,
        })
}

/// 判断 reasoning summary part added 事件是否缺少 summary_index。
pub fn websocket_reasoning_summary_part_added_missing_summary_index(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.reasoning_summary_part.added")
        && value.get("summary_index").and_then(Value::as_i64).is_none()
}

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
pub fn websocket_response_create_payload(request: &CodexResponsesRequest) -> Value {
    let mut body = Map::new();
    insert_value(
        &mut body,
        "type",
        Value::String("response.create".to_string()),
    );
    insert_value(&mut body, "model", Value::String(request.model.clone()));
    insert_value(
        &mut body,
        "instructions",
        Value::String(request.instructions.clone()),
    );
    insert_value(&mut body, "input", Value::Array(request.input.clone()));
    insert_value(&mut body, "store", Value::Bool(request.store));
    insert_value(&mut body, "stream", Value::Bool(request.stream));
    if let Some(previous_response_id) = &request.previous_response_id {
        insert_value(
            &mut body,
            "previous_response_id",
            Value::String(previous_response_id.clone()),
        );
    }
    if let Some(reasoning) = &request.reasoning {
        insert_value(&mut body, "reasoning", reasoning.clone());
    }
    if let Some(tools) = non_empty_tools(request) {
        insert_value(&mut body, "tools", Value::Array(tools.to_vec()));
    }
    insert_value(
        &mut body,
        "tool_choice",
        request
            .tool_choice
            .clone()
            .unwrap_or(Value::String("auto".to_string())),
    );
    insert_value(
        &mut body,
        "parallel_tool_calls",
        Value::Bool(request.parallel_tool_calls.unwrap_or(true)),
    );
    if let Some(text) = &request.text {
        insert_value(&mut body, "text", text.clone());
    }
    if let Some(service_tier) = &request.service_tier {
        insert_value(
            &mut body,
            "service_tier",
            Value::String(service_tier.clone()),
        );
    }
    if let Some(prompt_cache_key) = &request.prompt_cache_key {
        insert_value(
            &mut body,
            "prompt_cache_key",
            Value::String(prompt_cache_key.clone()),
        );
    }
    if let Some(include) = non_empty_include(request) {
        insert_value(
            &mut body,
            "include",
            include
                .iter()
                .cloned()
                .map(Value::String)
                .collect::<Vec<_>>()
                .into(),
        );
    }
    if let Some(client_metadata) = &request.client_metadata {
        insert_value(&mut body, "client_metadata", client_metadata.clone());
    }
    Value::Object(body)
}

/// 生成 Responses WebSocket `response.create` 文本帧内容。
pub fn websocket_response_create_payload_text(
    request: &CodexResponsesRequest,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&OrderedResponseCreatePayload(request))
}

struct OrderedResponseCreatePayload<'a>(&'a CodexResponsesRequest);

impl Serialize for OrderedResponseCreatePayload<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let request = self.0;
        let mut map = serializer.serialize_map(Some(websocket_payload_keys(request).len()))?;
        map.serialize_entry("type", "response.create")?;
        map.serialize_entry("model", &request.model)?;
        map.serialize_entry("instructions", &request.instructions)?;
        map.serialize_entry("input", &request.input)?;
        map.serialize_entry("store", &request.store)?;
        map.serialize_entry("stream", &request.stream)?;
        if let Some(previous_response_id) = &request.previous_response_id {
            map.serialize_entry("previous_response_id", previous_response_id)?;
        }
        if let Some(reasoning) = &request.reasoning {
            map.serialize_entry("reasoning", reasoning)?;
        }
        if let Some(tools) = non_empty_tools(request) {
            map.serialize_entry("tools", tools)?;
        }
        if let Some(tool_choice) = &request.tool_choice {
            map.serialize_entry("tool_choice", tool_choice)?;
        } else {
            map.serialize_entry("tool_choice", "auto")?;
        }
        map.serialize_entry(
            "parallel_tool_calls",
            &request.parallel_tool_calls.unwrap_or(true),
        )?;
        if let Some(text) = &request.text {
            map.serialize_entry("text", text)?;
        }
        if let Some(service_tier) = &request.service_tier {
            map.serialize_entry("service_tier", service_tier)?;
        }
        if let Some(prompt_cache_key) = &request.prompt_cache_key {
            map.serialize_entry("prompt_cache_key", prompt_cache_key)?;
        }
        if let Some(include) = non_empty_include(request) {
            map.serialize_entry("include", include)?;
        }
        if let Some(client_metadata) = &request.client_metadata {
            map.serialize_entry("client_metadata", client_metadata)?;
        }
        map.end()
    }
}

fn insert_value(body: &mut Map<String, Value>, key: &str, value: Value) {
    body.insert(key.to_string(), value);
}

fn websocket_payload_keys(request: &CodexResponsesRequest) -> Vec<String> {
    let mut keys = vec![
        "type".to_string(),
        "model".to_string(),
        "instructions".to_string(),
        "input".to_string(),
        "store".to_string(),
        "stream".to_string(),
    ];
    if request.previous_response_id.is_some() {
        keys.push("previous_response_id".to_string());
    }
    if request.reasoning.is_some() {
        keys.push("reasoning".to_string());
    }
    if non_empty_tools(request).is_some() {
        keys.push("tools".to_string());
    }
    keys.push("tool_choice".to_string());
    keys.push("parallel_tool_calls".to_string());
    if request.text.is_some() {
        keys.push("text".to_string());
    }
    if request.service_tier.is_some() {
        keys.push("service_tier".to_string());
    }
    if request.prompt_cache_key.is_some() {
        keys.push("prompt_cache_key".to_string());
    }
    if non_empty_include(request).is_some() {
        keys.push("include".to_string());
    }
    if request.client_metadata.is_some() {
        keys.push("client_metadata".to_string());
    }
    keys
}

fn non_empty_tools(request: &CodexResponsesRequest) -> Option<&[Value]> {
    request.tools.as_deref().filter(|tools| !tools.is_empty())
}

fn non_empty_include(request: &CodexResponsesRequest) -> Option<&[String]> {
    request
        .include
        .as_deref()
        .filter(|include| !include.is_empty())
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
