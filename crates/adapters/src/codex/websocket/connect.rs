//! Codex WebSocket 连接建立。

use std::{pin::Pin, sync::Arc, time::Duration, time::Instant};

use bytes::Bytes;
use codex_proxy_core::protocol::codex::{
    events::{
        extract_sse_usage, parse_rate_limits_event_raw, rate_limits_to_header_pairs,
        retry_after_seconds_from_body, TokenUsage,
    },
    responses::CodexResponsesRequest,
    sse::SseError,
    websocket::{
        classify_websocket_error_frame, is_terminal_websocket_event,
        retry_after_seconds_from_wrapped_error_headers, websocket_event_to_sse_frame,
        websocket_incomplete_response_reason, websocket_metadata_turn_state,
        websocket_response_completed_parse_error, websocket_response_create_payload_text,
        OpeningAuditHeader, OpeningAuditSnapshot, WebSocketErrorClassificationProfile,
    },
};
use futures::{channel::mpsc, SinkExt, Stream, StreamExt};
use serde_json::Value;
use thiserror::Error;
use tokio::{sync::Mutex, time::timeout};
use tokio_tungstenite::{connect_async_tls_with_config, Connector};
use tungstenite::{
    self,
    extensions::{compression::deflate::DeflateConfig, ExtensionsConfig},
    handshake::client::Request as WsRequest,
    http::{HeaderMap, Response as WsResponse},
    protocol::WebSocketConfig,
    Message,
};

use crate::codex::client::maybe_build_rustls_client_config_with_custom_ca;

use super::pool::{
    CodexWebSocketConnectionMetadata, CodexWebSocketPool, CodexWebSocketPoolKey, CodexWsStream,
    PooledWebSocketConnection, WebSocketPoolAcquire,
};

const REDACTED_HEADER_VALUE: &str = "<redacted>";
const CODEX_RESPONSES_PATH: &str = "/codex/responses";
const WEBSOCKET_EXTENSIONS: &str = "permessage-deflate; client_max_window_bits";
const WEBSOCKET_RECEIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(20);

/// WebSocket 连接适配器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketConnection {
    endpoint: String,
    headers: Vec<(String, String)>,
}

/// Prepared Responses WebSocket request descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketRequest {
    connection: CodexWebSocketConnection,
    payload_text: String,
}

/// Responses WebSocket exchange result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketExchange {
    /// 完整 SSE 文本。
    pub body: String,
    /// 从 SSE 中提取的 usage。
    pub usage: Option<TokenUsage>,
    /// 上游 metadata 帧中的最新 turn state。
    pub turn_state: Option<String>,
    /// 上游握手响应里的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游握手响应里的限流头。
    pub rate_limit_headers: Vec<(String, String)>,
    /// 打开握手响应状态码。
    pub handshake_status: u16,
}

/// Responses WebSocket live SSE exchange result.
pub struct CodexWebSocketStreamingExchange {
    /// Live SSE bytes converted from WebSocket events.
    pub body: CodexWebSocketSseStream,
    /// 上游 metadata 帧中的最新 turn state。
    pub turn_state: Option<String>,
    /// 上游握手响应里的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游握手响应里的限流头。
    pub rate_limit_headers: Vec<(String, String)>,
    /// 上游内部 rate-limit 事件里的动态更新。
    pub rate_limit_header_updates: CodexWebSocketRateLimitHeaderUpdates,
    /// 上游内部 metadata 事件里的动态 turn state。
    pub turn_state_update: CodexWebSocketTurnStateUpdate,
    /// 打开握手响应状态码。
    pub handshake_status: u16,
}

/// Responses WebSocket live SSE byte stream.
pub type CodexWebSocketSseStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, CodexWebSocketExchangeError>> + Send + 'static>>;
/// WebSocket live stream rate-limit header updates.
pub type CodexWebSocketRateLimitHeaderUpdates = Arc<Mutex<Vec<(String, String)>>>;
/// WebSocket live stream turn-state update.
pub type CodexWebSocketTurnStateUpdate = Arc<Mutex<Option<String>>>;

/// Responses WebSocket exchange error.
#[derive(Debug, Error)]
pub enum CodexWebSocketExchangeError {
    /// opening request 无法构造。
    #[error("invalid websocket request: {0}")]
    InvalidRequest(#[from] tungstenite::http::Error),
    /// WebSocket 传输失败。
    #[error("websocket transport error: {0}")]
    Transport(#[from] tungstenite::Error),
    /// SSE 聚合结果无法解析。
    #[error("invalid websocket SSE response: {0}")]
    InvalidSse(#[from] SseError),
    /// 上游 WebSocket 错误帧。
    #[error("websocket upstream error {status_code}: {body}")]
    Upstream {
        /// HTTP-style upstream status code.
        status_code: u16,
        /// 推导出的重试秒数。
        retry_after_seconds: Option<u64>,
        /// 原始错误帧。
        body: String,
        /// 上游透传的 `set-cookie` 列表。
        set_cookie_headers: Vec<String>,
    },
    /// 上游返回 `response.incomplete`。
    #[error("Incomplete response returned, reason: {reason}")]
    IncompleteResponse {
        /// incomplete_details.reason。
        reason: String,
    },
    /// 上游返回无法按官方形状解析的 `response.completed`。
    #[error("{message}")]
    InvalidCompletedResponse {
        /// 解析失败说明。
        message: String,
    },
    /// 上游在 terminal 事件前关闭。
    #[error("websocket closed before terminal event")]
    ClosedBeforeTerminal,
    /// 上游在指定时间内没有发送任何事件。
    #[error("websocket receive idle timeout after {timeout:?}")]
    ReceiveIdleTimeout {
        /// 超时时长。
        timeout: Duration,
    },
    /// 上游返回非文本事件帧。
    #[error("unexpected binary websocket event")]
    UnexpectedBinaryEvent,
}

impl CodexWebSocketRequest {
    /// 返回连接描述。
    pub fn connection(&self) -> &CodexWebSocketConnection {
        &self.connection
    }

    /// 返回将要发送的首个文本帧。
    pub fn payload_text(&self) -> &str {
        &self.payload_text
    }
}

/// 执行一次 prepared Responses WebSocket 请求并聚合为 SSE。
pub async fn execute_response_create_request(
    request: &CodexWebSocketRequest,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    execute_response_create_request_with_pool(request, None).await
}

pub(crate) async fn execute_response_create_request_with_pool(
    request: &CodexWebSocketRequest,
    pool: Option<(&CodexWebSocketPool, CodexWebSocketPoolKey)>,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let Some((pool, key)) = pool else {
        return execute_fresh_response_create_request(request).await;
    };

    match pool.acquire(&key).await {
        WebSocketPoolAcquire::Reused(connection) => {
            execute_pooled_response_create_request(request, pool, key, *connection).await
        }
        WebSocketPoolAcquire::FreshReserved => {
            let result =
                execute_fresh_pooled_response_create_request(request, pool, key.clone()).await;
            if result.is_err() {
                pool.discard(&key).await;
            }
            result
        }
        WebSocketPoolAcquire::Bypass => execute_fresh_response_create_request(request).await,
    }
}

pub(crate) async fn execute_response_create_request_stream_with_pool(
    request: &CodexWebSocketRequest,
    pool: Option<(&CodexWebSocketPool, CodexWebSocketPoolKey)>,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let Some((pool, key)) = pool else {
        return execute_fresh_response_create_request_stream(request).await;
    };

    match pool.acquire(&key).await {
        WebSocketPoolAcquire::Reused(connection) => {
            execute_pooled_response_create_request_stream(request, pool.clone(), key, *connection)
                .await
        }
        WebSocketPoolAcquire::FreshReserved => {
            let result = execute_fresh_pooled_response_create_request_stream(
                request,
                pool.clone(),
                key.clone(),
            )
            .await;
            if result.is_err() {
                pool.discard(&key).await;
            }
            result
        }
        WebSocketPoolAcquire::Bypass => execute_fresh_response_create_request_stream(request).await,
    }
}

async fn execute_fresh_response_create_request(
    request: &CodexWebSocketRequest,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let (mut websocket, response) = connect_websocket(request.connection()).await?;
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let metadata = websocket_connection_metadata(&response);
    let (exchange, _websocket, _metadata) = collect_websocket_response(websocket, metadata).await?;
    Ok(exchange)
}

async fn execute_fresh_response_create_request_stream(
    request: &CodexWebSocketRequest,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let (mut websocket, response) = connect_websocket(request.connection()).await?;
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let metadata = websocket_connection_metadata(&response);
    Ok(stream_websocket_response(websocket, metadata, None))
}

async fn execute_fresh_pooled_response_create_request(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let (mut websocket, response) = connect_websocket(request.connection()).await?;
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let now = Instant::now();
    let metadata = websocket_connection_metadata(&response);
    let (exchange, websocket, metadata) = collect_websocket_response(websocket, metadata).await?;
    let last_activity_at = Instant::now();
    pool.put(
        key,
        PooledWebSocketConnection {
            websocket,
            metadata,
            created_at: now,
            last_activity_at,
            last_ping_at: None,
        },
    )
    .await;
    Ok(exchange)
}

async fn execute_fresh_pooled_response_create_request_stream(
    request: &CodexWebSocketRequest,
    pool: CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let (mut websocket, response) = connect_websocket(request.connection()).await?;
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let now = Instant::now();
    let metadata = websocket_connection_metadata(&response);
    Ok(stream_websocket_response(
        websocket,
        metadata,
        Some(WebSocketStreamPoolReturn {
            pool,
            key,
            created_at: now,
            last_ping_at: None,
        }),
    ))
}

async fn execute_pooled_response_create_request(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
    mut connection: PooledWebSocketConnection,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    if let Err(error) = connection
        .websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await
    {
        pool.discard(&key).await;
        return Err(error.into());
    }

    let created_at = connection.created_at;
    let last_ping_at = connection.last_ping_at;
    match collect_websocket_response(connection.websocket, connection.metadata).await {
        Ok((exchange, websocket, metadata)) => {
            let last_activity_at = Instant::now();
            pool.put(
                key,
                PooledWebSocketConnection {
                    websocket,
                    metadata,
                    created_at,
                    last_activity_at,
                    last_ping_at,
                },
            )
            .await;
            Ok(exchange)
        }
        Err(error) => {
            pool.discard(&key).await;
            Err(error)
        }
    }
}

async fn execute_pooled_response_create_request_stream(
    request: &CodexWebSocketRequest,
    pool: CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
    mut connection: PooledWebSocketConnection,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    if let Err(error) = connection
        .websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await
    {
        pool.discard(&key).await;
        return Err(error.into());
    }

    Ok(stream_websocket_response(
        connection.websocket,
        connection.metadata,
        Some(WebSocketStreamPoolReturn {
            pool,
            key,
            created_at: connection.created_at,
            last_ping_at: connection.last_ping_at,
        }),
    ))
}

impl CodexWebSocketConnection {
    /// 构造待打开的 WebSocket 连接描述。
    pub fn new(endpoint: impl Into<String>, headers: Vec<(String, String)>) -> Self {
        Self {
            endpoint: endpoint.into(),
            headers,
        }
    }

    /// 构造 Responses WebSocket 连接描述。
    pub fn responses(
        base_url: &str,
        websocket_key: &str,
        business_headers: Vec<(String, String)>,
    ) -> Self {
        let endpoint = responses_websocket_endpoint(base_url);
        let mut headers = Vec::new();
        if let Some(host) = websocket_host_header(&endpoint) {
            headers.push(("Host".to_string(), host));
        }
        headers.extend([
            ("Connection".to_string(), "Upgrade".to_string()),
            ("Upgrade".to_string(), "websocket".to_string()),
            ("Sec-WebSocket-Version".to_string(), "13".to_string()),
            ("Sec-WebSocket-Key".to_string(), websocket_key.to_string()),
        ]);
        headers.extend(business_headers);
        headers.push((
            "sec-websocket-extensions".to_string(),
            WEBSOCKET_EXTENSIONS.to_string(),
        ));
        Self { endpoint, headers }
    }

    /// 构造 Responses WebSocket opening 与首个 `response.create` 文本帧。
    pub fn responses_create_request(
        base_url: &str,
        websocket_key: &str,
        business_headers: Vec<(String, String)>,
        request: &CodexResponsesRequest,
    ) -> Result<CodexWebSocketRequest, serde_json::Error> {
        Ok(CodexWebSocketRequest {
            connection: Self::responses(base_url, websocket_key, business_headers),
            payload_text: websocket_response_create_payload_text(request)?,
        })
    }

    /// 返回 WebSocket endpoint。
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// 返回按发送顺序保存的请求头。
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// 生成真实 WebSocket opening 请求文本，用于 capture/audit parity。
    pub fn opening_request_text(&self) -> String {
        String::from_utf8_lossy(&opening_request_bytes(self)).into_owned()
    }

    /// 生成打开握手审计快照。
    pub fn opening_audit_snapshot(&self) -> OpeningAuditSnapshot {
        OpeningAuditSnapshot {
            request_line: request_line_for_endpoint(&self.endpoint),
            header_order: self.headers.iter().map(|(name, _)| name.clone()).collect(),
            headers: self
                .headers
                .iter()
                .map(|(name, value)| OpeningAuditHeader {
                    name: name.clone(),
                    value: audit_header_value(name, value),
                })
                .collect(),
        }
    }
}

/// 将 Codex backend base URL 转换为 Responses WebSocket endpoint。
pub fn responses_websocket_endpoint(base_url: &str) -> String {
    let endpoint = format!("{}{}", base_url.trim_end_matches('/'), CODEX_RESPONSES_PATH);
    if let Some(rest) = endpoint.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = endpoint.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        endpoint
    }
}

fn websocket_host_header(endpoint: &str) -> Option<String> {
    let url = reqwest::Url::parse(endpoint).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

async fn connect_websocket(
    connection: &CodexWebSocketConnection,
) -> Result<(CodexWsStream, WsResponse<Option<Vec<u8>>>), CodexWebSocketExchangeError> {
    let request = websocket_handshake_request(connection)?;
    let connector = maybe_build_rustls_client_config_with_custom_ca()
        .map_err(|error| tungstenite::Error::Io(std::io::Error::other(error)))?
        .map(Connector::Rustls);
    match connect_async_tls_with_config(request, Some(websocket_config()), false, connector).await {
        Ok((websocket, response)) => Ok((websocket, response)),
        Err(tungstenite::Error::Http(response)) => Err(websocket_opening_error(*response)),
        Err(error) => Err(error.into()),
    }
}

fn websocket_handshake_request(
    connection: &CodexWebSocketConnection,
) -> Result<WsRequest, tungstenite::http::Error> {
    let mut builder = WsRequest::builder()
        .method("GET")
        .uri(connection.endpoint());
    for (name, value) in connection.headers() {
        if name.eq_ignore_ascii_case("sec-websocket-extensions") {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder.body(())
}

fn websocket_config() -> WebSocketConfig {
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());

    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    config
}

fn websocket_opening_error(response: WsResponse<Option<Vec<u8>>>) -> CodexWebSocketExchangeError {
    let status_code = response.status().as_u16();
    let body = response
        .body()
        .as_ref()
        .map(|body| String::from_utf8_lossy(body).into_owned())
        .unwrap_or_default();
    let retry_after_seconds = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .or_else(|| retry_after_seconds_from_body(&body));
    CodexWebSocketExchangeError::Upstream {
        status_code,
        retry_after_seconds,
        body,
        set_cookie_headers: websocket_set_cookie_headers(response.headers()),
    }
}

fn opening_request_bytes(connection: &CodexWebSocketConnection) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(request_line_for_endpoint(connection.endpoint()).as_bytes());
    bytes.extend_from_slice(b"\r\n");
    for (name, value) in connection.headers() {
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend_from_slice(value.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"\r\n");
    bytes
}

async fn collect_websocket_response(
    mut websocket: CodexWsStream,
    mut metadata: CodexWebSocketConnectionMetadata,
) -> Result<
    (
        CodexWebSocketExchange,
        CodexWsStream,
        CodexWebSocketConnectionMetadata,
    ),
    CodexWebSocketExchangeError,
> {
    let mut body = String::new();

    loop {
        let message = next_websocket_message(&mut websocket).await?;
        let Some(message) = message else {
            break;
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(_) => return Err(CodexWebSocketExchangeError::UnexpectedBinaryEvent),
            Message::Close(_) => break,
            _ => continue,
        };
        let raw = text.to_string();
        if let Some(classified) =
            classify_websocket_error_frame(&raw, WebSocketErrorClassificationProfile::OneShot)
        {
            let retry_after_seconds = retry_after_seconds_from_wrapped_error_headers(&raw)
                .or_else(|| retry_after_seconds_from_body(&raw));
            return Err(CodexWebSocketExchangeError::Upstream {
                status_code: classified.status_code,
                retry_after_seconds,
                body: raw,
                set_cookie_headers: Vec::new(),
            });
        }
        if let Some(reason) = websocket_incomplete_response_reason(&raw) {
            return Err(CodexWebSocketExchangeError::IncompleteResponse { reason });
        }
        if let Some(message) = websocket_response_completed_parse_error(&raw) {
            return Err(CodexWebSocketExchangeError::InvalidCompletedResponse { message });
        }
        if let Some(headers) = websocket_rate_limit_event_headers(&raw) {
            metadata.rate_limit_headers.extend(headers);
            continue;
        }
        if let Some(metadata_turn_state) = websocket_metadata_turn_state(&raw) {
            metadata.turn_state = Some(metadata_turn_state);
            continue;
        }
        let event = websocket_event_type(&raw);
        if event.as_deref() == Some("error") {
            continue;
        }
        let forwarded = if let Some(frame) = websocket_event_to_sse_frame(&raw) {
            body.push_str(&frame);
            true
        } else {
            false
        };
        if forwarded && event.as_deref().is_some_and(is_terminal_websocket_event) {
            let usage = extract_sse_usage(&body)?;
            let exchange = CodexWebSocketExchange {
                body,
                usage,
                turn_state: metadata.turn_state.clone(),
                set_cookie_headers: metadata.set_cookie_headers.clone(),
                rate_limit_headers: metadata.rate_limit_headers.clone(),
                handshake_status: metadata.handshake_status,
            };
            return Ok((exchange, websocket, metadata));
        }
    }

    Err(CodexWebSocketExchangeError::ClosedBeforeTerminal)
}

fn websocket_connection_metadata(
    response: &WsResponse<Option<Vec<u8>>>,
) -> CodexWebSocketConnectionMetadata {
    CodexWebSocketConnectionMetadata {
        turn_state: websocket_response_turn_state(response.headers()),
        set_cookie_headers: websocket_set_cookie_headers(response.headers()),
        rate_limit_headers: websocket_rate_limit_headers(response.headers()),
        handshake_status: response.status().as_u16(),
    }
}

fn websocket_rate_limit_event_headers(raw: &str) -> Option<Vec<(String, String)>> {
    parse_rate_limits_event_raw(raw).map(|parsed| rate_limits_to_header_pairs(&parsed))
}

struct WebSocketStreamPoolReturn {
    pool: CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
    created_at: Instant,
    last_ping_at: Option<Instant>,
}

fn stream_websocket_response(
    websocket: CodexWsStream,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
) -> CodexWebSocketStreamingExchange {
    let response_metadata = metadata.clone();
    let rate_limit_header_updates = Arc::new(Mutex::new(Vec::new()));
    let rate_limit_header_updates_for_task = Arc::clone(&rate_limit_header_updates);
    let turn_state_update = Arc::new(Mutex::new(metadata.turn_state.clone()));
    let turn_state_update_for_task = Arc::clone(&turn_state_update);
    let (tx, rx) = mpsc::unbounded();
    tokio::spawn(async move {
        forward_websocket_response_stream(
            websocket,
            metadata,
            pool_return,
            rate_limit_header_updates_for_task,
            turn_state_update_for_task,
            tx,
        )
        .await;
    });

    CodexWebSocketStreamingExchange {
        body: Box::pin(rx),
        turn_state: response_metadata.turn_state,
        set_cookie_headers: response_metadata.set_cookie_headers,
        rate_limit_headers: response_metadata.rate_limit_headers,
        rate_limit_header_updates,
        turn_state_update,
        handshake_status: response_metadata.handshake_status,
    }
}

async fn forward_websocket_response_stream(
    mut websocket: CodexWsStream,
    mut metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
    rate_limit_header_updates: CodexWebSocketRateLimitHeaderUpdates,
    turn_state_update: CodexWebSocketTurnStateUpdate,
    tx: mpsc::UnboundedSender<Result<Bytes, CodexWebSocketExchangeError>>,
) {
    let mut pool_return = pool_return;
    loop {
        let message = match next_websocket_message(&mut websocket).await {
            Ok(message) => message,
            Err(error) => {
                discard_stream_websocket(websocket, pool_return).await;
                let _ = tx.unbounded_send(Err(error));
                return;
            }
        };
        let Some(message) = message else {
            break;
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(_) => {
                discard_stream_websocket(websocket, pool_return).await;
                let _ = tx.unbounded_send(Err(CodexWebSocketExchangeError::UnexpectedBinaryEvent));
                return;
            }
            Message::Close(_) => {
                discard_stream_websocket(websocket, pool_return).await;
                let _ = tx.unbounded_send(Err(CodexWebSocketExchangeError::ClosedBeforeTerminal));
                return;
            }
            _ => continue,
        };
        let raw = text.to_string();
        if let Some(classified) =
            classify_websocket_error_frame(&raw, WebSocketErrorClassificationProfile::OneShot)
        {
            discard_stream_websocket(websocket, pool_return).await;
            let retry_after_seconds = retry_after_seconds_from_wrapped_error_headers(&raw)
                .or_else(|| retry_after_seconds_from_body(&raw));
            let _ = tx.unbounded_send(Err(CodexWebSocketExchangeError::Upstream {
                status_code: classified.status_code,
                retry_after_seconds,
                body: raw,
                set_cookie_headers: Vec::new(),
            }));
            return;
        }
        if let Some(reason) = websocket_incomplete_response_reason(&raw) {
            discard_stream_websocket(websocket, pool_return).await;
            let _ = tx.unbounded_send(Err(CodexWebSocketExchangeError::IncompleteResponse {
                reason,
            }));
            return;
        }
        if let Some(message) = websocket_response_completed_parse_error(&raw) {
            discard_stream_websocket(websocket, pool_return).await;
            let _ = tx.unbounded_send(Err(CodexWebSocketExchangeError::InvalidCompletedResponse {
                message,
            }));
            return;
        }
        if let Some(headers) = websocket_rate_limit_event_headers(&raw) {
            metadata.rate_limit_headers.extend(headers.iter().cloned());
            rate_limit_header_updates.lock().await.extend(headers);
            continue;
        }
        if let Some(metadata_turn_state) = websocket_metadata_turn_state(&raw) {
            metadata.turn_state = Some(metadata_turn_state);
            *turn_state_update.lock().await = metadata.turn_state.clone();
            continue;
        }
        let event = websocket_event_type(&raw);
        if event.as_deref() == Some("error") {
            continue;
        }
        let Some(frame) = websocket_event_to_sse_frame(&raw) else {
            continue;
        };
        let terminal = event.as_deref().is_some_and(is_terminal_websocket_event);
        if tx.unbounded_send(Ok(Bytes::from(frame))).is_err() {
            discard_stream_websocket(websocket, pool_return).await;
            return;
        }
        if terminal {
            finish_stream_websocket(websocket, metadata, pool_return.take()).await;
            return;
        }
    }

    discard_stream_websocket(websocket, pool_return).await;
    let _ = tx.unbounded_send(Err(CodexWebSocketExchangeError::ClosedBeforeTerminal));
}

async fn next_websocket_message(
    websocket: &mut CodexWsStream,
) -> Result<Option<Message>, CodexWebSocketExchangeError> {
    match timeout(WEBSOCKET_RECEIVE_IDLE_TIMEOUT, websocket.next()).await {
        Ok(message) => message.transpose().map_err(Into::into),
        Err(_) => Err(CodexWebSocketExchangeError::ReceiveIdleTimeout {
            timeout: WEBSOCKET_RECEIVE_IDLE_TIMEOUT,
        }),
    }
}

async fn finish_stream_websocket(
    mut websocket: CodexWsStream,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
) {
    let Some(pool_return) = pool_return else {
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
                last_activity_at: Instant::now(),
                last_ping_at: pool_return.last_ping_at,
            },
        )
        .await;
}

async fn discard_stream_websocket(
    mut websocket: CodexWsStream,
    pool_return: Option<WebSocketStreamPoolReturn>,
) {
    if let Some(pool_return) = pool_return {
        pool_return.pool.discard(&pool_return.key).await;
    }
    let _ = websocket.close(None).await;
}

fn websocket_event_type(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw).ok().and_then(|value| {
        value
            .get("type")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn websocket_response_turn_state(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-codex-turn-state")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn websocket_set_cookie_headers(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToString::to_string))
        .collect()
}

fn websocket_rate_limit_headers(headers: &HeaderMap) -> Vec<(String, String)> {
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
    name == "retry-after"
        || name.contains("ratelimit")
        || name.contains("rate-limit")
        || name.starts_with("x-codex-primary-")
        || name.starts_with("x-codex-secondary-")
        || name.starts_with("x-codex-code-review-")
        || name.starts_with("x-codex-review-")
        || name.starts_with("x-code-review-")
}

fn request_line_for_endpoint(endpoint: &str) -> String {
    let path = reqwest::Url::parse(endpoint)
        .ok()
        .map(|url| {
            let mut path = url.path().to_string();
            if let Some(query) = url.query() {
                path.push('?');
                path.push_str(query);
            }
            path
        })
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| endpoint.to_string());
    format!("GET {path} HTTP/1.1")
}

fn audit_header_value(name: &str, value: &str) -> String {
    if is_sensitive_opening_header(name) {
        REDACTED_HEADER_VALUE.to_string()
    } else {
        value.to_string()
    }
}

fn is_sensitive_opening_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "chatgpt-account-id"
            | "cookie"
            | "session_id"
            | "session-id"
            | "thread-id"
            | "x-client-request-id"
            | "x-codex-window-id"
            | "x-codex-turn-metadata"
            | "x-codex-turn-state"
            | "x-codex-parent-thread-id"
    )
}
