use std::{pin::Pin, sync::Arc, time::Instant};

use thiserror::Error;
use tokio::sync::Mutex;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest, http::Request as WsRequest, Error as WsError, Message,
    },
};

use futures::{channel::mpsc, SinkExt, Stream, StreamExt};
use reqwest::{header::HeaderMap, StatusCode};

use crate::codex::gateway::transport::{
    rate_limits::{parse_rate_limits_event_raw, rate_limits_to_header_pairs, ParsedRateLimits},
    types::CodexResponsesRequest,
};

mod codec;
mod pool;

use codec::{
    classify_ws_error_frame, codex_websocket_transport_error, is_internal_websocket_event,
    is_terminal_websocket_event, retry_after_seconds_from_body, websocket_event_type,
    websocket_message_text, websocket_request_body, websocket_sse_chunk,
};
use pool::{
    CodexWebSocketConnectionMetadata, CodexWsStream, PooledWebSocketConnection,
    WebSocketPoolAcquire,
};
pub use pool::{CodexWebSocketPool, CodexWebSocketPoolConfig, CodexWebSocketPoolKey};

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
    #[error("websocket closed before terminal event")]
    ClosedBeforeTerminal,
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
pub type SharedRateLimitUpdates = Arc<Mutex<Vec<ParsedRateLimits>>>;

pub struct CodexWebSocketStreamResponse {
    pub body_stream: CodexWebSocketSseStream,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
    pub rate_limit_updates: SharedRateLimitUpdates,
}

struct ActiveWebSocket {
    websocket: CodexWsStream,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketPoolReturn>,
    reused: bool,
    last_activity_at: Instant,
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
            last_activity_at,
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
                    last_activity_at,
                    last_ping_at: None,
                },
            )
            .await;
    }

    async fn discard(self) {
        let Self {
            mut websocket,
            pool_return,
            ..
        } = self;
        if let Some(pool_return) = pool_return {
            pool_return.pool.discard(&pool_return.key).await;
        }
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

#[tracing::instrument(
    skip(base_url, request, headers),
    fields(
        model = %request.model,
        has_previous_response_id = request.previous_response_id.is_some(),
    )
)]
pub async fn create_response_via_websocket(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
) -> Result<CodexWebSocketResponse, CodexWebSocketError> {
    let CodexWebSocketStreamResponse {
        mut body_stream,
        turn_state,
        set_cookie_headers,
        mut rate_limit_headers,
        rate_limit_updates,
    } = create_response_via_websocket_stream(base_url, request, headers).await?;

    let mut body = String::new();
    while let Some(chunk) = body_stream.next().await {
        body.push_str(&chunk?);
    }
    if body.is_empty() {
        return Err(CodexWebSocketError::EmptyResponse);
    }
    append_rate_limit_updates(&mut rate_limit_headers, &rate_limit_updates).await;

    Ok(CodexWebSocketResponse {
        body,
        turn_state,
        set_cookie_headers,
        rate_limit_headers,
    })
}

#[tracing::instrument(
    skip(base_url, request, headers),
    fields(
        model = %request.model,
        has_previous_response_id = request.previous_response_id.is_some(),
    )
)]
pub async fn create_response_via_websocket_stream(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError> {
    create_response_via_websocket_stream_inner(base_url, request, headers, None).await
}

#[tracing::instrument(
    skip(base_url, request, headers, pool, pool_key),
    fields(
        model = %request.model,
        has_previous_response_id = request.previous_response_id.is_some(),
    )
)]
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
    let mut pool_context = pool;
    let mut retry_stale_reuse = pool_context.is_some();
    let rate_limit_updates = Arc::new(Mutex::new(Vec::new()));
    loop {
        let mut active = acquire_websocket(base_url, headers.clone(), pool_context.clone()).await?;
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
                pool_context = None;
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
                    pool_context = None;
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
                        pool_context = None;
                        break;
                    }
                    return Err(CodexWebSocketError::Transport(error));
                }
            };
            active.last_activity_at = Instant::now();
            let Some(raw) = websocket_message_text(message) else {
                continue;
            };
            if is_internal_websocket_event(&raw) {
                capture_internal_rate_limit_event(&raw, &rate_limit_updates).await;
                continue;
            }
            if let Some(classified) = classify_ws_error_frame(&raw) {
                if classified.connection_fatal {
                    active.discard().await;
                } else {
                    active.finish().await;
                }
                return Err(CodexWebSocketError::Upstream {
                    status: classified.status,
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
                let rate_limit_updates = rate_limit_updates.clone();
                tokio::spawn(async move {
                    forward_websocket_as_sse(active, tx, rate_limit_updates).await;
                });
            }
            return Ok(CodexWebSocketStreamResponse {
                body_stream: Box::pin(rx),
                turn_state: metadata.turn_state,
                set_cookie_headers: metadata.set_cookie_headers,
                rate_limit_headers: metadata.rate_limit_headers,
                rate_limit_updates,
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
        let created_at = Instant::now();
        let (websocket, metadata) = connect_websocket(base_url, headers).await?;
        return Ok(ActiveWebSocket {
            websocket,
            metadata,
            pool_return: None,
            reused: false,
            last_activity_at: created_at,
        });
    };

    match pool.acquire(&key).await {
        WebSocketPoolAcquire::Reused(connection) => {
            let PooledWebSocketConnection {
                websocket,
                metadata,
                created_at,
                last_activity_at,
                last_ping_at: _,
            } = *connection;
            return Ok(ActiveWebSocket {
                websocket,
                metadata,
                pool_return: Some(WebSocketPoolReturn {
                    pool,
                    key,
                    created_at,
                }),
                reused: true,
                last_activity_at,
            });
        }
        WebSocketPoolAcquire::FreshReserved => {}
        WebSocketPoolAcquire::Bypass => {
            let (websocket, metadata) = connect_websocket(base_url, headers).await?;
            return Ok(ActiveWebSocket {
                websocket,
                metadata,
                pool_return: None,
                reused: false,
                last_activity_at: Instant::now(),
            });
        }
    }

    let created_at = Instant::now();
    let (websocket, metadata) = match connect_websocket(base_url, headers).await {
        Ok(connection) => connection,
        Err(error) => {
            pool.discard(&key).await;
            return Err(error);
        }
    };
    Ok(ActiveWebSocket {
        websocket,
        metadata,
        pool_return: Some(WebSocketPoolReturn {
            pool,
            key,
            created_at,
        }),
        reused: false,
        last_activity_at: created_at,
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

async fn forward_websocket_as_sse(
    mut active: ActiveWebSocket,
    tx: mpsc::UnboundedSender<Result<String, CodexWebSocketError>>,
    rate_limit_updates: SharedRateLimitUpdates,
) {
    while let Some(message) = active.websocket.next().await {
        match message {
            Ok(message) => {
                active.last_activity_at = Instant::now();
                let Some(raw) = websocket_message_text(message) else {
                    continue;
                };
                if is_internal_websocket_event(&raw) {
                    capture_internal_rate_limit_event(&raw, &rate_limit_updates).await;
                    continue;
                }
                let event = websocket_event_type(&raw);
                let terminal = event.as_deref().is_some_and(is_terminal_websocket_event);
                let chunk = websocket_sse_chunk(&raw, event.as_deref());
                if tx.unbounded_send(Ok(chunk)).is_err() {
                    active.discard().await;
                    return;
                }
                if terminal {
                    active.finish().await;
                    return;
                }
            }
            Err(error) => {
                let _ = tx.unbounded_send(Err(CodexWebSocketError::Transport(error)));
                active.discard().await;
                return;
            }
        }
    }
    let _ = tx.unbounded_send(Err(CodexWebSocketError::ClosedBeforeTerminal));
    active.discard().await;
}

async fn capture_internal_rate_limit_event(raw: &str, updates: &SharedRateLimitUpdates) {
    if let Some(parsed) = parse_rate_limits_event_raw(raw) {
        updates.lock().await.push(parsed);
    }
}

pub async fn append_rate_limit_updates(
    headers: &mut Vec<(String, String)>,
    updates: &SharedRateLimitUpdates,
) {
    let updates = updates.lock().await;
    for update in updates.iter() {
        headers.extend(rate_limits_to_header_pairs(update));
    }
}
