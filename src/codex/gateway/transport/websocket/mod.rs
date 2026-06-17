use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use thiserror::Error;
use tokio::{sync::Mutex, time::timeout};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, http::Request as WsRequest, Error as WsError, Message,
};

use futures::{channel::mpsc, SinkExt, Stream, StreamExt};
use reqwest::{header::HeaderMap, StatusCode};

use crate::codex::gateway::transport::{
    endpoints::{endpoint_url, CODEX_RESPONSES_PATH},
    rate_limits::{parse_rate_limits_event_raw, rate_limits_to_header_pairs, ParsedRateLimits},
    retry_after::retry_after_seconds_from_body,
    types::CodexResponsesRequest,
};

mod audit;
mod codec;
mod deflate;
mod opening;
mod pool;

pub use audit::{
    websocket_parity_diff, write_websocket_audit_artifact_for_dir, WebSocketAuditArtifact,
    WebSocketAuditErrorSnapshot, WebSocketParityDiff, WebSocketParityDifference, WS_AUDIT_DIR_ENV,
};
use codec::{
    agent_message_output_item_event_invalid_required_fields, classify_ws_error_frame,
    codex_websocket_transport_error, compaction_output_item_event_invalid_required_fields,
    custom_tool_call_output_item_event_invalid_required_fields,
    custom_tool_call_output_payload_item_event_invalid_required_fields,
    delta_event_missing_official_required_fields,
    function_call_output_item_event_invalid_required_fields,
    function_call_output_payload_item_event_invalid_required_fields,
    image_generation_call_output_item_event_invalid_required_fields, incomplete_response_reason,
    is_internal_websocket_event, is_terminal_websocket_event,
    local_shell_call_output_item_event_invalid_required_fields,
    message_output_item_event_invalid_required_fields, metadata_turn_state,
    output_item_event_invalid_item_type_tag, output_item_event_invalid_metadata,
    output_item_event_missing_item, output_item_event_non_object_item,
    reasoning_output_item_event_invalid_required_fields,
    reasoning_summary_part_added_missing_summary_index, response_completed_missing_response,
    response_completed_parse_error, response_created_missing_response,
    response_output_text_delta_missing_delta, responses_stream_event_shape_parse_error,
    retry_after_seconds_from_wrapped_error_headers,
    tool_search_call_output_item_event_invalid_required_fields,
    tool_search_output_item_event_invalid_required_fields,
    web_search_call_output_item_event_invalid_required_fields, websocket_event_type,
    websocket_message_text, websocket_request_body, websocket_request_text, websocket_sse_chunk,
    WebSocketErrorClassificationProfile,
};
pub use codec::{websocket_payload_audit_snapshot, PayloadAuditSnapshot};
pub use opening::{websocket_opening_audit_snapshot, OpeningAuditHeader, OpeningAuditSnapshot};
use pool::{
    CodexWebSocketConnectionMetadata, CodexWsStream, PooledWebSocketConnection,
    WebSocketPoolAcquire,
};
pub use pool::{CodexWebSocketPool, CodexWebSocketPoolConfig, CodexWebSocketPoolKey};

const WEBSOCKET_OPEN_TIMEOUT: Duration = Duration::from_secs(20);
const WEBSOCKET_EVENT_TIMEOUT: Duration = Duration::from_secs(20);

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
    #[error("failed to encode websocket request: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("websocket transport error: {0}")]
    Transport(#[source] WsError),
    #[error("websocket handshake timed out after {timeout:?}")]
    OpenTimeout { timeout: Duration },
    #[error("idle timeout sending websocket request")]
    SendIdleTimeout { timeout: Duration },
    #[error("idle timeout waiting for websocket")]
    ReceiveIdleTimeout { timeout: Duration },
    #[error("websocket handshake returned status {status}: {body}")]
    Upstream {
        status: StatusCode,
        body: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("websocket response ended before any events")]
    EmptyResponse,
    #[error("unexpected binary websocket event")]
    UnexpectedBinaryEvent,
    #[error("websocket closed by server before response.completed")]
    ClosedByServerBeforeCompleted,
    #[error("stream closed before response.completed")]
    StreamClosedBeforeCompleted,
    #[error("Incomplete response returned, reason: {reason}")]
    IncompleteResponse { reason: String },
    #[error("{message}")]
    InvalidCompletedResponse { message: String },
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
pub type SharedTurnState = Arc<Mutex<Option<String>>>;

pub struct CodexWebSocketStreamResponse {
    pub body_stream: CodexWebSocketSseStream,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
    pub rate_limit_updates: SharedRateLimitUpdates,
    pub turn_state_updates: SharedTurnState,
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
    fn error_classification_profile(&self) -> WebSocketErrorClassificationProfile {
        if self.pool_return.is_some() {
            WebSocketErrorClassificationProfile::Pooled
        } else {
            WebSocketErrorClassificationProfile::OneShot
        }
    }

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
        turn_state_updates,
    } = create_response_via_websocket_stream(base_url, request, headers).await?;

    let mut body = String::new();
    while let Some(chunk) = body_stream.next().await {
        body.push_str(&chunk?);
    }
    if body.is_empty() {
        return Err(CodexWebSocketError::EmptyResponse);
    }
    append_rate_limit_updates(&mut rate_limit_headers, &rate_limit_updates).await;
    let turn_state = latest_turn_state(&turn_state_updates).await.or(turn_state);

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
    let turn_state_updates = Arc::new(Mutex::new(None));
    loop {
        let mut active = acquire_websocket(base_url, headers.clone(), pool_context.clone()).await?;
        let metadata = active.metadata.clone();
        set_latest_turn_state(&turn_state_updates, metadata.turn_state.clone()).await;
        let payload = websocket_request_body(request);
        let payload_text = websocket_request_text(request).map_err(CodexWebSocketError::Encode)?;
        audit::record_websocket_audit_attempt_from_env(
            request,
            &active.metadata.opening_audit,
            &payload,
        )
        .await;
        if let Err(error) = timeout_websocket_send(
            active.websocket.send(Message::Text(payload_text.into())),
            WEBSOCKET_EVENT_TIMEOUT,
        )
        .await
        {
            let reused = active.reused;
            active.discard().await;
            if reused && retry_stale_reuse {
                retry_stale_reuse = false;
                pool_context = None;
                continue;
            }
            return Err(error);
        }
        loop {
            let message = match next_websocket_message(&mut active.websocket).await {
                Ok(Some(message)) => message,
                Ok(None) => {
                    let reused = active.reused;
                    active.discard().await;
                    if reused && retry_stale_reuse {
                        retry_stale_reuse = false;
                        pool_context = None;
                        break;
                    }
                    return Err(CodexWebSocketError::StreamClosedBeforeCompleted);
                }
                Err(CodexWebSocketError::Transport(error)) => {
                    let reused = active.reused;
                    active.discard().await;
                    if reused && retry_stale_reuse {
                        retry_stale_reuse = false;
                        pool_context = None;
                        break;
                    }
                    return Err(CodexWebSocketError::Transport(error));
                }
                Err(error) => {
                    active.discard().await;
                    return Err(error);
                }
            };
            active.last_activity_at = Instant::now();
            let raw = match websocket_message_text(message) {
                Ok(Some(raw)) => raw,
                Ok(None) => continue,
                Err(error) => {
                    let reused = active.reused;
                    active.discard().await;
                    if reused && retry_stale_reuse {
                        retry_stale_reuse = false;
                        pool_context = None;
                        break;
                    }
                    return Err(error);
                }
            };
            if is_internal_websocket_event(&raw) {
                capture_internal_rate_limit_event(&raw, &rate_limit_updates).await;
                continue;
            }
            let classification_profile = active.error_classification_profile();
            if let Some(classified) = classify_ws_error_frame(&raw, classification_profile) {
                active.discard().await;
                return Err(CodexWebSocketError::Upstream {
                    status: classified.status,
                    retry_after_seconds: retry_after_seconds_from_wrapped_error_headers(&raw)
                        .or_else(|| retry_after_seconds_from_body(&raw)),
                    body: raw,
                });
            }

            let Some(first_event) = websocket_event_type(&raw) else {
                continue;
            };
            if responses_stream_event_shape_parse_error(&raw) {
                continue;
            }
            if let Some(reason) = incomplete_response_reason(&raw) {
                active.discard().await;
                return Err(CodexWebSocketError::IncompleteResponse { reason });
            }
            if let Some(message) = response_completed_parse_error(&raw) {
                active.discard().await;
                return Err(CodexWebSocketError::InvalidCompletedResponse { message });
            }
            if response_completed_missing_response(&raw) {
                continue;
            }
            if response_created_missing_response(&raw) {
                continue;
            }
            if response_output_text_delta_missing_delta(&raw) {
                continue;
            }
            if delta_event_missing_official_required_fields(&raw) {
                continue;
            }
            if output_item_event_missing_item(&raw) {
                continue;
            }
            if output_item_event_non_object_item(&raw) {
                continue;
            }
            if output_item_event_invalid_item_type_tag(&raw) {
                continue;
            }
            if output_item_event_invalid_metadata(&raw) {
                continue;
            }
            if message_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if agent_message_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if reasoning_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if function_call_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if function_call_output_payload_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if custom_tool_call_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if custom_tool_call_output_payload_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if tool_search_call_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if tool_search_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if local_shell_call_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if web_search_call_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if image_generation_call_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if compaction_output_item_event_invalid_required_fields(&raw) {
                continue;
            }
            if reasoning_summary_part_added_missing_summary_index(&raw) {
                continue;
            }
            if first_event == "response.metadata" {
                capture_metadata_turn_state_event(&raw, &turn_state_updates).await;
                continue;
            }
            if first_event == "error" {
                continue;
            }
            let first_chunk = websocket_sse_chunk(&raw, &first_event);
            let terminal = is_terminal_websocket_event(&first_event);
            let (tx, rx) = mpsc::unbounded();
            if tx.unbounded_send(Ok(first_chunk)).is_err() {
                active.discard().await;
                return Err(CodexWebSocketError::EmptyResponse);
            }
            if terminal {
                active.finish().await;
            } else {
                let rate_limit_updates = rate_limit_updates.clone();
                let turn_state_updates = turn_state_updates.clone();
                tokio::spawn(async move {
                    forward_websocket_as_sse(active, tx, rate_limit_updates, turn_state_updates)
                        .await;
                });
            }
            return Ok(CodexWebSocketStreamResponse {
                body_stream: Box::pin(rx),
                turn_state: metadata.turn_state,
                set_cookie_headers: metadata.set_cookie_headers,
                rate_limit_headers: metadata.rate_limit_headers,
                rate_limit_updates,
                turn_state_updates,
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
    connect_websocket_with_timeout(base_url, headers, WEBSOCKET_OPEN_TIMEOUT).await
}

async fn connect_websocket_with_timeout(
    base_url: &str,
    headers: HeaderMap,
    open_timeout: Duration,
) -> Result<(CodexWsStream, CodexWebSocketConnectionMetadata), CodexWebSocketError> {
    let ws_request = build_ws_request(base_url, headers)?;
    let (websocket, handshake_response, opening_audit) = timeout(
        open_timeout,
        opening::connect_with_original_opening_handshake(ws_request),
    )
    .await
    .map_err(|_| CodexWebSocketError::OpenTimeout {
        timeout: open_timeout,
    })?
    .map_err(codex_websocket_transport_error)?;
    Ok((
        websocket,
        CodexWebSocketConnectionMetadata::from_headers(handshake_response.headers(), opening_audit),
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
    let url = endpoint_url(base_url, CODEX_RESPONSES_PATH);
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
    turn_state_updates: SharedTurnState,
) {
    loop {
        match next_websocket_message(&mut active.websocket).await {
            Ok(Some(message)) => {
                active.last_activity_at = Instant::now();
                let raw = match websocket_message_text(message) {
                    Ok(Some(raw)) => raw,
                    Ok(None) => continue,
                    Err(error) => {
                        active.discard().await;
                        let _ = tx.unbounded_send(Err(error));
                        return;
                    }
                };
                if is_internal_websocket_event(&raw) {
                    capture_internal_rate_limit_event(&raw, &rate_limit_updates).await;
                    continue;
                }
                let classification_profile = active.error_classification_profile();
                if let Some(classified) = classify_ws_error_frame(&raw, classification_profile) {
                    active.discard().await;
                    let _ = tx.unbounded_send(Err(CodexWebSocketError::Upstream {
                        status: classified.status,
                        retry_after_seconds: retry_after_seconds_from_wrapped_error_headers(&raw)
                            .or_else(|| retry_after_seconds_from_body(&raw)),
                        body: raw,
                    }));
                    return;
                }
                let Some(event) = websocket_event_type(&raw) else {
                    continue;
                };
                if responses_stream_event_shape_parse_error(&raw) {
                    continue;
                }
                if let Some(reason) = incomplete_response_reason(&raw) {
                    active.discard().await;
                    let _ =
                        tx.unbounded_send(Err(CodexWebSocketError::IncompleteResponse { reason }));
                    return;
                }
                if let Some(message) = response_completed_parse_error(&raw) {
                    active.discard().await;
                    let _ = tx.unbounded_send(Err(CodexWebSocketError::InvalidCompletedResponse {
                        message,
                    }));
                    return;
                }
                if response_completed_missing_response(&raw) {
                    continue;
                }
                if response_created_missing_response(&raw) {
                    continue;
                }
                if response_output_text_delta_missing_delta(&raw) {
                    continue;
                }
                if delta_event_missing_official_required_fields(&raw) {
                    continue;
                }
                if output_item_event_missing_item(&raw) {
                    continue;
                }
                if output_item_event_non_object_item(&raw) {
                    continue;
                }
                if output_item_event_invalid_item_type_tag(&raw) {
                    continue;
                }
                if output_item_event_invalid_metadata(&raw) {
                    continue;
                }
                if message_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if agent_message_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if reasoning_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if function_call_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if function_call_output_payload_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if custom_tool_call_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if custom_tool_call_output_payload_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if tool_search_call_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if tool_search_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if local_shell_call_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if web_search_call_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if image_generation_call_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if compaction_output_item_event_invalid_required_fields(&raw) {
                    continue;
                }
                if reasoning_summary_part_added_missing_summary_index(&raw) {
                    continue;
                }
                if event == "response.metadata" {
                    capture_metadata_turn_state_event(&raw, &turn_state_updates).await;
                    continue;
                }
                if event == "error" {
                    continue;
                }
                let terminal = is_terminal_websocket_event(&event);
                let chunk = websocket_sse_chunk(&raw, &event);
                if tx.unbounded_send(Ok(chunk)).is_err() {
                    active.discard().await;
                    return;
                }
                if terminal {
                    active.finish().await;
                    return;
                }
            }
            Ok(None) => {
                let _ = tx.unbounded_send(Err(CodexWebSocketError::StreamClosedBeforeCompleted));
                active.discard().await;
                return;
            }
            Err(error) => {
                let _ = tx.unbounded_send(Err(error));
                active.discard().await;
                return;
            }
        }
    }
}

async fn next_websocket_message(
    websocket: &mut CodexWsStream,
) -> Result<Option<Message>, CodexWebSocketError> {
    match timeout(WEBSOCKET_EVENT_TIMEOUT, websocket.next()).await {
        Ok(Some(Ok(message))) => Ok(Some(message)),
        Ok(Some(Err(error))) => Err(CodexWebSocketError::Transport(error)),
        Ok(None) => Ok(None),
        Err(_) => Err(CodexWebSocketError::ReceiveIdleTimeout {
            timeout: WEBSOCKET_EVENT_TIMEOUT,
        }),
    }
}

async fn timeout_websocket_send<F>(
    send: F,
    timeout_duration: Duration,
) -> Result<(), CodexWebSocketError>
where
    F: Future<Output = Result<(), WsError>>,
{
    match timeout(timeout_duration, send).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(CodexWebSocketError::Transport(error)),
        Err(_) => Err(CodexWebSocketError::SendIdleTimeout {
            timeout: timeout_duration,
        }),
    }
}

async fn capture_internal_rate_limit_event(raw: &str, updates: &SharedRateLimitUpdates) {
    if let Some(parsed) = parse_rate_limits_event_raw(raw) {
        updates.lock().await.push(parsed);
    }
}

async fn capture_metadata_turn_state_event(raw: &str, updates: &SharedTurnState) {
    if let Some(turn_state) = metadata_turn_state(raw) {
        set_latest_turn_state(updates, Some(turn_state)).await;
    }
}

async fn set_latest_turn_state(updates: &SharedTurnState, turn_state: Option<String>) {
    if turn_state.is_some() {
        *updates.lock().await = turn_state;
    }
}

pub async fn latest_turn_state(updates: &SharedTurnState) -> Option<String> {
    updates.lock().await.clone()
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

#[cfg(test)]
mod tests {
    use futures::future;
    use reqwest::header::HeaderMap;
    use tokio::{net::TcpListener, time::Duration};

    use super::{connect_websocket_with_timeout, timeout_websocket_send, CodexWebSocketError};

    #[tokio::test]
    async fn connect_websocket_with_timeout_should_fail_when_handshake_stalls() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let result = connect_websocket_with_timeout(
            &format!("http://{addr}"),
            HeaderMap::new(),
            Duration::from_millis(10),
        )
        .await;

        let Err(CodexWebSocketError::OpenTimeout { timeout }) = result else {
            panic!("expected websocket open timeout");
        };
        assert_eq!(timeout, Duration::from_millis(10));
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_websocket_send_should_fail_when_send_future_stalls() {
        let result = tokio::spawn(timeout_websocket_send(
            future::pending(),
            Duration::from_secs(20),
        ));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(20)).await;

        let Err(CodexWebSocketError::SendIdleTimeout { timeout }) = result.await.unwrap() else {
            panic!("expected websocket send timeout");
        };
        assert_eq!(timeout, Duration::from_secs(20));
    }
}
