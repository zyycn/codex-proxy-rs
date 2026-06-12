use thiserror::Error;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{HeaderMap as WsHeaderMap, Request as WsRequest},
        Error as WsError, Message,
    },
};

use futures::{SinkExt, StreamExt};
use reqwest::{header::HeaderMap, StatusCode};
use serde_json::{json, Value};

use crate::codex::{sse::encode_sse_event, types::CodexResponsesRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexTransport {
    HttpSse,
    WebSocketRequired,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebSocketSupportError {
    #[error("previous_response_id requires Codex WebSocket transport")]
    PreviousResponseRequiresWebSocket,
    #[error("request explicitly requires Codex WebSocket transport")]
    ExplicitWebSocketRequired,
}

#[derive(Debug, Error)]
pub enum CodexWebSocketError {
    #[error("invalid WebSocket request: {0}")]
    InvalidRequest(#[from] tokio_tungstenite::tungstenite::http::Error),
    #[error("websocket transport error: {0}")]
    Transport(#[source] WsError),
    #[error("websocket handshake returned status {status}: {body}")]
    Upstream {
        status: StatusCode,
        body: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("websocket response ended before any events")]
    EmptyResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketResponse {
    pub body: String,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
}

pub fn transport_for_request(request: &CodexResponsesRequest) -> CodexTransport {
    if request.previous_response_id.is_some() || request.use_websocket {
        CodexTransport::WebSocketRequired
    } else {
        CodexTransport::HttpSse
    }
}

pub fn ensure_http_sse_supported(
    request: &CodexResponsesRequest,
) -> Result<(), WebSocketSupportError> {
    if request.previous_response_id.is_some() {
        return Err(WebSocketSupportError::PreviousResponseRequiresWebSocket);
    }
    if request.use_websocket {
        return Err(WebSocketSupportError::ExplicitWebSocketRequired);
    }
    Ok(())
}

pub async fn create_response_via_websocket(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
) -> Result<CodexWebSocketResponse, CodexWebSocketError> {
    let ws_request = build_ws_request(base_url, headers)?;
    let (mut websocket, handshake_response) = connect_async(ws_request)
        .await
        .map_err(codex_websocket_transport_error)?;
    websocket
        .send(Message::Text(
            websocket_request_body(request).to_string().into(),
        ))
        .await
        .map_err(CodexWebSocketError::Transport)?;

    let mut body = String::new();
    while let Some(message) = websocket.next().await {
        let message = message.map_err(CodexWebSocketError::Transport)?;
        let Some(raw) = websocket_message_text(message) else {
            continue;
        };
        let event = websocket_event_type(&raw);
        body.push_str(&encode_sse_event(
            event.as_deref().unwrap_or_default(),
            &raw,
        ));
        if event.as_deref().is_some_and(is_terminal_websocket_event) {
            break;
        }
    }
    if body.is_empty() {
        return Err(CodexWebSocketError::EmptyResponse);
    }
    let headers = handshake_response.headers();
    Ok(CodexWebSocketResponse {
        body,
        turn_state: turn_state(headers),
        set_cookie_headers: set_cookie_headers(headers),
        rate_limit_headers: rate_limit_headers(headers),
    })
}

fn build_ws_request(
    base_url: &str,
    headers: HeaderMap,
) -> Result<WsRequest<()>, CodexWebSocketError> {
    let mut request = websocket_url(base_url)
        .into_client_request()
        .map_err(codex_websocket_transport_error)?;
    for (name, value) in &headers {
        let Ok(name) =
            tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_str().as_bytes())
        else {
            continue;
        };
        let Ok(value) =
            tokio_tungstenite::tungstenite::http::HeaderValue::from_bytes(value.as_bytes())
        else {
            continue;
        };
        request.headers_mut().insert(name, value);
    }
    Ok(request)
}

fn websocket_url(base_url: &str) -> String {
    let url = format!("{}/codex/responses", base_url.trim_end_matches('/'));
    if let Some(rest) = url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        url
    }
}

fn websocket_request_body(request: &CodexResponsesRequest) -> Value {
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

fn websocket_message_text(message: Message) -> Option<String> {
    match message {
        Message::Text(text) => Some(text.to_string()),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
        _ => None,
    }
}

fn websocket_event_type(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw).ok().and_then(|value| {
        value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn is_terminal_websocket_event(event: &str) -> bool {
    event == "response.completed" || event == "response.failed" || event == "error"
}

fn codex_websocket_transport_error(error: WsError) -> CodexWebSocketError {
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

fn turn_state(headers: &WsHeaderMap) -> Option<String> {
    headers
        .get("x-codex-turn-state")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn set_cookie_headers(headers: &WsHeaderMap) -> Vec<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToString::to_string))
        .collect()
}

fn rate_limit_headers(headers: &WsHeaderMap) -> Vec<(String, String)> {
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

fn is_rate_limit_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "retry-after" || name.contains("ratelimit") || name.contains("rate-limit")
}

fn retry_after_seconds(headers: &WsHeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
}

fn retry_after_seconds_from_body(body: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
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
