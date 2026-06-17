use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio_tungstenite::tungstenite::{http::HeaderMap as WsHeaderMap, Error as WsError, Message};

use crate::codex::gateway::transport::{
    retry_after::retry_after_seconds_from_body, sse::encode_sse_event, types::CodexResponsesRequest,
};

use super::CodexWebSocketError;

const REDACTED_PAYLOAD_VALUE: &str = "<redacted>";

pub(super) struct ClassifiedWebSocketError {
    pub(super) status: StatusCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WebSocketErrorClassificationProfile {
    OneShot,
    Pooled,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PayloadAuditSnapshot {
    pub top_level_keys: Vec<String>,
    pub body: Value,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResponseCompleted {
    id: String,
    #[serde(default)]
    usage: Option<ResponseCompletedUsage>,
    #[serde(default)]
    end_turn: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResponseCompletedUsage {
    input_tokens: i64,
    input_tokens_details: Option<ResponseCompletedInputTokensDetails>,
    output_tokens: i64,
    output_tokens_details: Option<ResponseCompletedOutputTokensDetails>,
    total_tokens: i64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResponseCompletedInputTokensDetails {
    cached_tokens: i64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResponseCompletedOutputTokensDetails {
    reasoning_tokens: i64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResponsesStreamEventShape {
    #[serde(rename = "type")]
    kind: String,
    headers: Option<Value>,
    metadata: Option<Value>,
    response: Option<Value>,
    item: Option<Value>,
    item_id: Option<String>,
    call_id: Option<String>,
    delta: Option<String>,
    summary_index: Option<i64>,
    content_index: Option<i64>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ResponsesWsRequest<'a> {
    #[serde(rename = "response.create")]
    ResponseCreate(ResponseCreateWsPayload<'a>),
}

#[derive(Serialize)]
struct ResponseCreateWsPayload<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    instructions: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: &'a [Value],
    tools: Vec<Value>,
    tool_choice: Value,
    parallel_tool_calls: bool,
    reasoning: Option<&'a Value>,
    store: bool,
    stream: bool,
    include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_metadata: Option<&'a Value>,
}

impl<'a> ResponsesWsRequest<'a> {
    fn from_request(request: &'a CodexResponsesRequest) -> Self {
        Self::ResponseCreate(ResponseCreateWsPayload {
            model: &request.model,
            instructions: &request.instructions,
            previous_response_id: request.previous_response_id.as_deref(),
            input: &request.input,
            tools: request.tools.clone().unwrap_or_default(),
            tool_choice: request.tool_choice.clone().unwrap_or_else(|| json!("auto")),
            parallel_tool_calls: request.parallel_tool_calls.unwrap_or(true),
            reasoning: request.reasoning.as_ref(),
            store: request.store,
            stream: request.stream,
            include: request.include.clone().unwrap_or_default(),
            service_tier: request.service_tier.as_deref(),
            prompt_cache_key: request.prompt_cache_key.as_deref(),
            text: request.text.as_ref(),
            generate: request.generate,
            client_metadata: request.client_metadata.as_ref(),
        })
    }
}

pub(super) fn websocket_request_text(
    request: &CodexResponsesRequest,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&ResponsesWsRequest::from_request(request))
}

pub(super) fn websocket_request_body(request: &CodexResponsesRequest) -> Value {
    serde_json::to_value(ResponsesWsRequest::from_request(request)).unwrap_or(Value::Null)
}

pub fn websocket_payload_audit_snapshot(request: &CodexResponsesRequest) -> PayloadAuditSnapshot {
    let body = websocket_request_body(request);
    websocket_payload_audit_snapshot_from_body_and_keys(&body, websocket_payload_keys(request))
}

pub(super) fn websocket_payload_audit_snapshot_from_request_body(
    request: &CodexResponsesRequest,
    body: &Value,
) -> PayloadAuditSnapshot {
    websocket_payload_audit_snapshot_from_body_and_keys(body, websocket_payload_keys(request))
}

fn websocket_payload_audit_snapshot_from_body_and_keys(
    body: &Value,
    top_level_keys: Vec<String>,
) -> PayloadAuditSnapshot {
    PayloadAuditSnapshot {
        top_level_keys,
        body: redact_payload_body(body.clone()),
    }
}

fn websocket_payload_keys(request: &CodexResponsesRequest) -> Vec<String> {
    let mut keys = Vec::from(["type".to_string(), "model".to_string()]);
    if !request.instructions.is_empty() {
        keys.push("instructions".to_string());
    }
    if request.previous_response_id.is_some() {
        keys.push("previous_response_id".to_string());
    }
    keys.extend(
        [
            "input",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "reasoning",
            "store",
            "stream",
            "include",
        ]
        .into_iter()
        .map(ToString::to_string),
    );
    if request.service_tier.is_some() {
        keys.push("service_tier".to_string());
    }
    if request.prompt_cache_key.is_some() {
        keys.push("prompt_cache_key".to_string());
    }
    if request.text.is_some() {
        keys.push("text".to_string());
    }
    if request.generate.is_some() {
        keys.push("generate".to_string());
    }
    if request.client_metadata.is_some() {
        keys.push("client_metadata".to_string());
    }
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
                    redact_nested_payload_value(value)
                };
                (key, value)
            })
            .collect::<Map<_, _>>(),
    )
}

fn redact_nested_payload_value(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(redact_nested_payload_value)
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, redact_nested_payload_value(value)))
                .collect(),
        ),
        value => value,
    }
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

pub(super) fn websocket_message_text(
    message: Message,
) -> Result<Option<String>, CodexWebSocketError> {
    match message {
        Message::Text(text) => Ok(Some(text.to_string())),
        Message::Binary(_) => Err(CodexWebSocketError::UnexpectedBinaryEvent),
        Message::Close(_) => Err(CodexWebSocketError::ClosedByServerBeforeCompleted),
        _ => Ok(None),
    }
}

pub(super) fn websocket_event_type(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw).ok().and_then(|value| {
        value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

pub(super) fn metadata_turn_state(raw: &str) -> Option<String> {
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
}

fn json_value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Array(items) => items.first().and_then(json_value_as_string),
        _ => None,
    }
}

fn json_field_absent_or_null(value: &Value, field: &str) -> bool {
    matches!(value.get(field), None | Some(Value::Null))
}

pub(super) fn incomplete_response_reason(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("response.incomplete") {
        return None;
    }
    Some(
        value
            .pointer("/response/incomplete_details/reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    )
}

pub(super) fn response_completed_parse_error(raw: &str) -> Option<String> {
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

pub(super) fn responses_stream_event_shape_parse_error(raw: &str) -> bool {
    serde_json::from_str::<ResponsesStreamEventShape>(raw).is_err()
}

pub(super) fn response_completed_missing_response(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.completed")
        && json_field_absent_or_null(&value, "response")
}

pub(super) fn response_created_missing_response(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.created")
        && json_field_absent_or_null(&value, "response")
}

pub(super) fn response_output_text_delta_missing_delta(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
        && json_field_absent_or_null(&value, "delta")
}

pub(super) fn delta_event_missing_official_required_fields(raw: &str) -> bool {
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

pub(super) fn output_item_event_missing_item(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) && json_field_absent_or_null(&value, "item")
}

pub(super) fn output_item_event_non_object_item(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) && value
        .get("item")
        .is_some_and(|item| !item.is_null() && !item.is_object())
}

pub(super) fn output_item_event_invalid_item_type_tag(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| item.get("type").and_then(Value::as_str).is_none())
}

pub(super) fn output_item_event_invalid_metadata(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn message_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

fn optional_message_phase_invalid(item: &serde_json::Map<String, Value>) -> bool {
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

pub(super) fn agent_message_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn reasoning_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn function_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn function_call_output_payload_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn custom_tool_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn custom_tool_call_output_payload_item_event_invalid_required_fields(
    raw: &str,
) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn tool_search_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn tool_search_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn local_shell_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn web_search_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

fn valid_local_shell_status(status: Option<&Value>) -> bool {
    matches!(
        status.and_then(Value::as_str),
        Some("completed" | "in_progress" | "incomplete")
    )
}

fn local_shell_action_invalid_required_fields(action: &serde_json::Map<String, Value>) -> bool {
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

pub(super) fn image_generation_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

fn optional_string_field_invalid(item: &serde_json::Map<String, Value>, field: &str) -> bool {
    item.get(field)
        .is_some_and(|value| !value.is_null() && !value.is_string())
}

fn optional_string_array_field_invalid(item: &serde_json::Map<String, Value>, field: &str) -> bool {
    item.get(field).is_some_and(|value| {
        !value.is_null()
            && value
                .as_array()
                .is_none_or(|items| items.iter().any(|item| !item.is_string()))
    })
}

fn optional_u64_field_invalid(item: &serde_json::Map<String, Value>, field: &str) -> bool {
    item.get(field)
        .is_some_and(|value| !value.is_null() && value.as_u64().is_none())
}

fn optional_string_map_field_invalid(item: &serde_json::Map<String, Value>, field: &str) -> bool {
    item.get(field).is_some_and(|value| {
        !value.is_null()
            && value
                .as_object()
                .is_none_or(|object| object.values().any(|value| !value.is_string()))
    })
}

pub(super) fn compaction_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    ) {
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

pub(super) fn reasoning_summary_part_added_missing_summary_index(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.reasoning_summary_part.added")
        && value.get("summary_index").and_then(Value::as_i64).is_none()
}

pub(super) fn websocket_sse_chunk(raw: &str, event: &str) -> String {
    encode_sse_event(event, raw)
}

pub(super) fn is_internal_websocket_event(raw: &str) -> bool {
    websocket_event_type(raw).as_deref() == Some("codex.rate_limits")
}

pub(super) fn is_terminal_websocket_event(event: &str) -> bool {
    event == "response.completed" || event == "response.failed" || event == "error"
}

pub(super) fn classify_ws_error_frame(
    raw: &str,
    profile: WebSocketErrorClassificationProfile,
) -> Option<ClassifiedWebSocketError> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let event_type = value.get("type").and_then(Value::as_str)?;
    if event_type != "error" && event_type != "response.failed" {
        return None;
    }
    let code = value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/response/error/type"))
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.pointer("/error/type"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    if code.as_deref() == Some("websocket_connection_limit_reached") {
        return Some(ClassifiedWebSocketError {
            status: StatusCode::SERVICE_UNAVAILABLE,
        });
    }
    if event_type == "error" {
        if let Some(status) = value
            .get("status")
            .or_else(|| value.get("status_code"))
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .and_then(|status| StatusCode::from_u16(status).ok())
        {
            if status.is_success() {
                return None;
            }
            return Some(ClassifiedWebSocketError { status });
        }
    }
    if let Some(code) = code {
        if let Some(status) = rotatable_error_status(&code, profile) {
            return Some(ClassifiedWebSocketError { status });
        }
    }
    if event_type == "response.failed" {
        return Some(ClassifiedWebSocketError {
            status: StatusCode::SERVICE_UNAVAILABLE,
        });
    }
    None
}

pub(super) fn codex_websocket_transport_error(error: WsError) -> CodexWebSocketError {
    match error {
        WsError::Http(response) => {
            let (parts, body) = (*response).into_parts();
            let body = body
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_default();
            CodexWebSocketError::Upstream {
                status: parts.status,
                retry_after_seconds: retry_after_seconds(&parts.headers)
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
            }
        }
        error => CodexWebSocketError::Transport(error),
    }
}

pub(super) fn turn_state(headers: &WsHeaderMap) -> Option<String> {
    headers
        .get("x-codex-turn-state")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

pub(super) fn set_cookie_headers(headers: &WsHeaderMap) -> Vec<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToString::to_string))
        .collect()
}

pub(super) fn rate_limit_headers(headers: &WsHeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| is_rate_limit_header(name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

pub(super) fn retry_after_seconds_from_wrapped_error_headers(raw: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("error") {
        return None;
    }
    value
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

fn rotatable_error_status(
    code: &str,
    profile: WebSocketErrorClassificationProfile,
) -> Option<StatusCode> {
    match code {
        "usage_limit_reached" | "rate_limit_exceeded" | "rate_limit_reached" => {
            Some(StatusCode::TOO_MANY_REQUESTS)
        }
        "quota_exhausted" | "quota_exceeded" | "insufficient_quota" | "payment_required" => {
            Some(StatusCode::PAYMENT_REQUIRED)
        }
        "usage_not_included" => Some(StatusCode::TOO_MANY_REQUESTS),
        "unauthorized" | "token_invalid" | "token_expired" | "account_deactivated" => {
            Some(StatusCode::UNAUTHORIZED)
        }
        "forbidden" | "account_banned" | "banned" => Some(StatusCode::FORBIDDEN),
        "context_length_exceeded" | "invalid_prompt" | "cyber_policy" | "invalid_request" => {
            Some(StatusCode::BAD_REQUEST)
        }
        "previous_response_not_found" => Some(StatusCode::BAD_REQUEST),
        "server_is_overloaded" | "slow_down" => Some(StatusCode::SERVICE_UNAVAILABLE),
        "websocket_connection_limit_reached"
            if profile == WebSocketErrorClassificationProfile::Pooled =>
        {
            Some(StatusCode::SERVICE_UNAVAILABLE)
        }
        _ => None,
    }
}

fn is_rate_limit_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "retry-after"
        || name.contains("ratelimit")
        || name.contains("rate-limit")
        || name.starts_with("x-codex-primary-")
        || name.starts_with("x-codex-secondary-")
        || name.starts_with("x-codex-code-review-")
        || name.starts_with("x-codex-review-")
        || name.starts_with("x-code-review-")
}

fn retry_after_seconds(headers: &WsHeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
}
