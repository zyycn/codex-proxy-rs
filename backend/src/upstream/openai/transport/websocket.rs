//! Codex WebSocket 连接建立（关键函数）。

use std::{
    collections::VecDeque,
    fmt, io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use chrono::Utc;
use futures::Stream;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::{sync::Mutex, time::timeout};
use tokio_tungstenite::{connect_async_tls_with_config, Connector};
use tungstenite::{
    self,
    extensions::{compression::deflate::DeflateConfig, ExtensionsConfig},
    handshake::client::Request as WsRequest,
    http::Response as WsResponse,
    protocol::WebSocketConfig,
    Message,
};

use crate::infra::time::china_filename_timestamp_millis;
use crate::upstream::openai::protocol::events::{self, TokenUsage};
use crate::upstream::openai::protocol::responses::{
    response_body_has_first_output, CodexResponsesRequest,
};
use crate::upstream::openai::protocol::sse::SseError;
use crate::upstream::openai::protocol::websocket::{
    classify_websocket_error_frame, is_terminal_websocket_event,
    retry_after_seconds_from_wrapped_error_headers, websocket_event_to_sse_frame,
    websocket_event_type, websocket_incomplete_response_reason, websocket_metadata_turn_state,
    websocket_response_completed_parse_error, websocket_response_create_payload_text,
    OpeningAuditHeader, OpeningAuditSnapshot, WebSocketAuditArtifact,
};

use super::websocket_pool::{
    CodexWebSocketConnectionMetadata, CodexWebSocketPool, CodexWebSocketPoolKey,
    PooledWebSocketConnection, WebSocketPoolAcquire, WebSocketPoolDecision,
};
use super::websocket_pump::{PumpKeepalive, PumpedWebSocket, RawWsStream};
use super::{diagnostics::CodexUpstreamDiagnostics, response_meta};
use uuid::Uuid;

const REDACTED_HEADER_VALUE: &str = "<redacted>";
const CODEX_RESPONSES_PATH: &str = "/codex/responses";
const WEBSOCKET_EXTENSIONS: &str = "permessage-deflate; client_max_window_bits";
const WEBSOCKET_RECEIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const WEBSOCKET_ACTIVE_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const WEBSOCKET_STREAM_BUFFER: usize = 16;
const WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS: usize = 2;
/// WebSocket audit artifact 输出目录环境变量。
pub const WS_AUDIT_DIR_ENV: &str = "CODEX_PROXY_WS_AUDIT_DIR";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketConnection {
    pub(super) endpoint: String,
    pub(super) headers: Vec<(String, String)>,
}

/// 显式写入 WebSocket audit artifact。
pub async fn write_websocket_audit_artifact_for_dir(
    dir: Option<&Path>,
    artifact: &WebSocketAuditArtifact,
) -> io::Result<Option<PathBuf>> {
    let Some(dir) = dir.filter(|dir| !dir.as_os_str().is_empty()) else {
        return Ok(None);
    };

    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(websocket_audit_file_name());
    let body = serde_json::to_vec_pretty(artifact).map_err(io::Error::other)?;
    tokio::fs::write(&path, body).await?;
    Ok(Some(path))
}

/// 按环境变量配置写入 WebSocket audit artifact。
pub async fn write_websocket_audit_artifact_from_env(
    artifact: &WebSocketAuditArtifact,
) -> io::Result<Option<PathBuf>> {
    let Some(dir) = std::env::var_os(WS_AUDIT_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };

    write_websocket_audit_artifact_for_dir(Some(&dir), artifact).await
}

/// Prepared Responses WebSocket request descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketRequest {
    pub(super) connection: CodexWebSocketConnection,
    pub(super) payload_text: String,
}

#[path = "websocket_frames.rs"]
mod frames;

use frames::{
    audit_header_value, collect_websocket_response, is_first_token_timeout,
    prefetch_stream_frames_until_output_or_terminal, reusable_websocket_metadata,
    reused_stream_prefetch_error, stream_websocket_response, websocket_audit_file_name,
    websocket_connection_metadata, WebSocketStreamPoolReturn,
};
pub use frames::{
    execute_response_create_request, CodexWebSocketExchange, CodexWebSocketExchangeError,
    CodexWebSocketRateLimitHeaderUpdates, CodexWebSocketSseStream, CodexWebSocketStreamingExchange,
    CodexWebSocketTurnStateUpdate, CodexWebSocketUpstreamError,
};

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

// ---------------------------------------------------------------------------
// WebSocket endpoint helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

async fn connect_websocket(
    connection: &CodexWebSocketConnection,
) -> Result<(RawWsStream, WsResponse<Option<Vec<u8>>>), CodexWebSocketExchangeError> {
    let request = websocket_handshake_request(connection)?;
    let connector = super::tls::maybe_build_rustls_client_config_with_custom_ca()
        .map_err(|error| tungstenite::Error::Io(std::io::Error::other(error)))?
        .map(Connector::Rustls);
    match connect_async_tls_with_config(request, Some(websocket_config()), false, connector).await {
        Ok((websocket, response)) => Ok((websocket, response)),
        Err(tungstenite::Error::Http(response)) => Err(websocket_opening_error(response.as_ref())),
        Err(error) => Err(error.into()),
    }
}

/// 建立连接并交给后台 pump 托管；`keepalive` 决定该连接是否做主动保活。
///
/// 非池化（即用即弃）连接用 [`PumpKeepalive::disabled`]；池化连接用连接池派生的保活策略，
/// 让空闲期在后台完成 ping/pong 与失活检测，从而复用前可零成本判活。
async fn connect_pumped_websocket(
    connection: &CodexWebSocketConnection,
    keepalive: PumpKeepalive,
) -> Result<(PumpedWebSocket, WsResponse<Option<Vec<u8>>>), CodexWebSocketExchangeError> {
    let (raw, response) = connect_websocket(connection).await?;
    Ok((PumpedWebSocket::new(raw, keepalive), response))
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

fn websocket_opening_error(response: &WsResponse<Option<Vec<u8>>>) -> CodexWebSocketExchangeError {
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
        .or_else(|| events::retry_after_seconds_from_body(&body));
    CodexWebSocketExchangeError::upstream(
        status_code,
        retry_after_seconds,
        body,
        response_meta::set_cookie_headers(response.headers()),
        response_meta::diagnostics(Some(status_code), response.headers()),
    )
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

// ---------------------------------------------------------------------------
// Fresh request execution
// ---------------------------------------------------------------------------

async fn execute_fresh_response_create_request(
    request: &CodexWebSocketRequest,
    started_at: Instant,
    first_token_timeout: Option<Duration>,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let (websocket, response) =
        connect_pumped_websocket(request.connection(), PumpKeepalive::disabled()).await?;
    let first_token_started_at = Instant::now();
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let metadata = websocket_connection_metadata(&response);
    let (exchange, _websocket, _metadata) = collect_websocket_response(
        websocket,
        metadata,
        false,
        started_at,
        first_token_started_at,
        first_token_timeout,
    )
    .await?;
    Ok(exchange)
}

async fn execute_fresh_response_create_request_stream(
    request: &CodexWebSocketRequest,
    first_token_timeout: Option<Duration>,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let (mut websocket, response) =
        connect_pumped_websocket(request.connection(), PumpKeepalive::disabled()).await?;
    let first_token_started_at = Instant::now();
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let mut metadata = websocket_connection_metadata(&response);
    let prefetched_frames = match prefetch_stream_frames_until_output_or_terminal(
        &mut websocket,
        &mut metadata,
        first_token_started_at,
        first_token_timeout,
    )
    .await
    {
        Ok(prefetched_frames) => prefetched_frames,
        Err(error) => {
            websocket.close().await;
            return Err(error);
        }
    };
    Ok(stream_websocket_response(
        websocket,
        metadata,
        None,
        prefetched_frames,
    ))
}

async fn execute_fresh_response_create_request_with_retries(
    request: &CodexWebSocketRequest,
    started_at: Instant,
    first_token_timeout: Option<Duration>,
    attempts: usize,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let attempts = attempts.max(1);
    for attempt in 1..=attempts {
        match execute_fresh_response_create_request(request, started_at, first_token_timeout).await
        {
            Ok(exchange) => return Ok(exchange),
            Err(error) if attempt < attempts && is_first_token_timeout(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("fresh websocket retry loop always returns");
}

async fn execute_fresh_response_create_request_stream_with_retries(
    request: &CodexWebSocketRequest,
    first_token_timeout: Option<Duration>,
    attempts: usize,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let attempts = attempts.max(1);
    for attempt in 1..=attempts {
        match execute_fresh_response_create_request_stream(request, first_token_timeout).await {
            Ok(exchange) => return Ok(exchange),
            Err(error) if attempt < attempts && is_first_token_timeout(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("fresh websocket stream retry loop always returns");
}

// ---------------------------------------------------------------------------
// Pool-aware execution
// ---------------------------------------------------------------------------

pub(crate) async fn execute_response_create_request_with_pool(
    request: &CodexWebSocketRequest,
    pool: Option<(&CodexWebSocketPool, CodexWebSocketPoolKey)>,
    started_at: Instant,
    fallback_first_token_timeout: Option<Duration>,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let Some((pool, key)) = pool else {
        return execute_fresh_response_create_request_with_retries(
            request,
            started_at,
            fallback_first_token_timeout,
            WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS,
        )
        .await;
    };

    match pool.acquire(&key).await {
        WebSocketPoolAcquire::Reused(connection) => {
            let result =
                execute_pooled_response_create_request(request, pool, key, *connection, started_at)
                    .await;
            match result {
                Err(CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                    ..
                })
                | Err(CodexWebSocketExchangeError::FirstTokenTimeout { .. }) => {
                    let mut exchange = execute_fresh_response_create_request_with_retries(
                        request,
                        started_at,
                        pool.first_token_timeout(),
                        WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS,
                    )
                    .await?;
                    exchange.pool_decision = Some(WebSocketPoolDecision::retry_after_stale_reuse());
                    Ok(exchange)
                }
                Ok(mut exchange) => {
                    exchange.pool_decision = Some(WebSocketPoolDecision::reuse());
                    Ok(exchange)
                }
                Err(error) => Err(error),
            }
        }
        WebSocketPoolAcquire::FreshReserved => {
            let result = execute_fresh_pooled_response_create_request(
                request,
                pool,
                key.clone(),
                started_at,
            )
            .await;
            match result {
                Ok(mut exchange) => {
                    exchange.pool_decision = Some(WebSocketPoolDecision::new());
                    Ok(exchange)
                }
                Err(error) if is_first_token_timeout(&error) => {
                    pool.discard(&key).await;
                    let mut exchange = execute_fresh_response_create_request_with_retries(
                        request,
                        started_at,
                        pool.first_token_timeout(),
                        1,
                    )
                    .await?;
                    exchange.pool_decision = Some(WebSocketPoolDecision::new());
                    Ok(exchange)
                }
                Err(error) => {
                    pool.discard(&key).await;
                    Err(error)
                }
            }
        }
        WebSocketPoolAcquire::Bypass(reason) => {
            let mut exchange = execute_fresh_response_create_request_with_retries(
                request,
                started_at,
                pool.first_token_timeout(),
                WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS,
            )
            .await?;
            exchange.pool_decision = Some(WebSocketPoolDecision::bypass(reason));
            Ok(exchange)
        }
    }
}

pub(crate) async fn execute_response_create_request_stream_with_pool(
    request: &CodexWebSocketRequest,
    pool: Option<(&CodexWebSocketPool, CodexWebSocketPoolKey)>,
    fallback_first_token_timeout: Option<Duration>,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let Some((pool, key)) = pool else {
        return execute_fresh_response_create_request_stream_with_retries(
            request,
            fallback_first_token_timeout,
            WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS,
        )
        .await;
    };

    match pool.acquire(&key).await {
        WebSocketPoolAcquire::Reused(connection) => {
            let result = execute_pooled_response_create_request_stream(
                request,
                pool.clone(),
                key,
                *connection,
            )
            .await;
            match result {
                Err(CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                    ..
                })
                | Err(CodexWebSocketExchangeError::FirstTokenTimeout { .. }) => {
                    let mut exchange = execute_fresh_response_create_request_stream_with_retries(
                        request,
                        pool.first_token_timeout(),
                        WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS,
                    )
                    .await?;
                    exchange.pool_decision = Some(WebSocketPoolDecision::retry_after_stale_reuse());
                    Ok(exchange)
                }
                Ok(mut exchange) => {
                    exchange.pool_decision = Some(WebSocketPoolDecision::reuse());
                    Ok(exchange)
                }
                Err(error) => Err(error),
            }
        }
        WebSocketPoolAcquire::FreshReserved => {
            let result = execute_fresh_pooled_response_create_request_stream(
                request,
                pool.clone(),
                key.clone(),
            )
            .await;
            match result {
                Ok(mut exchange) => {
                    exchange.pool_decision = Some(WebSocketPoolDecision::new());
                    Ok(exchange)
                }
                Err(error) if is_first_token_timeout(&error) => {
                    pool.discard(&key).await;
                    let mut exchange = execute_fresh_response_create_request_stream_with_retries(
                        request,
                        pool.first_token_timeout(),
                        1,
                    )
                    .await?;
                    exchange.pool_decision = Some(WebSocketPoolDecision::new());
                    Ok(exchange)
                }
                Err(error) => {
                    pool.discard(&key).await;
                    Err(error)
                }
            }
        }
        WebSocketPoolAcquire::Bypass(reason) => {
            let mut exchange = execute_fresh_response_create_request_stream_with_retries(
                request,
                pool.first_token_timeout(),
                WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS,
            )
            .await?;
            exchange.pool_decision = Some(WebSocketPoolDecision::bypass(reason));
            Ok(exchange)
        }
    }
}

async fn execute_fresh_pooled_response_create_request(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
    started_at: Instant,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let (websocket, response) =
        connect_pumped_websocket(request.connection(), pool.keepalive()).await?;
    let first_token_started_at = Instant::now();
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let now = Instant::now();
    let metadata = websocket_connection_metadata(&response);
    let (exchange, websocket, metadata) = collect_websocket_response(
        websocket,
        metadata,
        false,
        started_at,
        first_token_started_at,
        pool.first_token_timeout(),
    )
    .await?;
    pool.put(
        key,
        PooledWebSocketConnection {
            websocket,
            metadata: reusable_websocket_metadata(metadata),
            created_at: now,
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
    let (mut websocket, response) =
        connect_pumped_websocket(request.connection(), pool.keepalive()).await?;
    let first_token_started_at = Instant::now();
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let now = Instant::now();
    let mut metadata = websocket_connection_metadata(&response);
    let prefetched_frames = match prefetch_stream_frames_until_output_or_terminal(
        &mut websocket,
        &mut metadata,
        first_token_started_at,
        pool.first_token_timeout(),
    )
    .await
    {
        Ok(prefetched_frames) => prefetched_frames,
        Err(error) => {
            websocket.close().await;
            return Err(error);
        }
    };
    Ok(stream_websocket_response(
        websocket,
        metadata,
        Some(WebSocketStreamPoolReturn {
            pool,
            key,
            created_at: now,
        }),
        prefetched_frames,
    ))
}

async fn execute_pooled_response_create_request(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
    connection: PooledWebSocketConnection,
    started_at: Instant,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let first_token_started_at = Instant::now();
    if let Err(error) = connection
        .websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await
    {
        pool.discard(&key).await;
        return Err(
            CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                message: error.to_string(),
            },
        );
    }

    let created_at = connection.created_at;
    match collect_websocket_response(
        connection.websocket,
        reusable_websocket_metadata(connection.metadata),
        true,
        started_at,
        first_token_started_at,
        pool.first_token_timeout(),
    )
    .await
    {
        Ok((exchange, websocket, metadata)) => {
            pool.put(
                key,
                PooledWebSocketConnection {
                    websocket,
                    metadata: reusable_websocket_metadata(metadata),
                    created_at,
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
    connection: PooledWebSocketConnection,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let mut websocket = connection.websocket;
    let mut metadata = reusable_websocket_metadata(connection.metadata);
    let created_at = connection.created_at;
    let first_token_started_at = Instant::now();
    if let Err(error) = websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await
    {
        pool.discard(&key).await;
        return Err(
            CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                message: error.to_string(),
            },
        );
    }

    let prefetched_frames = match prefetch_stream_frames_until_output_or_terminal(
        &mut websocket,
        &mut metadata,
        first_token_started_at,
        pool.first_token_timeout(),
    )
    .await
    {
        Ok(prefetched_frames) => prefetched_frames,
        Err(error) => {
            pool.discard(&key).await;
            websocket.close().await;
            return Err(reused_stream_prefetch_error(error));
        }
    };

    Ok(stream_websocket_response(
        websocket,
        metadata,
        Some(WebSocketStreamPoolReturn {
            pool,
            key,
            created_at,
        }),
        prefetched_frames,
    ))
}
