//! Codex HTTP/SSE 上游客户端、请求头构造、TLS 与自定义 CA。

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt, TryStreamExt};
use reqwest::{
    Client, Response as ReqwestResponse, StatusCode,
    header::{
        ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderMap, HeaderName,
        HeaderValue, RETRY_AFTER, USER_AGENT,
    },
};
use serde_json::{Value, map::Map};
use thiserror::Error;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;

use crate::upstream::openai::fingerprint::{Fingerprint, RuntimeFingerprint};
use crate::upstream::openai::protocol::events::{extract_sse_usage, retry_after_seconds_from_body};
use crate::upstream::openai::protocol::responses::{
    CodexResponsesRequest, TransportRequirement, transport_requirement,
};
use crate::upstream::openai::protocol::sse::SseError;
use crate::upstream::openai::protocol::websocket::{
    websocket_audit_artifact_from_attempt, websocket_payload_audit_snapshot,
};

use super::diagnostics::CodexUpstreamDiagnostics;
use super::endpoints::{CODEX_RESPONSES_PATH, endpoint_url};
use super::headers::{
    build_ordered_codex_base_headers, insert_optional_header, insert_ordered_headers,
    websocket_header_pairs,
};
use super::response_meta;
use super::tls::{CustomCaError, build_reqwest_client_with_custom_ca, custom_ca_env_cache_key};
use super::websocket::{
    CodexWebSocketConnection, CodexWebSocketExchangeError, CodexWebSocketRateLimitHeaderUpdates,
    CodexWebSocketRequest, CodexWebSocketTurnStateUpdate, PreparedWebSocket,
    WEBSOCKET_FAST_PATH_BUDGET, execute_prepared_response_create_request,
    execute_prepared_response_create_request_stream, post_send_ambiguous,
    prepare_response_create_request_with_pool, write_websocket_audit_artifact_from_env,
};
use super::websocket_breaker::WebSocketOriginBreaker;
use super::websocket_pool::{
    CodexWebSocketPool, CodexWebSocketPoolKey, DEFAULT_INITIAL_EVENT_TIMEOUT, WebSocketPoolDecision,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_UPSTREAM_ERROR_BODY_BYTES: usize = 1024 * 1024;
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const UPSTREAM_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
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
#[derive(Debug, Error)]
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
    #[error("upstream returned status {status}: {body}")]
    Upstream {
        /// 上游状态码。
        status: StatusCode,
        /// 上游错误体。
        body: String,
        /// 推导出的重试秒数。
        retry_after_seconds: Option<u64>,
        /// 上游诊断元数据。
        diagnostics: CodexUpstreamDiagnostics,
        /// 上游透传的 `set-cookie` 列表。
        set_cookie_headers: Vec<String>,
        /// 实际收到该上游响应的 transport。
        transport: CodexBackendTransport,
    },
}

impl CodexClientError {
    /// 返回错误实际发生的 transport；请求编码等本地错误没有 transport。
    pub fn transport(&self) -> Option<CodexBackendTransport> {
        match self {
            Self::Http(_) | Self::StreamIdleTimeout { .. } | Self::InvalidSse(_) => {
                Some(CodexBackendTransport::HttpSse)
            }
            Self::WebSocket(_) => Some(CodexBackendTransport::WebSocket),
            Self::Upstream { transport, .. } => Some(*transport),
            Self::CustomCa(_)
            | Self::InvalidHeaderName(_)
            | Self::InvalidHeaderValue(_)
            | Self::WebSocketEncode(_) => None,
        }
    }
}

/// 判断上游错误正文是否表示账号已封禁或停用。
pub fn is_banned_auth_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("account_deactivated")
        || value.contains("account deactivated")
        || value.contains("account has been deactivated")
        || value.contains("deactivated")
        || value.contains("banned")
}

/// 判断 402 错误正文是否带有 OpenAI 工作区停用标记。
pub fn is_deactivated_workspace_error_body(value: &str) -> bool {
    serde_json::from_str::<Value>(value).is_ok_and(|value| {
        value.pointer("/detail/code").and_then(Value::as_str) == Some("deactivated_workspace")
    })
}

/// 判断 Codex 上游错误是否表示账号已封禁或停用。
pub fn is_banned_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if (status.as_u16() == 403
                && !is_html_error_body(body))
                || (status.as_u16() == 402 && is_deactivated_workspace_error_body(body))
    )
}

fn is_html_error_body(value: &str) -> bool {
    let value = value.trim_start().to_ascii_lowercase();
    value.starts_with("<!doctype") || value.starts_with("<html") || value.contains("<html")
}

/// Codex 客户端结果类型。
pub type CodexClientResult<T> = Result<T, CodexClientError>;

/// Codex SSE 字节流。
pub type CodexBackendSseStream =
    Pin<Box<dyn Stream<Item = CodexClientResult<Bytes>> + Send + 'static>>;

/// 拉取上游模型目录时的请求上下文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexModelCatalogRequest<'a> {
    /// 当前账号访问令牌。
    pub access_token: &'a str,
    /// 上游账号 ID。
    pub account_id: Option<&'a str>,
    /// 请求 ID。
    pub request_id: &'a str,
    /// Codex installation id。
    pub installation_id: Option<&'a str>,
    /// 订阅计划类型。
    pub plan_type: &'a str,
}

/// 上游模型目录客户端错误。
#[derive(Debug, Error)]
pub enum CodexModelCatalogClientError {
    /// 上游请求失败。
    #[error("model catalog request failed: {message}")]
    RequestFailed {
        /// 错误说明。
        message: String,
    },
}

/// 上游模型目录客户端。
#[async_trait]
pub trait CodexModelCatalogClient: Send + Sync + 'static {
    /// 读取当前账号可见的上游模型目录。
    async fn fetch_models(
        &self,
        request: &CodexModelCatalogRequest<'_>,
    ) -> Result<Vec<Value>, CodexModelCatalogClientError>;
}

// ---------------------------------------------------------------------------
// Request context & response types
// ---------------------------------------------------------------------------

/// 单次 Codex 上游请求的上下文。
#[derive(Debug, Clone, Copy)]
pub struct CodexRequestContext<'a> {
    /// Access token。
    pub access_token: &'a str,
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

/// Codex Responses 上游响应。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexResponseMetadata {
    /// 上游实际选用的模型。
    pub effective_model: Option<String>,
    /// 模型目录版本。
    pub models_etag: Option<String>,
    /// 上游是否声明响应包含 reasoning。
    pub reasoning_included: bool,
    /// 允许透传给客户端的安全响应头。
    pub client_headers: Vec<(String, String)>,
}

/// Codex Responses 上游响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexBackendResponse {
    /// 完整 SSE 文本。
    pub body: String,
    /// 实际使用的上游传输。
    pub transport: CodexBackendTransport,
    /// 从 SSE 中提取出的最终 usage。
    pub usage: Option<crate::upstream::openai::protocol::events::TokenUsage>,
    /// 响应头里的最新 turn state。
    pub turn_state: Option<String>,
    /// 上游透传的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游透传的限流头。
    pub rate_limit_headers: Vec<(String, String)>,
    /// 首个有效上游 SSE/WebSocket 事件到达代理的耗时。
    pub first_token_ms: Option<i64>,
    /// WebSocket 连接池决策。
    pub websocket_pool_decision: Option<WebSocketPoolDecision>,
    /// 上游诊断元数据。
    pub diagnostics: CodexUpstreamDiagnostics,
    /// 安全响应元数据。
    pub response_metadata: CodexResponseMetadata,
    /// 传输选择与低延迟阶段耗时。
    pub transport_metrics: CodexTransportMetrics,
    /// 当前响应是否由池中 WebSocket 保留 connection-local continuation。
    pub connection_local_continuation: bool,
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
    Http2WebSocketSlow,
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
            Self::Http2WebSocketSlow => "http2_ws_slow",
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
    fingerprint: RuntimeFingerprint,
    websocket_pool: Option<Arc<CodexWebSocketPool>>,
    websocket_initial_event_timeout: Option<Duration>,
    websocket_fast_path_budget: Duration,
    websocket_origin_breaker: WebSocketOriginBreaker,
    websocket_origin_key: String,
}

/// 已完成账号级 opening 准备、但尚未发送 payload 的 transport。
pub(crate) struct PreparedResponseTransport {
    requirement: TransportRequirement,
    route: PreparedResponseRoute,
    metrics: CodexTransportMetrics,
}

enum PreparedResponseRoute {
    Http,
    WebSocket(Box<PreparedWebSocketRoute>),
}

struct PreparedWebSocketRoute {
    request: CodexWebSocketRequest,
    prepared: PreparedWebSocket,
}

#[path = "client_sse.rs"]
mod requests;

#[async_trait]
impl CodexModelCatalogClient for CodexBackendClient {
    async fn fetch_models(
        &self,
        request: &CodexModelCatalogRequest<'_>,
    ) -> Result<Vec<Value>, CodexModelCatalogClientError> {
        self.fetch_models_with_context(CodexRequestContext {
            access_token: request.access_token,
            account_id: request.account_id,
            request_id: request.request_id,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: None,
            installation_id: request.installation_id,
            session_id: None,
            thread_id: None,
            client_request_id: None,
            turn_id: None,
        })
        .await
        .map_err(|error| CodexModelCatalogClientError::RequestFailed {
            message: error.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

fn log_websocket_pool_decision(
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
struct WebSocketPoolLogContext {
    conversation_id_hash: String,
    pool_key_hash: String,
}

impl WebSocketPoolLogContext {
    fn from_key(key: &CodexWebSocketPoolKey) -> Self {
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

pub(super) async fn read_capped_error_body(
    response: ReqwestResponse,
) -> Result<String, reqwest::Error> {
    let body = response.bytes().await?;
    let len = body.len().min(MAX_UPSTREAM_ERROR_BODY_BYTES);
    Ok(String::from_utf8_lossy(&body[..len]).into_owned())
}

// ---------------------------------------------------------------------------
// Model entry extraction
// ---------------------------------------------------------------------------

fn extract_model_entries(value: &Value) -> Vec<Value> {
    let Some(models) = value
        .pointer("/chat_models/models")
        .or_else(|| value.get("models"))
        .or_else(|| value.get("data"))
        .or_else(|| value.get("categories"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for model in models {
        if let Some(nested) = model.get("models").and_then(Value::as_array) {
            entries.extend(nested.iter().filter(|entry| is_model_entry(entry)).cloned());
        } else if is_model_entry(model) {
            entries.push(model.clone());
        }
    }
    entries
}

fn is_model_entry(value: &Value) -> bool {
    ["slug", "id", "name", "display_name", "title"]
        .into_iter()
        .any(|key| {
            value
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
}

// ---------------------------------------------------------------------------
// Request helpers
// ---------------------------------------------------------------------------

fn websocket_upstream_request(request: &CodexResponsesRequest) -> CodexResponsesRequest {
    let mut request = request.clone();
    stamp_ws_stream_request_start_ms(&mut request);
    request
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

fn openai_subagent_from_metadata(client_metadata: Option<&Value>) -> Option<String> {
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

fn websocket_exchange_error_to_client_error(
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
                diagnostics: upstream.diagnostics,
                set_cookie_headers: upstream.set_cookie_headers,
                transport: CodexBackendTransport::WebSocket,
            }
        }
        error => CodexClientError::WebSocket(error),
    }
}

fn websocket_success_decision(
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

fn http_fallback_decision(error: &CodexWebSocketExchangeError) -> CodexTransportDecision {
    match error {
        CodexWebSocketExchangeError::FastPathTimeout { .. } => {
            CodexTransportDecision::Http2WebSocketSlow
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

fn merge_preparation_metrics(
    response: &mut CodexTransportMetrics,
    preparation: CodexTransportMetrics,
) {
    response.decision = preparation.decision;
    response.ws_connect_ms = preparation.ws_connect_ms;
    response.transport_decision_wait_ms = preparation.transport_decision_wait_ms;
}

fn elapsed_duration_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis())
        .unwrap_or(i64::MAX)
        .max(1)
}

fn http_version_name(version: reqwest::Version) -> &'static str {
    match version {
        reqwest::Version::HTTP_09 => "HTTP/0.9",
        reqwest::Version::HTTP_10 => "HTTP/1.0",
        reqwest::Version::HTTP_11 => "HTTP/1.1",
        reqwest::Version::HTTP_2 => "HTTP/2",
        reqwest::Version::HTTP_3 => "HTTP/3",
        _ => "unknown",
    }
}

fn websocket_origin_key(base_url: &str) -> String {
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
