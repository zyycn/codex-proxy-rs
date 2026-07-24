//! Codex HTTP/SSE 上游客户端、请求头构造、TLS 与自定义 CA。

use std::{
    collections::HashMap,
    fmt,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::transport::profile::CodexWireProfileState;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use gateway_protocol::openai::{
    WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY, events::retry_after_seconds_from_body,
    sse::SseError,
};
use reqwest::{
    Client, Response as ReqwestResponse, StatusCode,
    header::{HeaderMap, RETRY_AFTER},
};
use serde_json::{Value, map::Map};
use thiserror::Error;

use crate::transport::protocol::responses::{CodexResponsesRequest, TransportRequirement};

use super::diagnostics::{CodexUpstreamDiagnostics, CodexUpstreamFailure, CodexUpstreamSendPhase};
use super::response_meta::CodexResponseMetadata;
use super::tls::{CustomCaError, build_reqwest_client_with_custom_ca, custom_ca_env_cache_key};
use super::websocket::{
    CodexWebSocketExchangeError, CodexWebSocketPool, CodexWebSocketPoolKey,
    CodexWebSocketRateLimitHeaderUpdates, CodexWebSocketRequest, CodexWebSocketTurnStateUpdate,
    PreparedWebSocket, WebSocketOriginBreaker, WebSocketPoolDecision,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_UPSTREAM_ERROR_BODY_BYTES: usize = 1024 * 1024;
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub(super) const UPSTREAM_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const X_CODEX_WS_STREAM_REQUEST_START_MS_CLIENT_METADATA_KEY: &str =
    "x-codex-ws-stream-request-start-ms";
type ReqwestClientCacheKey = Option<String>;
type ReqwestClientCache = Mutex<HashMap<ReqwestClientCacheKey, Client>>;

/// 构建带缓存、自动协商 HTTP/2 的 reqwest Client。
pub fn build_reqwest_client() -> Result<Client, CustomCaError> {
    let cache_key = custom_ca_env_cache_key();
    static CLIENTS: OnceLock<ReqwestClientCache> = OnceLock::new();
    let cache = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(client) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&cache_key)
    {
        return Ok(client.clone());
    }

    let builder = Client::builder()
        .use_rustls_tls()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(4)
        .pool_idle_timeout(None::<Duration>)
        .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
        .tcp_keepalive(Duration::from_secs(30))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .http2_keep_alive_timeout(Duration::from_secs(5))
        .http2_keep_alive_while_idle(true)
        .gzip(true)
        .brotli(true)
        .zstd(true)
        .deflate(true);
    let client = build_reqwest_client_with_custom_ca(builder)?;
    let mut clients = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(clients.entry(cache_key).or_insert(client).clone())
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Codex 上游 HTTP 客户端错误。
#[derive(Error)]
pub enum CodexClientError {
    /// Reqwest 传输失败。
    #[error("http transport error: {0}")]
    Http(#[from] reqwest::Error),
    /// 自定义 CA 构建失败。
    #[error("custom CA transport error: {0}")]
    CustomCa(#[from] CustomCaError),
    /// 请求头名字无效。
    #[error("invalid request header name: {0}")]
    InvalidHeaderName(#[from] reqwest::header::InvalidHeaderName),
    /// 请求头值无效。
    #[error("invalid request header value: {0}")]
    InvalidHeaderValue(#[from] reqwest::header::InvalidHeaderValue),
    /// SSE 响应解析失败。
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
    /// 模型目录不是一个完整、安全的官方快照。
    #[error("invalid Codex model catalog: {0}")]
    ModelCatalog(#[from] super::catalog::CodexModelCatalogError),
    /// HTTP/SSE 上游在空闲窗口内没有发送任何数据。
    #[error("upstream HTTP/SSE stream idle for {timeout:?}")]
    StreamIdleTimeout {
        /// 相邻数据块允许的最大空闲时间。
        timeout: Duration,
    },
    /// WebSocket 请求编码失败。
    #[error("failed to encode websocket request: {0}")]
    WebSocketEncode(#[source] serde_json::Error),
    /// WebSocket 请求失败。
    #[error("websocket request failed: {0}")]
    WebSocket(#[from] CodexWebSocketExchangeError),
    /// 上游返回非成功响应。
    #[error("upstream returned status {status}")]
    Upstream {
        /// 上游状态码。
        status: StatusCode,
        /// 上游错误体。
        body: String,
        /// 推导出的重试秒数。
        retry_after_seconds: Option<u64>,
        /// 上游诊断元数据。
        diagnostics: Box<CodexUpstreamDiagnostics>,
        /// 上游透传的 `set-cookie` 列表。
        set_cookie_headers: Vec<String>,
        /// 上游错误响应携带的限流头。
        rate_limit_headers: Vec<(String, String)>,
        /// 实际收到该上游响应的 transport。
        transport: CodexBackendTransport,
        /// 错误响应前已经确认的 transport 与 HTTP 阶段事实。
        transport_metrics: Box<CodexTransportMetrics>,
        /// 上游拒绝发生时业务 payload 的发送阶段。
        send_phase: CodexUpstreamSendPhase,
    },
}

impl fmt::Debug for CodexClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(_) => formatter.write_str("CodexClientError::Http([REDACTED])"),
            Self::CustomCa(_) => formatter.write_str("CodexClientError::CustomCa([REDACTED])"),
            Self::InvalidHeaderName(_) => {
                formatter.write_str("CodexClientError::InvalidHeaderName([REDACTED])")
            }
            Self::InvalidHeaderValue(_) => {
                formatter.write_str("CodexClientError::InvalidHeaderValue([REDACTED])")
            }
            Self::InvalidSse(_) => formatter.write_str("CodexClientError::InvalidSse([REDACTED])"),
            Self::ModelCatalog(error) => formatter
                .debug_tuple("CodexClientError::ModelCatalog")
                .field(error)
                .finish(),
            Self::StreamIdleTimeout { timeout } => formatter
                .debug_struct("CodexClientError::StreamIdleTimeout")
                .field("timeout", timeout)
                .finish(),
            Self::WebSocketEncode(_) => {
                formatter.write_str("CodexClientError::WebSocketEncode([REDACTED])")
            }
            Self::WebSocket(_) => formatter.write_str("CodexClientError::WebSocket([REDACTED])"),
            Self::Upstream {
                status,
                retry_after_seconds,
                transport,
                send_phase,
                ..
            } => formatter
                .debug_struct("CodexClientError::Upstream")
                .field("status", status)
                .field("retry_after_seconds", retry_after_seconds)
                .field("transport", transport)
                .field("send_phase", send_phase)
                .field("body", &"[REDACTED]")
                .finish(),
        }
    }
}

impl CodexClientError {
    /// 返回错误实际发生的 transport；请求编码等本地错误没有 transport。
    pub fn transport(&self) -> Option<CodexBackendTransport> {
        match self {
            Self::Http(_)
            | Self::StreamIdleTimeout { .. }
            | Self::InvalidSse(_)
            | Self::ModelCatalog(_) => Some(CodexBackendTransport::HttpSse),
            Self::WebSocket(_) => Some(CodexBackendTransport::WebSocket),
            Self::Upstream { transport, .. } => Some(*transport),
            Self::CustomCa(_)
            | Self::InvalidHeaderName(_)
            | Self::InvalidHeaderValue(_)
            | Self::WebSocketEncode(_) => None,
        }
    }

    pub(crate) fn upstream_failure(&self) -> Option<CodexUpstreamFailure> {
        match self {
            Self::Upstream {
                status,
                body,
                retry_after_seconds,
                diagnostics,
                set_cookie_headers,
                rate_limit_headers,
                send_phase,
                ..
            } => Some(CodexUpstreamFailure::from_response(
                *status,
                body,
                *retry_after_seconds,
                diagnostics,
                set_cookie_headers,
                rate_limit_headers,
                *send_phase,
            )),
            Self::WebSocket(CodexWebSocketExchangeError::Upstream(upstream)) => {
                Some(CodexUpstreamFailure::from_response(
                    StatusCode::from_u16(upstream.status_code).unwrap_or(StatusCode::BAD_GATEWAY),
                    &upstream.body,
                    upstream.retry_after_seconds,
                    &upstream.diagnostics,
                    &upstream.set_cookie_headers,
                    &[],
                    upstream.send_phase,
                ))
            }
            _ => None,
        }
    }
}

/// Codex 客户端结果类型。
pub type CodexClientResult<T> = Result<T, CodexClientError>;

/// Codex SSE 字节流。
pub type CodexBackendSseStream =
    Pin<Box<dyn Stream<Item = CodexClientResult<Bytes>> + Send + 'static>>;

// ---------------------------------------------------------------------------
// Request context & response types
// ---------------------------------------------------------------------------

/// 单次 Codex 上游请求的上下文。
#[derive(Clone, Copy)]
pub struct CodexRequestContext<'a> {
    /// Provider 已构造并脱敏持有的完整 Authorization 值。
    pub authorization: &'a str,
    /// ChatGPT account id。
    pub account_id: Option<&'a str>,
    /// 代理请求 ID。
    pub request_id: &'a str,
    /// 当前账号同一 turn 内的 opaque sticky-routing 状态。
    pub turn_state: Option<&'a str>,
    /// 客户端 turn metadata；其中 installation ID 已按当前账号处理。
    pub turn_metadata: Option<&'a str>,
    /// x-codex-beta-features。
    pub beta_features: Option<&'a str>,
    /// x-responsesapi-include-timing-metrics。
    pub include_timing_metrics: Option<&'a str>,
    /// version。
    pub version: Option<&'a str>,
    /// 客户端 window ID。
    pub codex_window_id: Option<&'a str>,
    /// 客户端 parent thread ID。
    pub parent_thread_id: Option<&'a str>,
    /// cookie 头。
    pub cookie_header: Option<&'a str>,
    /// 当前账号稳定派生的 installation ID。
    pub installation_id: Option<&'a str>,
    /// 客户端 session ID。
    pub session_id: Option<&'a str>,
    /// 客户端 thread ID。
    pub thread_id: Option<&'a str>,
    /// 客户端逻辑 request ID；缺失时使用代理请求 ID。
    pub client_request_id: Option<&'a str>,
    /// 客户端 turn ID。
    pub turn_id: Option<&'a str>,
}

impl<'a> CodexRequestContext<'a> {
    #[must_use]
    pub const fn auxiliary(
        authorization: &'a str,
        account_id: Option<&'a str>,
        request_id: &'a str,
        installation_id: Option<&'a str>,
    ) -> Self {
        Self {
            authorization,
            account_id,
            request_id,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: None,
            installation_id,
            session_id: None,
            thread_id: None,
            client_request_id: None,
            turn_id: None,
        }
    }
}

impl fmt::Debug for CodexRequestContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexRequestContext")
            .field("authorization", &"[REDACTED]")
            .field("account_id", &self.account_id.map(|_| "[REDACTED]"))
            .field("request_id", &self.request_id)
            .field("turn_state", &self.turn_state.map(|_| "[REDACTED]"))
            .field("turn_metadata", &self.turn_metadata.map(|_| "[REDACTED]"))
            .field("cookie_header", &self.cookie_header.map(|_| "[REDACTED]"))
            .field(
                "installation_id",
                &self.installation_id.map(|_| "[PSEUDONYM]"),
            )
            .finish_non_exhaustive()
    }
}

/// Codex Responses 实际使用的上游传输。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexBackendTransport {
    /// HTTP SSE transport.
    HttpSse,
    /// WebSocket transport.
    WebSocket,
}

/// transport owner 最终做出的稳定决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexTransportDecision {
    HttpRequired,
    ReusedWebSocket,
    ConnectedWebSocket,
    ExactWebSocket,
    RequiredWebSocket,
    Http2WebSocketBudgetExhausted,
    Http2BreakerOpen,
    Http2PoolUnavailable,
    Http2PreSendFailure,
}

impl CodexTransportDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HttpRequired => "http_required",
            Self::ReusedWebSocket => "ws_reused",
            Self::ConnectedWebSocket => "ws_connected_fast",
            Self::ExactWebSocket => "ws_exact_required",
            Self::RequiredWebSocket => "ws_required",
            Self::Http2WebSocketBudgetExhausted => "http2_ws_budget_exhausted",
            Self::Http2BreakerOpen => "http2_breaker_open",
            Self::Http2PoolUnavailable => "http2_pool_unavailable",
            Self::Http2PreSendFailure => "http2_ws_pre_send_failure",
        }
    }
}

/// transport 决策与握手阶段观测值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexTransportMetrics {
    pub decision: Option<CodexTransportDecision>,
    pub ws_connect_ms: Option<i64>,
    pub transport_decision_wait_ms: Option<i64>,
    pub upstream_headers_ms: Option<i64>,
    pub first_event_ms: Option<i64>,
    pub http_version: Option<String>,
}

/// Live stream rate-limit updates captured after the response headers.
pub type CodexRateLimitHeaderUpdates = CodexWebSocketRateLimitHeaderUpdates;

/// Live stream turn-state updates captured after the response headers.
pub type CodexTurnStateUpdate = CodexWebSocketTurnStateUpdate;

/// Codex Responses 上游 live SSE 响应。
pub struct CodexBackendStreamingResponse {
    /// 上游 SSE 字节流。
    pub body: CodexBackendSseStream,
    /// 实际使用的上游传输。
    pub transport: CodexBackendTransport,
    /// 响应头里的最新 turn state。
    pub turn_state: Option<String>,
    /// 上游透传的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游透传的限流头。
    pub rate_limit_headers: Vec<(String, String)>,
    /// live stream 期间捕获的限流头更新。
    pub rate_limit_header_updates: Option<CodexRateLimitHeaderUpdates>,
    /// live stream 期间捕获的 turn-state 更新。
    pub turn_state_update: Option<CodexTurnStateUpdate>,
    /// WebSocket 连接池决策。
    pub websocket_pool_decision: Option<WebSocketPoolDecision>,
    /// 上游诊断元数据。
    pub diagnostics: CodexUpstreamDiagnostics,
    /// 安全响应元数据。
    pub response_metadata: CodexResponseMetadata,
    /// 传输选择与低延迟阶段耗时。
    pub transport_metrics: CodexTransportMetrics,
    /// terminal completed 后是否由池中 WebSocket 保留 connection-local continuation。
    pub connection_local_continuation: bool,
}

// ---------------------------------------------------------------------------
// CodexBackendClient
// ---------------------------------------------------------------------------

/// Codex HTTP/SSE 上游客户端。
#[derive(Clone)]
pub struct CodexBackendClient {
    pub(super) client: Client,
    pub(super) base_url: String,
    pub(super) profile: CodexWireProfileState,
    pub(super) websocket_pool: Option<Arc<CodexWebSocketPool>>,
    pub(super) websocket_origin_breaker: WebSocketOriginBreaker,
    pub(super) websocket_origin_key: String,
}

/// 已完成账号级 opening 准备、但尚未发送 payload 的 transport。
pub(crate) struct PreparedResponseTransport {
    pub(super) requirement: TransportRequirement,
    pub(super) route: PreparedResponseRoute,
    pub(super) metrics: CodexTransportMetrics,
}

pub(super) enum PreparedResponseRoute {
    Http,
    WebSocket(Box<PreparedWebSocketRoute>),
}

pub(super) struct PreparedWebSocketRoute {
    pub(super) request: CodexWebSocketRequest,
    pub(super) prepared: PreparedWebSocket,
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

pub(super) fn log_websocket_pool_decision(
    request_id: &str,
    account_id: Option<&str>,
    pool_context: Option<&WebSocketPoolLogContext>,
    decision: Option<WebSocketPoolDecision>,
) {
    let Some(decision) = decision else {
        return;
    };
    let rid_short = request_id.chars().take(8).collect::<String>();
    tracing::info!(
        request_id = %request_id,
        rid = %rid_short,
        account_id = account_id.unwrap_or_default(),
        ws_pool = decision.kind(),
        conversation_id_hash = pool_context.map_or("", |context| context.conversation_id_hash.as_str()),
        ws_pool_key_hash = pool_context.map_or("", |context| context.pool_key_hash.as_str()),
        "WebSocket pool decision"
    );
}

#[derive(Debug, Clone)]
pub(super) struct WebSocketPoolLogContext {
    conversation_id_hash: String,
    pool_key_hash: String,
}

impl WebSocketPoolLogContext {
    pub(super) fn from_key(key: &CodexWebSocketPoolKey) -> Self {
        Self {
            conversation_id_hash: key.conversation_id_hash(),
            pool_key_hash: key.stable_hash(),
        }
    }
}

pub(super) fn retry_after_seconds(headers: &HeaderMap, body: Option<&str>) -> Option<u64> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .or_else(|| body.and_then(retry_after_seconds_from_body))
}

pub(super) fn truncate_for_error(body: &str) -> String {
    body.chars().take(200).collect()
}

pub(super) struct CappedResponseBody {
    bytes: Vec<u8>,
    limit_exceeded: bool,
}

impl CappedResponseBody {
    pub(super) const fn limit_exceeded(&self) -> bool {
        self.limit_exceeded
    }

    pub(super) fn into_string(self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

pub(super) async fn read_capped_response_body(
    response: ReqwestResponse,
    max_bytes: usize,
) -> Result<CappedResponseBody, reqwest::Error> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Ok(CappedResponseBody {
            bytes: Vec::new(),
            limit_exceeded: true,
        });
    }

    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .map_or(0, |length| length.min(max_bytes)),
    );
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await.transpose()? {
        let remaining = max_bytes.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            return Ok(CappedResponseBody {
                bytes,
                limit_exceeded: true,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(CappedResponseBody {
        bytes,
        limit_exceeded: false,
    })
}

pub(super) async fn read_capped_error_body(
    response: ReqwestResponse,
) -> Result<String, reqwest::Error> {
    let body = read_capped_response_body(response, MAX_UPSTREAM_ERROR_BODY_BYTES).await?;
    if body.limit_exceeded() {
        return Ok("upstream error response exceeded the body limit".to_owned());
    }
    Ok(body.into_string())
}

// ---------------------------------------------------------------------------
// Request helpers
// ---------------------------------------------------------------------------

pub(super) fn websocket_upstream_request(request: &CodexResponsesRequest) -> CodexResponsesRequest {
    let mut request = request.clone();
    project_responses_lite_to_ws_metadata(&mut request);
    stamp_ws_stream_request_start_ms(&mut request);
    request
}

fn project_responses_lite_to_ws_metadata(request: &mut CodexResponsesRequest) {
    let Some(responses_lite) = request.responses_lite.clone() else {
        return;
    };
    let mut metadata = match request.client_metadata() {
        Some(Value::Object(metadata)) => metadata.clone(),
        None => Map::new(),
        Some(_) => return,
    };
    metadata
        .entry(WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY.to_string())
        .or_insert(Value::String(responses_lite));
    request.set_client_metadata(Some(Value::Object(metadata)));
}

fn stamp_ws_stream_request_start_ms(request: &mut CodexResponsesRequest) {
    let mut metadata = match request.client_metadata() {
        Some(Value::Object(metadata)) => metadata.clone(),
        None => Map::new(),
        Some(_) => return,
    };
    metadata.insert(
        X_CODEX_WS_STREAM_REQUEST_START_MS_CLIENT_METADATA_KEY.to_string(),
        Value::String(now_unix_timestamp_millis().to_string()),
    );
    request.set_client_metadata(Some(Value::Object(metadata)));
}

fn now_unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

pub(super) fn openai_subagent_from_metadata(client_metadata: Option<&Value>) -> Option<String> {
    let value = client_metadata?
        .as_object()?
        .get("x-openai-subagent")?
        .as_str()?
        .trim();
    if matches!(
        value,
        "review" | "compact" | "memory_consolidation" | "collab_spawn"
    ) {
        Some(value.to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Error conversion helpers
// ---------------------------------------------------------------------------

pub(super) fn websocket_exchange_error_to_client_error(
    error: CodexWebSocketExchangeError,
) -> CodexClientError {
    match error {
        CodexWebSocketExchangeError::Upstream(upstream) => {
            let upstream = *upstream;
            CodexClientError::Upstream {
                status: StatusCode::from_u16(upstream.status_code)
                    .unwrap_or(StatusCode::BAD_GATEWAY),
                body: upstream.body,
                retry_after_seconds: upstream.retry_after_seconds,
                diagnostics: Box::new(upstream.diagnostics),
                set_cookie_headers: upstream.set_cookie_headers,
                rate_limit_headers: Vec::new(),
                transport: CodexBackendTransport::WebSocket,
                transport_metrics: Box::default(),
                send_phase: upstream.send_phase,
            }
        }
        error => CodexClientError::WebSocket(error),
    }
}

pub(super) fn websocket_success_decision(
    requirement: TransportRequirement,
    prepared: &PreparedWebSocket,
) -> CodexTransportDecision {
    match requirement {
        TransportRequirement::ExactWebSocketContinuation => CodexTransportDecision::ExactWebSocket,
        TransportRequirement::ExplicitWebSocketWarmup => CodexTransportDecision::RequiredWebSocket,
        _ if prepared.reused() => CodexTransportDecision::ReusedWebSocket,
        _ => CodexTransportDecision::ConnectedWebSocket,
    }
}

pub(super) fn http_fallback_decision(
    error: &CodexWebSocketExchangeError,
) -> CodexTransportDecision {
    match error {
        CodexWebSocketExchangeError::FastPathTimeout { .. } => {
            CodexTransportDecision::Http2WebSocketBudgetExhausted
        }
        CodexWebSocketExchangeError::OriginCircuitOpen
        | CodexWebSocketExchangeError::OriginHalfOpenBusy => {
            CodexTransportDecision::Http2BreakerOpen
        }
        CodexWebSocketExchangeError::ContinuationUnavailable { .. }
        | CodexWebSocketExchangeError::SharedConnectFailed => {
            CodexTransportDecision::Http2PoolUnavailable
        }
        _ => CodexTransportDecision::Http2PreSendFailure,
    }
}

pub(super) fn merge_preparation_metrics(
    response: &mut CodexTransportMetrics,
    preparation: CodexTransportMetrics,
) {
    response.decision = preparation.decision;
    response.ws_connect_ms = preparation.ws_connect_ms;
    response.transport_decision_wait_ms = preparation.transport_decision_wait_ms;
}

pub(super) fn elapsed_duration_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis())
        .unwrap_or(i64::MAX)
        .max(1)
}

pub(super) fn http_version_name(version: reqwest::Version) -> &'static str {
    match version {
        reqwest::Version::HTTP_09 => "HTTP/0.9",
        reqwest::Version::HTTP_10 => "HTTP/1.0",
        reqwest::Version::HTTP_11 => "HTTP/1.1",
        reqwest::Version::HTTP_2 => "HTTP/2",
        reqwest::Version::HTTP_3 => "HTTP/3",
        _ => "unknown",
    }
}

pub(super) fn websocket_origin_key(base_url: &str) -> String {
    let origin = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?;
            Some(format!(
                "{}://{}:{}",
                url.scheme(),
                host,
                url.port_or_known_default().unwrap_or_default()
            ))
        })
        .unwrap_or_else(|| base_url.trim_end_matches('/').to_string());
    match custom_ca_env_cache_key() {
        Some(tls_profile) => format!("{origin}\0{tls_profile}"),
        None => origin,
    }
}
