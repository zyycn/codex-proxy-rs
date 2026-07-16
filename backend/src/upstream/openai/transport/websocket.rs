//! Codex WebSocket 连接建立（关键函数）。

use std::{
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
use tokio::sync::{mpsc, oneshot};
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
    CodexResponsesRequest, TransportRequirement, response_body_has_semantic_output,
    transport_requirement,
};
use crate::upstream::openai::protocol::sse::SseError;
use crate::upstream::openai::protocol::websocket::{
    OpeningAuditHeader, OpeningAuditSnapshot, WebSocketAuditArtifact, is_terminal_websocket_event,
    websocket_event_to_sse_frame, websocket_event_type, websocket_metadata_headers,
    websocket_metadata_turn_state, websocket_response_completed_id,
    websocket_response_create_payload_text,
};

use super::websocket_breaker::{
    WebSocketOriginBreaker, WebSocketOriginBreakerDecision, WebSocketOriginBreakerPermit,
    WebSocketOriginFastPathReporter,
};
use super::websocket_pool::{
    CodexWebSocketConnectionMetadata, CodexWebSocketPool, CodexWebSocketPoolKey,
    PooledWebSocketConnection, WebSocketContinuationState, WebSocketPoolAcquire,
    WebSocketPoolBypassReason, WebSocketPoolConnectLease, WebSocketPoolConnectOutcome,
    WebSocketPoolDecision, WebSocketPoolLease,
};
use super::websocket_pump::{PumpExitReason, PumpKeepalive, PumpedWebSocket, RawWsStream};
use super::{client::CodexResponseMetadata, diagnostics::CodexUpstreamDiagnostics, response_meta};
use uuid::Uuid;

const REDACTED_HEADER_VALUE: &str = "<redacted>";
const CODEX_RESPONSES_PATH: &str = "/codex/responses";
const WEBSOCKET_EXTENSIONS: &str = "permessage-deflate; client_max_window_bits";
const WEBSOCKET_RECEIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const WEBSOCKET_ACTIVE_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const WEBSOCKET_FAST_PATH_BUDGET: Duration = Duration::from_millis(800);
const WEBSOCKET_SEND_TIMEOUT: Duration = WEBSOCKET_ACTIVE_STREAM_IDLE_TIMEOUT;
const WEBSOCKET_STREAM_BUFFER: usize = 16;
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
        let response_id = request.previous_response_id().map(ToString::to_string);
        match (transport_requirement(request), response_id) {
            (TransportRequirement::ExactWebSocketContinuation, Some(response_id)) => {
                Self::ConnectionLocal { response_id }
            }
            (TransportRequirement::PersistedContinuation, Some(response_id)) => {
                Self::Persisted { response_id }
            }
            (TransportRequirement::ExternalUnknown, Some(response_id)) => {
                Self::ExternalUnknown { response_id }
            }
            _ => Self::NewChain,
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
    collect_websocket_response, reusable_websocket_metadata, stream_websocket_response,
    websocket_audit_file_name, websocket_connection_metadata,
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
        .map_err(|error| {
            CodexWebSocketExchangeError::Connect(tungstenite::Error::Io(std::io::Error::other(
                error,
            )))
        })?
        .map(Connector::Rustls);
    let result = timeout(
        WEBSOCKET_CONNECT_TIMEOUT,
        connect_async_tls_with_config(request, Some(websocket_config()), false, connector),
    )
    .await
    .map_err(|_| CodexWebSocketExchangeError::ConnectTimeout {
        timeout: WEBSOCKET_CONNECT_TIMEOUT,
    })?;
    match result {
        Ok((websocket, response)) => Ok((websocket, response)),
        Err(tungstenite::Error::Http(response)) => Err(websocket_opening_error(response.as_ref())),
        Err(error) => Err(CodexWebSocketExchangeError::Connect(error)),
    }
}

async fn send_websocket_request(
    websocket: &PumpedWebSocket,
    payload_text: &str,
) -> Result<(), CodexWebSocketExchangeError> {
    timeout(
        WEBSOCKET_SEND_TIMEOUT,
        websocket.send(Message::Text(payload_text.to_string().into())),
    )
    .await
    .map_err(|_| CodexWebSocketExchangeError::SendTimeout {
        timeout: WEBSOCKET_SEND_TIMEOUT,
    })??;
    Ok(())
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
// Prepare-before-send transport boundary
// ---------------------------------------------------------------------------

/// 尚未发送 `response.create` 的 WebSocket。只有该类型可以安全切换到 HTTP。
pub(crate) struct PreparedWebSocket {
    connection: PooledWebSocketConnection,
    lease: Option<WebSocketPoolLease>,
    reused: bool,
    pool_decision: Option<WebSocketPoolDecision>,
    connect_elapsed: Option<Duration>,
    decision_wait_elapsed: Duration,
    initial_event_timeout: Option<Duration>,
}

impl PreparedWebSocket {
    pub(crate) fn pool_decision(&self) -> Option<WebSocketPoolDecision> {
        self.pool_decision
    }

    pub(crate) fn reused(&self) -> bool {
        self.reused
    }

    pub(crate) fn connect_elapsed(&self) -> Option<Duration> {
        self.connect_elapsed
    }

    pub(crate) fn decision_wait_elapsed(&self) -> Duration {
        self.decision_wait_elapsed
    }
}

/// 只建立或租用 WebSocket，不发送 payload。
pub(crate) async fn prepare_response_create_request_with_pool(
    request: &CodexWebSocketRequest,
    pool: Option<(&CodexWebSocketPool, CodexWebSocketPoolKey)>,
    breaker: &WebSocketOriginBreaker,
    origin_key: &str,
    fast_path_budget: Option<Duration>,
    require_pool: bool,
    fallback_initial_event_timeout: Option<Duration>,
) -> Result<PreparedWebSocket, CodexWebSocketExchangeError> {
    let decision_started_at = Instant::now();
    let Some((pool, key)) = pool else {
        if require_pool || !request.continuation().permits_fresh_connection() {
            return Err(continuation_unavailable(
                PreviousResponseUnavailableReason::PoolUnavailable,
            ));
        }
        return prepare_unpooled_websocket(
            request,
            breaker,
            origin_key,
            fast_path_budget,
            fallback_initial_event_timeout,
            decision_started_at,
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
            Ok(PreparedWebSocket {
                connection: *connection,
                lease: Some(lease),
                reused: true,
                pool_decision: Some(WebSocketPoolDecision::reuse()),
                connect_elapsed: None,
                decision_wait_elapsed: decision_started_at.elapsed(),
                initial_event_timeout: pool.initial_event_timeout(),
            })
        }
        WebSocketPoolAcquire::Connect(connect_lease) => {
            if !request.continuation().permits_fresh_connection() {
                connect_lease.failed().await;
                return Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::FreshConnectionRequired,
                ));
            }
            prepare_pooled_websocket(
                request,
                pool,
                connect_lease,
                breaker,
                origin_key,
                fast_path_budget,
                decision_started_at,
            )
            .await
        }
        WebSocketPoolAcquire::Wait(waiter) => {
            if require_pool || !request.continuation().permits_fresh_connection() {
                return Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::ConnectionBusy,
                ));
            }
            let outcome =
                wait_for_shared_connect(waiter, fast_path_budget, decision_started_at).await?;
            match outcome {
                WebSocketPoolConnectOutcome::Ready => Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::ConnectionBusy,
                )),
                WebSocketPoolConnectOutcome::Failed | WebSocketPoolConnectOutcome::Pending => {
                    Err(CodexWebSocketExchangeError::SharedConnectFailed)
                }
            }
        }
        WebSocketPoolAcquire::Bypass(reason) => {
            Err(continuation_unavailable(bypass_unavailable_reason(reason)))
        }
    }
}

async fn prepare_unpooled_websocket(
    request: &CodexWebSocketRequest,
    breaker: &WebSocketOriginBreaker,
    origin_key: &str,
    fast_path_budget: Option<Duration>,
    initial_event_timeout: Option<Duration>,
    decision_started_at: Instant,
) -> Result<PreparedWebSocket, CodexWebSocketExchangeError> {
    let permit = acquire_breaker_permit(breaker, origin_key)?;
    let connected = connect_with_budget(
        request.connection(),
        PumpKeepalive::disabled(),
        fast_path_budget,
    )
    .await;
    let (connection, connect_elapsed) = finish_breaker_attempt(permit, connected)?;
    Ok(PreparedWebSocket {
        connection,
        lease: None,
        reused: false,
        pool_decision: None,
        connect_elapsed: Some(connect_elapsed),
        decision_wait_elapsed: decision_started_at.elapsed(),
        initial_event_timeout,
    })
}

async fn prepare_pooled_websocket(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    connect_lease: WebSocketPoolConnectLease,
    breaker: &WebSocketOriginBreaker,
    origin_key: &str,
    fast_path_budget: Option<Duration>,
    decision_started_at: Instant,
) -> Result<PreparedWebSocket, CodexWebSocketExchangeError> {
    let permit = match acquire_breaker_permit(breaker, origin_key) {
        Ok(permit) => permit,
        Err(error) => {
            connect_lease.failed().await;
            return Err(error);
        }
    };
    let mut waiter = start_pooled_websocket_connect(
        request.connection().clone(),
        pool.clone(),
        connect_lease,
        permit,
    );
    let connect_elapsed = waiter.wait(fast_path_budget).await?;
    let Some((connection, lease)) = pool.take_idle(waiter.key()).await else {
        return Err(continuation_unavailable(
            PreviousResponseUnavailableReason::PoolUnavailable,
        ));
    };
    Ok(PreparedWebSocket {
        connection: *connection,
        lease: Some(lease),
        reused: false,
        pool_decision: Some(WebSocketPoolDecision::new()),
        connect_elapsed: Some(connect_elapsed),
        decision_wait_elapsed: decision_started_at.elapsed(),
        initial_event_timeout: pool.initial_event_timeout(),
    })
}

struct PooledWebSocketConnectWaiter {
    key: CodexWebSocketPoolKey,
    started_at: tokio::time::Instant,
    receiver: oneshot::Receiver<Result<Duration, CodexWebSocketExchangeError>>,
    fast_path_reporter: WebSocketOriginFastPathReporter,
}

impl PooledWebSocketConnectWaiter {
    fn key(&self) -> &CodexWebSocketPoolKey {
        &self.key
    }

    async fn wait(
        &mut self,
        fast_path_budget: Option<Duration>,
    ) -> Result<Duration, CodexWebSocketExchangeError> {
        let received = match fast_path_budget {
            Some(budget) => {
                let remaining = budget.saturating_sub(self.started_at.elapsed());
                match timeout(remaining, &mut self.receiver).await {
                    Ok(received) => received,
                    Err(_) => {
                        self.fast_path_reporter.missed();
                        return Err(CodexWebSocketExchangeError::FastPathTimeout {
                            timeout: budget,
                        });
                    }
                }
            }
            None => (&mut self.receiver).await,
        };
        received.map_err(|_| CodexWebSocketExchangeError::SharedConnectFailed)?
    }
}

fn start_pooled_websocket_connect(
    connection: CodexWebSocketConnection,
    pool: CodexWebSocketPool,
    connect_lease: WebSocketPoolConnectLease,
    permit: WebSocketOriginBreakerPermit,
) -> PooledWebSocketConnectWaiter {
    let key = connect_lease.key().clone();
    let started_at = connect_lease.started_at();
    let cancellation = connect_lease.cancellation_token();
    let fast_path_reporter = permit.fast_path_reporter();
    let keepalive = pool.keepalive();
    let task_key = key.clone();
    let (sender, receiver) = oneshot::channel();
    pool.spawn_connect_task(async move {
        let connected = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                permit.cancel();
                let _ = sender.send(Err(CodexWebSocketExchangeError::SharedConnectFailed));
                connect_lease.failed().await;
                tracing::info!(
                    account_id = task_key.account_id(),
                    conversation_id_hash = task_key.conversation_id_hash(),
                    ws_preconnect_duration_ms = duration_millis_u64(started_at.elapsed()),
                    ws_preconnect_outcome = "cancelled",
                    "WebSocket pool connect finished"
                );
                return;
            }
            result = connect_with_budget(&connection, keepalive, None) => result,
        };
        match finish_breaker_attempt(permit, connected) {
            Ok((connection, connect_elapsed)) => {
                match connect_lease.connected_idle(connection).await {
                    Ok(()) => {
                        let foreground_waiting = sender.send(Ok(connect_elapsed)).is_ok();
                        tracing::info!(
                            account_id = task_key.account_id(),
                            conversation_id_hash = task_key.conversation_id_hash(),
                            ws_preconnect_duration_ms = duration_millis_u64(connect_elapsed),
                            foreground_waiting,
                            ws_preconnect_outcome = "ready",
                            "WebSocket pool connect finished"
                        );
                    }
                    Err(connection) => {
                        connection.websocket.close().await;
                        let _ = sender.send(Err(continuation_unavailable(
                            PreviousResponseUnavailableReason::PoolUnavailable,
                        )));
                        tracing::info!(
                            account_id = task_key.account_id(),
                            conversation_id_hash = task_key.conversation_id_hash(),
                            ws_preconnect_duration_ms = duration_millis_u64(connect_elapsed),
                            ws_preconnect_outcome = "rejected",
                            "WebSocket pool connect finished"
                        );
                    }
                }
            }
            Err(error) => {
                let error_message = error.to_string();
                // 先交付 opening 原始错误，避免连接池清理侵占前台 fast-path 预算。
                let foreground_waiting = sender.send(Err(error)).is_ok();
                connect_lease.failed().await;
                tracing::warn!(
                    account_id = task_key.account_id(),
                    conversation_id_hash = task_key.conversation_id_hash(),
                    ws_preconnect_duration_ms = duration_millis_u64(started_at.elapsed()),
                    foreground_waiting,
                    error = %error_message,
                    ws_preconnect_outcome = "failed",
                    "WebSocket pool connect finished"
                );
            }
        }
    });
    PooledWebSocketConnectWaiter {
        key,
        started_at,
        receiver,
        fast_path_reporter,
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

async fn connect_with_budget(
    connection: &CodexWebSocketConnection,
    keepalive: PumpKeepalive,
    fast_path_budget: Option<Duration>,
) -> Result<(PooledWebSocketConnection, Duration), CodexWebSocketExchangeError> {
    let started_at = Instant::now();
    let connected = match fast_path_budget {
        Some(budget) => timeout(budget, connect_pumped_websocket(connection, keepalive))
            .await
            .map_err(|_| CodexWebSocketExchangeError::FastPathTimeout { timeout: budget })?,
        None => connect_pumped_websocket(connection, keepalive).await,
    }?;
    let (websocket, response) = connected;
    Ok((
        PooledWebSocketConnection {
            websocket,
            metadata: websocket_connection_metadata(&response),
            continuation: WebSocketContinuationState::default(),
            created_at: tokio::time::Instant::now(),
        },
        started_at.elapsed(),
    ))
}

fn acquire_breaker_permit(
    breaker: &WebSocketOriginBreaker,
    origin_key: &str,
) -> Result<WebSocketOriginBreakerPermit, CodexWebSocketExchangeError> {
    match breaker.try_acquire(origin_key) {
        WebSocketOriginBreakerDecision::Allowed(permit) => Ok(permit),
        WebSocketOriginBreakerDecision::Open => Err(CodexWebSocketExchangeError::OriginCircuitOpen),
        WebSocketOriginBreakerDecision::HalfOpenBusy => {
            Err(CodexWebSocketExchangeError::OriginHalfOpenBusy)
        }
    }
}

fn finish_breaker_attempt(
    permit: WebSocketOriginBreakerPermit,
    connected: Result<(PooledWebSocketConnection, Duration), CodexWebSocketExchangeError>,
) -> Result<(PooledWebSocketConnection, Duration), CodexWebSocketExchangeError> {
    match connected {
        Ok(connection) => {
            permit.succeed();
            Ok(connection)
        }
        Err(error @ CodexWebSocketExchangeError::FastPathTimeout { .. }) => {
            permit.fast_timeout();
            Err(error)
        }
        Err(CodexWebSocketExchangeError::Upstream(upstream)) if upstream.status_code < 500 => {
            // 账号或请求级 opening 响应证明 origin 可达，不得污染 transport 熔断器。
            permit.succeed();
            Err(CodexWebSocketExchangeError::Upstream(upstream))
        }
        Err(error) => {
            permit.fail();
            Err(error)
        }
    }
}

async fn wait_for_shared_connect(
    waiter: super::websocket_pool::WebSocketPoolConnectWaiter,
    fast_path_budget: Option<Duration>,
    _decision_started_at: Instant,
) -> Result<WebSocketPoolConnectOutcome, CodexWebSocketExchangeError> {
    match fast_path_budget {
        Some(budget) => {
            let remaining = waiter.remaining_budget(budget);
            timeout(remaining, waiter.wait())
                .await
                .map_err(|_| CodexWebSocketExchangeError::FastPathTimeout { timeout: budget })
        }
        None => Ok(waiter.wait().await),
    }
}

fn bypass_unavailable_reason(
    reason: WebSocketPoolBypassReason,
) -> PreviousResponseUnavailableReason {
    match reason {
        WebSocketPoolBypassReason::Busy => PreviousResponseUnavailableReason::ConnectionBusy,
        WebSocketPoolBypassReason::Disabled | WebSocketPoolBypassReason::Cap => {
            PreviousResponseUnavailableReason::PoolUnavailable
        }
    }
}

fn continuation_unavailable(
    reason: PreviousResponseUnavailableReason,
) -> CodexWebSocketExchangeError {
    CodexWebSocketExchangeError::ContinuationUnavailable { reason }
}

/// 发送 payload 后只等待该次 exchange；此边界之后不允许 transport fallback。
pub(crate) async fn execute_prepared_response_create_request(
    request: &CodexWebSocketRequest,
    prepared: PreparedWebSocket,
    started_at: Instant,
) -> Result<CodexWebSocketExchange, CodexWebSocketExchangeError> {
    let PreparedWebSocket {
        connection,
        lease,
        reused,
        pool_decision,
        initial_event_timeout,
        ..
    } = prepared;
    let PooledWebSocketConnection {
        websocket,
        metadata,
        continuation,
        created_at,
    } = connection;
    if let Err(error) = send_websocket_request(&websocket, request.payload_text()).await {
        discard_after_send(websocket, lease).await;
        return Err(post_send_ambiguous(error));
    }
    let connection_local_available = lease.is_some();
    let collected = collect_websocket_response(
        websocket,
        metadata,
        continuation,
        reused,
        started_at,
        initial_event_timeout,
    )
    .await;
    let (mut exchange, websocket, metadata, continuation, terminal) = match collected {
        Ok(collected) => collected,
        Err(error) => {
            if let Some(lease) = lease {
                lease.discard().await;
            }
            return Err(post_send_ambiguous(error));
        }
    };
    exchange.pool_decision = pool_decision;
    exchange.connection_local_continuation = connection_local_available;
    match (terminal, lease) {
        (WebSocketTerminalKind::Completed, Some(lease)) => {
            lease
                .put(PooledWebSocketConnection {
                    websocket,
                    metadata: reusable_websocket_metadata(metadata),
                    continuation,
                    created_at,
                })
                .await;
        }
        (_, Some(lease)) => {
            lease.discard().await;
            websocket.close().await;
        }
        (_, None) => websocket.close().await,
    }
    Ok(exchange)
}

pub(crate) async fn execute_prepared_response_create_request_stream(
    request: &CodexWebSocketRequest,
    prepared: PreparedWebSocket,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let PreparedWebSocket {
        connection,
        lease,
        reused,
        pool_decision,
        initial_event_timeout,
        ..
    } = prepared;
    let PooledWebSocketConnection {
        websocket,
        metadata,
        continuation,
        created_at,
    } = connection;
    if let Err(error) = send_websocket_request(&websocket, request.payload_text()).await {
        discard_after_send(websocket, lease).await;
        return Err(post_send_ambiguous(error));
    }
    let connection_local_available = lease.is_some();
    let pool_return = lease.map(|lease| WebSocketStreamPoolReturn {
        lease,
        created_at,
        continuation,
    });
    let mut exchange = stream_websocket_response(
        websocket,
        metadata,
        pool_return,
        reused,
        initial_event_timeout,
    );
    exchange.pool_decision = pool_decision;
    exchange.connection_local_continuation = connection_local_available;
    Ok(exchange)
}

async fn discard_after_send(websocket: PumpedWebSocket, lease: Option<WebSocketPoolLease>) {
    if let Some(lease) = lease {
        lease.discard().await;
    }
    websocket.close().await;
}

pub(crate) fn post_send_ambiguous(
    error: CodexWebSocketExchangeError,
) -> CodexWebSocketExchangeError {
    match error {
        error @ CodexWebSocketExchangeError::Upstream(_)
        | error @ CodexWebSocketExchangeError::PostSendAmbiguous { .. } => error,
        error => CodexWebSocketExchangeError::PostSendAmbiguous {
            message: error.to_string(),
        },
    }
}
