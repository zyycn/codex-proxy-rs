use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{http::HeaderMap as WsHeaderMap, Error as WsError, Message};

use crate::codex::gateway::transport::{sse::encode_sse_event, types::CodexResponsesRequest};

use super::CodexWebSocketError;

pub(super) struct ClassifiedWebSocketError {
    pub(super) status: StatusCode,
    pub(super) connection_fatal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WebSocketErrorClassificationProfile {
    OneShot,
    Pooled,
}

pub(super) fn websocket_request_body(request: &CodexResponsesRequest) -> Value {
    let mut body = json!({
        "type": "response.create",
        "model": request.model,
        "instructions": request.instructions,
        "input": request.input,
        "stream": true,
        "store": false,
        "tool_choice": request.tool_choice.clone().unwrap_or_else(|| json!("auto")),
        "parallel_tool_calls": request.parallel_tool_calls.unwrap_or(true),
    });
    if let Some(previous_response_id) = &request.previous_response_id {
        body["previous_response_id"] = Value::String(previous_response_id.clone());
    }
    if let Some(reasoning) = &request.reasoning {
        body["reasoning"] = reasoning.clone();
    }
    if let Some(tools) = request.tools.as_ref().filter(|tools| !tools.is_empty()) {
        body["tools"] = Value::Array(tools.clone());
    }
    if let Some(text) = &request.text {
        body["text"] = text.clone();
    }
    if let Some(service_tier) = &request.service_tier {
        body["service_tier"] = Value::String(service_tier.clone());
    }
    if let Some(prompt_cache_key) = &request.prompt_cache_key {
        body["prompt_cache_key"] = Value::String(prompt_cache_key.clone());
    }
    if let Some(include) = request
        .include
        .as_ref()
        .filter(|include| !include.is_empty())
    {
        body["include"] = Value::Array(include.iter().cloned().map(Value::String).collect());
    }
    if let Some(client_metadata) = &request.client_metadata {
        body["client_metadata"] = client_metadata.clone();
    }
    body
}

pub(super) fn websocket_message_text(message: Message) -> Option<String> {
    match message {
        Message::Text(text) => Some(text.to_string()),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
        _ => None,
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

pub(super) fn websocket_sse_chunk(raw: &str, event: Option<&str>) -> String {
    encode_sse_event(event.unwrap_or_default(), raw)
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
        .and_then(Value::as_str)?
        .to_ascii_lowercase();
    let status = rotatable_error_status(&code, profile)?;
    Some(ClassifiedWebSocketError {
        status,
        connection_fatal: profile == WebSocketErrorClassificationProfile::Pooled
            && code == "websocket_connection_limit_reached",
    })
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

pub(super) fn retry_after_seconds_from_body(body: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let error = value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
        .unwrap_or(&value);
    if let Some(seconds) = error
        .get("resets_in_seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0)
    {
        return Some(seconds);
    }
    let resets_at = error.get("resets_at").and_then(Value::as_u64)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    (resets_at > now).then_some(resets_at - now)
}

fn rotatable_error_status(
    code: &str,
    profile: WebSocketErrorClassificationProfile,
) -> Option<StatusCode> {
    match code {
        "usage_limit_reached" | "rate_limit_exceeded" | "rate_limit_reached" => {
            Some(StatusCode::TOO_MANY_REQUESTS)
        }
        "quota_exhausted" | "payment_required" => Some(StatusCode::PAYMENT_REQUIRED),
        "unauthorized" | "token_invalid" | "token_expired" | "account_deactivated" => {
            Some(StatusCode::UNAUTHORIZED)
        }
        "forbidden" | "account_banned" | "banned" => Some(StatusCode::FORBIDDEN),
        "previous_response_not_found" => Some(StatusCode::BAD_REQUEST),
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
