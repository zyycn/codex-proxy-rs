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
use tokio_tungstenite::{Connector, connect_async_tls_with_config};
use tungstenite::{
    self, Message,
    extensions::{ExtensionsConfig, compression::deflate::DeflateConfig},
    handshake::client::Request as WsRequest,
    http::Response as WsResponse,
    protocol::WebSocketConfig,
};

use crate::infra::time::china_filename_timestamp_millis;
use crate::upstream::openai::protocol::events::{self, TokenUsage};
use crate::upstream::openai::protocol::responses::{
    CodexResponsesRequest, PreviousResponseScope, StreamCommitPolicy,
    response_body_has_first_output,
};
use crate::upstream::openai::protocol::sse::SseError;
use crate::upstream::openai::protocol::websocket::{
    ClassifiedWebSocketError, OpeningAuditHeader, OpeningAuditSnapshot, WebSocketAuditArtifact,
    classify_websocket_error_frame, is_terminal_websocket_event,
    retry_after_seconds_from_wrapped_error_headers, websocket_event_to_sse_frame,
    websocket_event_type, websocket_metadata_headers, websocket_metadata_turn_state,
    websocket_response_completed_id, websocket_response_create_payload_text,
};

use super::websocket_pool::{
    CodexWebSocketConnectionMetadata, CodexWebSocketPool, CodexWebSocketPoolKey,
    PooledWebSocketConnection, WebSocketContinuationState, WebSocketPoolAcquire,
    WebSocketPoolDecision, WebSocketPoolLease,
};
use super::websocket_pump::{PumpKeepalive, PumpedWebSocket, RawWsStream};
use super::{client::CodexResponseMetadata, diagnostics::CodexUpstreamDiagnostics, response_meta};
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
    pub(super) continuation: WebSocketContinuationRequirement,
    pub(super) stream_commit_policy: StreamCommitPolicy,
}

/// 当前 WebSocket 请求对 previous response 状态的要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketContinuationRequirement {
    /// 不依赖任何已有响应状态。
    NewChain,
    /// 上游已持久化 response，可在新连接 hydration。
    Persisted { response_id: String },
    /// 代理没有所有权信息，只允许 dispatch 选定的单个账号原样尝试。
    ExternalUnknown { response_id: String },
    /// `store=false` response，只能在拥有该 ID 的原连接续接。
    ConnectionLocal { response_id: String },
}

impl WebSocketContinuationRequirement {
    fn from_request(request: &CodexResponsesRequest) -> Self {
        match request.previous_response_id() {
            None => Self::NewChain,
            Some(response_id)
                if request.previous_response_scope == Some(PreviousResponseScope::Persisted)
                    || (request.previous_response_scope.is_none() && request.store()) =>
            {
                Self::Persisted {
                    response_id: response_id.to_string(),
                }
            }
            Some(response_id)
                if request.previous_response_scope
                    == Some(PreviousResponseScope::ExternalUnknown) =>
            {
                Self::ExternalUnknown {
                    response_id: response_id.to_string(),
                }
            }
            Some(response_id) => Self::ConnectionLocal {
                response_id: response_id.to_string(),
            },
        }
    }

    fn permits_fresh_connection(&self) -> bool {
        matches!(
            self,
            Self::NewChain | Self::Persisted { .. } | Self::ExternalUnknown { .. }
        )
    }
}

/// 连接本地 previous response 无法满足的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviousResponseUnavailableReason {
    PoolUnavailable,
    FreshConnectionRequired,
    ConnectionBusy,
    LatestResponseMismatch,
    ReusedConnectionLost,
    UpstreamRejected,
}

impl fmt::Display for PreviousResponseUnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PoolUnavailable => "pool_unavailable",
            Self::FreshConnectionRequired => "fresh_connection_required",
            Self::ConnectionBusy => "connection_busy",
            Self::LatestResponseMismatch => "latest_response_mismatch",
            Self::ReusedConnectionLost => "reused_connection_lost",
            Self::UpstreamRejected => "upstream_rejected",
        })
    }
}

#[path = "websocket_frames.rs"]
mod frames;

pub use frames::{
    CodexWebSocketExchange, CodexWebSocketExchangeError, CodexWebSocketRateLimitHeaderUpdates,
    CodexWebSocketSseStream, CodexWebSocketStreamingExchange, CodexWebSocketTurnStateUpdate,
    CodexWebSocketUpstreamError, execute_response_create_request,
};
use frames::{
    WebSocketStreamPoolReturn, WebSocketTerminalKind, audit_header_value,
    collect_websocket_response, is_initial_event_timeout,
    prefetch_stream_frames_until_output_or_terminal, reusable_websocket_metadata,
    reused_stream_prefetch_error, stream_websocket_response, websocket_audit_file_name,
    websocket_connection_metadata,
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

    /// 返回连接续接要求。
    pub fn continuation(&self) -> &WebSocketContinuationRequirement {
        &self.continuation
    }

    pub fn stream_commit_policy(&self) -> StreamCommitPolicy {
        self.stream_commit_policy
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
            continuation: WebSocketContinuationRequirement::from_request(request),
            stream_commit_policy: request.stream_commit_policy,
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
    initial_event_timeout: Option<Duration>,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let (websocket, response) =
        connect_pumped_websocket(request.connection(), PumpKeepalive::disabled()).await?;
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let metadata = websocket_connection_metadata(&response);
    let (exchange, websocket, _metadata, _continuation, _terminal) = collect_websocket_response(
        websocket,
        metadata,
        WebSocketContinuationState::default(),
        false,
        started_at,
        initial_event_timeout,
    )
    .await?;
    websocket.close().await;
    Ok(exchange)
}

async fn execute_fresh_response_create_request_stream(
    request: &CodexWebSocketRequest,
    initial_event_timeout: Option<Duration>,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let (mut websocket, response) =
        connect_pumped_websocket(request.connection(), PumpKeepalive::disabled()).await?;
    websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await?;

    let mut metadata = websocket_connection_metadata(&response);
    let prefetched_frames = if should_prefetch_until_output_or_terminal(request) {
        match prefetch_stream_frames_until_output_or_terminal(
            &mut websocket,
            &mut metadata,
            initial_event_timeout,
        )
        .await
        {
            Ok(prefetched_frames) => prefetched_frames,
            Err(error) => {
                websocket.close().await;
                return Err(error);
            }
        }
    } else {
        Vec::new()
    };
    Ok(stream_websocket_response(
        websocket,
        metadata,
        None,
        prefetched_frames,
        initial_event_timeout,
    ))
}

async fn execute_fresh_response_create_request_with_retries(
    request: &CodexWebSocketRequest,
    started_at: Instant,
    initial_event_timeout: Option<Duration>,
    attempts: usize,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let attempts = attempts.max(1);
    for attempt in 1..=attempts {
        match execute_fresh_response_create_request(request, started_at, initial_event_timeout)
            .await
        {
            Ok(exchange) => return Ok(exchange),
            Err(error) if attempt < attempts && is_initial_event_timeout(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("fresh websocket retry loop always returns");
}

async fn execute_fresh_response_create_request_stream_with_retries(
    request: &CodexWebSocketRequest,
    initial_event_timeout: Option<Duration>,
    attempts: usize,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let attempts = attempts.max(1);
    for attempt in 1..=attempts {
        match execute_fresh_response_create_request_stream(request, initial_event_timeout).await {
            Ok(exchange) => return Ok(exchange),
            Err(error) if attempt < attempts && is_initial_event_timeout(&error) => continue,
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
    fallback_initial_event_timeout: Option<Duration>,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let Some((pool, key)) = pool else {
        if !request.continuation().permits_fresh_connection() {
            return Err(continuation_unavailable(
                PreviousResponseUnavailableReason::PoolUnavailable,
            ));
        }
        return execute_fresh_response_create_request_with_retries(
            request,
            started_at,
            fallback_initial_event_timeout,
            WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS,
        )
        .await;
    };

    match pool.acquire(&key).await {
        WebSocketPoolAcquire::Reused { connection, lease } => {
            if let WebSocketContinuationRequirement::ConnectionLocal { response_id } =
                request.continuation()
                && connection.continuation.latest_response_id() != Some(response_id.as_str())
            {
                lease.put(*connection).await;
                return Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::LatestResponseMismatch,
                ));
            }
            let result = execute_pooled_response_create_request(
                request,
                pool,
                lease,
                *connection,
                started_at,
            )
            .await;
            match result {
                Err(CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                    ..
                })
                | Err(CodexWebSocketExchangeError::InitialEventTimeout { .. }) => {
                    if !request.continuation().permits_fresh_connection() {
                        return Err(continuation_unavailable(
                            PreviousResponseUnavailableReason::ReusedConnectionLost,
                        ));
                    }
                    let mut exchange = execute_fresh_response_create_request_with_retries(
                        request,
                        started_at,
                        pool.initial_event_timeout(),
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
        WebSocketPoolAcquire::FreshReserved(lease) => {
            if !request.continuation().permits_fresh_connection() {
                lease.discard().await;
                return Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::FreshConnectionRequired,
                ));
            }
            let result =
                execute_fresh_pooled_response_create_request(request, pool, lease, started_at)
                    .await;
            match result {
                Ok(mut exchange) => {
                    exchange.pool_decision = Some(WebSocketPoolDecision::new());
                    Ok(exchange)
                }
                Err(error) if is_initial_event_timeout(&error) => {
                    let mut exchange = execute_fresh_response_create_request_with_retries(
                        request,
                        started_at,
                        pool.initial_event_timeout(),
                        1,
                    )
                    .await?;
                    exchange.pool_decision = Some(WebSocketPoolDecision::new());
                    Ok(exchange)
                }
                Err(error) => Err(error),
            }
        }
        WebSocketPoolAcquire::Bypass(reason) => {
            if !request.continuation().permits_fresh_connection() {
                return Err(continuation_unavailable(match reason {
                    super::websocket_pool::WebSocketPoolBypassReason::Busy => {
                        PreviousResponseUnavailableReason::ConnectionBusy
                    }
                    super::websocket_pool::WebSocketPoolBypassReason::Disabled
                    | super::websocket_pool::WebSocketPoolBypassReason::Cap => {
                        PreviousResponseUnavailableReason::PoolUnavailable
                    }
                }));
            }
            let mut exchange = execute_fresh_response_create_request_with_retries(
                request,
                started_at,
                pool.initial_event_timeout(),
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
    fallback_initial_event_timeout: Option<Duration>,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let Some((pool, key)) = pool else {
        if !request.continuation().permits_fresh_connection() {
            return Err(continuation_unavailable(
                PreviousResponseUnavailableReason::PoolUnavailable,
            ));
        }
        return execute_fresh_response_create_request_stream_with_retries(
            request,
            fallback_initial_event_timeout,
            WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS,
        )
        .await;
    };

    match pool.acquire(&key).await {
        WebSocketPoolAcquire::Reused { connection, lease } => {
            if let WebSocketContinuationRequirement::ConnectionLocal { response_id } =
                request.continuation()
                && connection.continuation.latest_response_id() != Some(response_id.as_str())
            {
                lease.put(*connection).await;
                return Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::LatestResponseMismatch,
                ));
            }
            let result =
                execute_pooled_response_create_request_stream(request, pool, lease, *connection)
                    .await;
            match result {
                Err(CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                    ..
                })
                | Err(CodexWebSocketExchangeError::InitialEventTimeout { .. }) => {
                    if !request.continuation().permits_fresh_connection() {
                        return Err(continuation_unavailable(
                            PreviousResponseUnavailableReason::ReusedConnectionLost,
                        ));
                    }
                    let mut exchange = execute_fresh_response_create_request_stream_with_retries(
                        request,
                        pool.initial_event_timeout(),
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
        WebSocketPoolAcquire::FreshReserved(lease) => {
            if !request.continuation().permits_fresh_connection() {
                lease.discard().await;
                return Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::FreshConnectionRequired,
                ));
            }
            let result =
                execute_fresh_pooled_response_create_request_stream(request, pool, lease).await;
            match result {
                Ok(mut exchange) => {
                    exchange.pool_decision = Some(WebSocketPoolDecision::new());
                    Ok(exchange)
                }
                Err(error) if is_initial_event_timeout(&error) => {
                    let mut exchange = execute_fresh_response_create_request_stream_with_retries(
                        request,
                        pool.initial_event_timeout(),
                        1,
                    )
                    .await?;
                    exchange.pool_decision = Some(WebSocketPoolDecision::new());
                    Ok(exchange)
                }
                Err(error) => Err(error),
            }
        }
        WebSocketPoolAcquire::Bypass(reason) => {
            if !request.continuation().permits_fresh_connection() {
                return Err(continuation_unavailable(match reason {
                    super::websocket_pool::WebSocketPoolBypassReason::Busy => {
                        PreviousResponseUnavailableReason::ConnectionBusy
                    }
                    super::websocket_pool::WebSocketPoolBypassReason::Disabled
                    | super::websocket_pool::WebSocketPoolBypassReason::Cap => {
                        PreviousResponseUnavailableReason::PoolUnavailable
                    }
                }));
            }
            let mut exchange = execute_fresh_response_create_request_stream_with_retries(
                request,
                pool.initial_event_timeout(),
                WEBSOCKET_FIRST_TOKEN_FRESH_RETRY_ATTEMPTS,
            )
            .await?;
            exchange.pool_decision = Some(WebSocketPoolDecision::bypass(reason));
            Ok(exchange)
        }
    }
}

fn continuation_unavailable(
    reason: PreviousResponseUnavailableReason,
) -> CodexWebSocketExchangeError {
    CodexWebSocketExchangeError::ContinuationUnavailable { reason }
}

async fn execute_fresh_pooled_response_create_request(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    lease: WebSocketPoolLease,
    started_at: Instant,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let (websocket, response) =
        match connect_pumped_websocket(request.connection(), pool.keepalive()).await {
            Ok(connected) => connected,
            Err(error) => {
                lease.discard().await;
                return Err(error);
            }
        };
    if let Err(error) = websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await
    {
        lease.discard().await;
        return Err(error.into());
    }

    let now = tokio::time::Instant::now();
    let metadata = websocket_connection_metadata(&response);
    let (exchange, websocket, metadata, continuation, terminal) = match collect_websocket_response(
        websocket,
        metadata,
        WebSocketContinuationState::default(),
        false,
        started_at,
        pool.initial_event_timeout(),
    )
    .await
    {
        Ok(exchange) => exchange,
        Err(error) => {
            lease.discard().await;
            return Err(error);
        }
    };
    match terminal {
        WebSocketTerminalKind::Completed => {
            lease
                .put(PooledWebSocketConnection {
                    websocket,
                    metadata: reusable_websocket_metadata(metadata),
                    continuation,
                    created_at: now,
                })
                .await;
        }
        WebSocketTerminalKind::Incomplete | WebSocketTerminalKind::Failed => {
            lease.discard().await;
            websocket.close().await;
        }
    }
    Ok(exchange)
}

async fn execute_fresh_pooled_response_create_request_stream(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    lease: WebSocketPoolLease,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let (mut websocket, response) =
        match connect_pumped_websocket(request.connection(), pool.keepalive()).await {
            Ok(connected) => connected,
            Err(error) => {
                lease.discard().await;
                return Err(error);
            }
        };
    if let Err(error) = websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await
    {
        lease.discard().await;
        return Err(error.into());
    }

    let now = tokio::time::Instant::now();
    let mut metadata = websocket_connection_metadata(&response);
    let prefetched_frames = if should_prefetch_until_output_or_terminal(request) {
        match prefetch_stream_frames_until_output_or_terminal(
            &mut websocket,
            &mut metadata,
            pool.initial_event_timeout(),
        )
        .await
        {
            Ok(prefetched_frames) => prefetched_frames,
            Err(error) => {
                websocket.close().await;
                lease.discard().await;
                return Err(error);
            }
        }
    } else {
        Vec::new()
    };
    Ok(stream_websocket_response(
        websocket,
        metadata,
        Some(WebSocketStreamPoolReturn {
            lease,
            created_at: now,
            continuation: WebSocketContinuationState::default(),
        }),
        prefetched_frames,
        pool.initial_event_timeout(),
    ))
}

async fn execute_pooled_response_create_request(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    lease: WebSocketPoolLease,
    connection: PooledWebSocketConnection,
    started_at: Instant,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    if let Err(error) = connection
        .websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await
    {
        lease.discard().await;
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
        connection.continuation,
        true,
        started_at,
        pool.initial_event_timeout(),
    )
    .await
    {
        Ok((exchange, websocket, metadata, continuation, terminal)) => {
            match terminal {
                WebSocketTerminalKind::Completed => {
                    lease
                        .put(PooledWebSocketConnection {
                            websocket,
                            metadata: reusable_websocket_metadata(metadata),
                            continuation,
                            created_at,
                        })
                        .await;
                }
                WebSocketTerminalKind::Incomplete | WebSocketTerminalKind::Failed => {
                    lease.discard().await;
                    websocket.close().await;
                }
            }
            Ok(exchange)
        }
        Err(error) => {
            lease.discard().await;
            Err(error)
        }
    }
}

async fn execute_pooled_response_create_request_stream(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    lease: WebSocketPoolLease,
    connection: PooledWebSocketConnection,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let mut websocket = connection.websocket;
    let mut metadata = reusable_websocket_metadata(connection.metadata);
    let created_at = connection.created_at;
    let continuation = connection.continuation;
    if let Err(error) = websocket
        .send(Message::Text(request.payload_text().to_string().into()))
        .await
    {
        lease.discard().await;
        return Err(
            CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput {
                message: error.to_string(),
            },
        );
    }

    let prefetched_frames = if should_prefetch_until_output_or_terminal(request) {
        match prefetch_stream_frames_until_output_or_terminal(
            &mut websocket,
            &mut metadata,
            pool.initial_event_timeout(),
        )
        .await
        {
            Ok(prefetched_frames) => prefetched_frames,
            Err(error) => {
                lease.discard().await;
                websocket.close().await;
                return Err(reused_stream_prefetch_error(error));
            }
        }
    } else {
        Vec::new()
    };

    Ok(stream_websocket_response(
        websocket,
        metadata,
        Some(WebSocketStreamPoolReturn {
            lease,
            created_at,
            continuation,
        }),
        prefetched_frames,
        pool.initial_event_timeout(),
    ))
}

fn should_prefetch_until_output_or_terminal(request: &CodexWebSocketRequest) -> bool {
    matches!(
        request.stream_commit_policy(),
        StreamCommitPolicy::UntilOutputOrTerminal
    )
}
