use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use thiserror::Error;
use tokio::{net::TcpStream, sync::Mutex};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{HeaderMap as WsHeaderMap, Request as WsRequest},
        Error as WsError, Message,
    },
    MaybeTlsStream, WebSocketStream,
};

use futures::{channel::mpsc, SinkExt, Stream, StreamExt};
use reqwest::{header::HeaderMap, StatusCode};
use serde_json::{json, Value};

use crate::codex::gateway::transport::{sse::encode_sse_event, types::CodexResponsesRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexTransport {
    HttpSse,
    WebSocketPreferred,
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

pub type CodexWebSocketSseStream =
    Pin<Box<dyn Stream<Item = Result<String, CodexWebSocketError>> + Send>>;

pub struct CodexWebSocketStreamResponse {
    pub body_stream: CodexWebSocketSseStream,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodexWebSocketPoolKey {
    base_url: String,
    account_id: String,
    conversation_id: String,
}

impl CodexWebSocketPoolKey {
    pub fn new(
        base_url: impl Into<String>,
        account_id: impl Into<String>,
        conversation_id: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            account_id: account_id.into(),
            conversation_id: conversation_id.into(),
        }
    }

    fn account_id(&self) -> &str {
        &self.account_id
    }
}

#[derive(Clone)]
pub struct CodexWebSocketPool {
    inner: Arc<Mutex<HashMap<CodexWebSocketPoolKey, PooledWebSocketConnection>>>,
    max_age: Duration,
}

impl CodexWebSocketPool {
    const DEFAULT_MAX_AGE: Duration = Duration::from_secs(55 * 60);

    pub fn with_default_max_age() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_age: Self::DEFAULT_MAX_AGE,
        }
    }

    pub async fn evict_account(&self, account_id: &str) {
        self.inner
            .lock()
            .await
            .retain(|key, _| key.account_id() != account_id);
    }

    async fn take(&self, key: &CodexWebSocketPoolKey) -> Option<PooledWebSocketConnection> {
        let connection = self.inner.lock().await.remove(key)?;
        if connection.created_at.elapsed() <= self.max_age {
            Some(connection)
        } else {
            None
        }
    }

    async fn put(&self, key: CodexWebSocketPoolKey, connection: PooledWebSocketConnection) {
        if connection.created_at.elapsed() > self.max_age {
            return;
        }
        self.inner.lock().await.insert(key, connection);
    }
}

impl Default for CodexWebSocketPool {
    fn default() -> Self {
        Self::with_default_max_age()
    }
}

type CodexWsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexWebSocketConnectionMetadata {
    turn_state: Option<String>,
    set_cookie_headers: Vec<String>,
    rate_limit_headers: Vec<(String, String)>,
}

impl CodexWebSocketConnectionMetadata {
    fn from_headers(headers: &WsHeaderMap) -> Self {
        Self {
            turn_state: turn_state(headers),
            set_cookie_headers: set_cookie_headers(headers),
            rate_limit_headers: rate_limit_headers(headers),
        }
    }
}

struct PooledWebSocketConnection {
    websocket: CodexWsStream,
    metadata: CodexWebSocketConnectionMetadata,
    created_at: Instant,
}

struct ActiveWebSocket {
    websocket: CodexWsStream,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketPoolReturn>,
    reused: bool,
}

struct WebSocketPoolReturn {
    pool: Arc<CodexWebSocketPool>,
    key: CodexWebSocketPoolKey,
    created_at: Instant,
}

impl ActiveWebSocket {
    async fn finish(self) {
        let Self {
            websocket,
            metadata,
            pool_return,
            reused: _,
        } = self;
        let Some(pool_return) = pool_return else {
            let mut websocket = websocket;
            let _ = websocket.close(None).await;
            return;
        };
        pool_return
            .pool
            .put(
                pool_return.key,
                PooledWebSocketConnection {
                    websocket,
                    metadata,
                    created_at: pool_return.created_at,
                },
            )
            .await;
    }

    async fn discard(self) {
        let mut websocket = self.websocket;
        let _ = websocket.close(None).await;
    }
}

pub fn transport_for_request(request: &CodexResponsesRequest) -> CodexTransport {
    if request.previous_response_id.is_some() {
        CodexTransport::WebSocketRequired
    } else if request.force_http_sse {
        CodexTransport::HttpSse
    } else {
        CodexTransport::WebSocketPreferred
    }
}

pub fn http_sse_fallback_allowed(request: &CodexResponsesRequest) -> bool {
    matches!(
        transport_for_request(request),
        CodexTransport::WebSocketPreferred | CodexTransport::HttpSse
    )
}

pub async fn create_response_via_websocket(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
) -> Result<CodexWebSocketResponse, CodexWebSocketError> {
    let CodexWebSocketStreamResponse {
        mut body_stream,
        turn_state,
        set_cookie_headers,
        rate_limit_headers,
    } = create_response_via_websocket_stream(base_url, request, headers).await?;

    let mut body = String::new();
    while let Some(chunk) = body_stream.next().await {
        body.push_str(&chunk?);
    }
    if body.is_empty() {
        return Err(CodexWebSocketError::EmptyResponse);
    }

    Ok(CodexWebSocketResponse {
        body,
        turn_state,
        set_cookie_headers,
        rate_limit_headers,
    })
}

pub async fn create_response_via_websocket_stream(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError> {
    create_response_via_websocket_stream_inner(base_url, request, headers, None).await
}

pub async fn create_response_via_websocket_stream_with_pool(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
    pool: Arc<CodexWebSocketPool>,
    pool_key: CodexWebSocketPoolKey,
) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError> {
    create_response_via_websocket_stream_inner(base_url, request, headers, Some((pool, pool_key)))
        .await
}

async fn create_response_via_websocket_stream_inner(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
    pool: Option<(Arc<CodexWebSocketPool>, CodexWebSocketPoolKey)>,
) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError> {
    let mut retry_stale_reuse = pool.is_some();
    loop {
        let mut active = acquire_websocket(base_url, headers.clone(), pool.clone()).await?;
        if let Err(error) = active
            .websocket
            .send(Message::Text(
                websocket_request_body(request).to_string().into(),
            ))
            .await
        {
            let reused = active.reused;
            active.discard().await;
            if reused && retry_stale_reuse {
                retry_stale_reuse = false;
                continue;
            }
            return Err(CodexWebSocketError::Transport(error));
        }
        let metadata = active.metadata.clone();

        loop {
            let Some(message) = active.websocket.next().await else {
                let reused = active.reused;
                active.discard().await;
                if reused && retry_stale_reuse {
                    retry_stale_reuse = false;
                    break;
                }
                return Err(CodexWebSocketError::EmptyResponse);
            };
            let message = match message {
                Ok(message) => message,
                Err(error) => {
                    let reused = active.reused;
                    active.discard().await;
                    if reused && retry_stale_reuse {
                        retry_stale_reuse = false;
                        break;
                    }
                    return Err(CodexWebSocketError::Transport(error));
                }
            };
            let Some(raw) = websocket_message_text(message) else {
                continue;
            };
            if is_internal_websocket_event(&raw) {
                continue;
            }
            if let Some(status) = classify_ws_error_frame(&raw) {
                active.finish().await;
                return Err(CodexWebSocketError::Upstream {
                    status,
                    retry_after_seconds: retry_after_seconds_from_body(&raw),
                    body: raw,
                });
            }

            let first_event = websocket_event_type(&raw);
            let first_chunk = websocket_sse_chunk(&raw, first_event.as_deref());
            let terminal = first_event
                .as_deref()
                .is_some_and(is_terminal_websocket_event);
            let (tx, rx) = mpsc::unbounded();
            if tx.unbounded_send(Ok(first_chunk)).is_err() {
                active.discard().await;
                return Err(CodexWebSocketError::EmptyResponse);
            }
            if terminal {
                active.finish().await;
            } else {
                tokio::spawn(async move {
                    forward_websocket_as_sse(active, tx).await;
                });
            }
            return Ok(CodexWebSocketStreamResponse {
                body_stream: Box::pin(rx),
                turn_state: metadata.turn_state,
                set_cookie_headers: metadata.set_cookie_headers,
                rate_limit_headers: metadata.rate_limit_headers,
            });
        }
    }
}

async fn acquire_websocket(
    base_url: &str,
    headers: HeaderMap,
    pool: Option<(Arc<CodexWebSocketPool>, CodexWebSocketPoolKey)>,
) -> Result<ActiveWebSocket, CodexWebSocketError> {
    let Some((pool, key)) = pool else {
        let (websocket, metadata) = connect_websocket(base_url, headers).await?;
        return Ok(ActiveWebSocket {
            websocket,
            metadata,
            pool_return: None,
            reused: false,
        });
    };

    if let Some(connection) = pool.take(&key).await {
        return Ok(ActiveWebSocket {
            websocket: connection.websocket,
            metadata: connection.metadata,
            pool_return: Some(WebSocketPoolReturn {
                pool,
                key,
                created_at: connection.created_at,
            }),
            reused: true,
        });
    }

    let created_at = Instant::now();
    let (websocket, metadata) = connect_websocket(base_url, headers).await?;
    Ok(ActiveWebSocket {
        websocket,
        metadata,
        pool_return: Some(WebSocketPoolReturn {
            pool,
            key,
            created_at,
        }),
        reused: false,
    })
}

async fn connect_websocket(
    base_url: &str,
    headers: HeaderMap,
) -> Result<(CodexWsStream, CodexWebSocketConnectionMetadata), CodexWebSocketError> {
    let ws_request = build_ws_request(base_url, headers)?;
    let (websocket, handshake_response) = connect_async(ws_request)
        .await
        .map_err(codex_websocket_transport_error)?;
    Ok((
        websocket,
        CodexWebSocketConnectionMetadata::from_headers(handshake_response.headers()),
    ))
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

fn websocket_sse_chunk(raw: &str, event: Option<&str>) -> String {
    encode_sse_event(event.unwrap_or_default(), raw)
}

fn is_internal_websocket_event(raw: &str) -> bool {
    websocket_event_type(raw).as_deref() == Some("codex.rate_limits")
}

fn is_terminal_websocket_event(event: &str) -> bool {
    event == "response.completed" || event == "response.failed" || event == "error"
}

async fn forward_websocket_as_sse(
    mut active: ActiveWebSocket,
    tx: mpsc::UnboundedSender<Result<String, CodexWebSocketError>>,
) {
    while let Some(message) = active.websocket.next().await {
        match message {
            Ok(message) => {
                let Some(raw) = websocket_message_text(message) else {
                    continue;
                };
                if is_internal_websocket_event(&raw) {
                    continue;
                }
                let event = websocket_event_type(&raw);
                let terminal = event.as_deref().is_some_and(is_terminal_websocket_event);
                let chunk = websocket_sse_chunk(&raw, event.as_deref());
                if tx.unbounded_send(Ok(chunk)).is_err() {
                    active.discard().await;
                    break;
                }
                if terminal {
                    active.finish().await;
                    break;
                }
            }
            Err(error) => {
                let _ = tx.unbounded_send(Err(CodexWebSocketError::Transport(error)));
                break;
            }
        }
    }
}

fn classify_ws_error_frame(raw: &str) -> Option<StatusCode> {
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
    rotatable_error_status(&code)
}

fn rotatable_error_status(code: &str) -> Option<StatusCode> {
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
        _ => None,
    }
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
