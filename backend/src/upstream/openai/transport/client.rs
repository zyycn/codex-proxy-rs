//! Codex HTTP/SSE 上游客户端、请求头构造、TLS 与自定义 CA。

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, TryStreamExt};
use reqwest::{
    header::{
        HeaderMap, HeaderName, HeaderValue, ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_TYPE,
        COOKIE, RETRY_AFTER, USER_AGENT,
    },
    Client, Response as ReqwestResponse, StatusCode,
};
use serde_json::{map::Map, Value};
use thiserror::Error;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use uuid::Uuid;

use crate::upstream::openai::fingerprint::{Fingerprint, RuntimeFingerprint};
use crate::upstream::openai::protocol::events::{extract_sse_usage, retry_after_seconds_from_body};
use crate::upstream::openai::protocol::responses::{
    http_sse_fallback_allowed, transport_for_request, CodexCompactRequest, CodexResponsesRequest,
    CodexTransport,
};
use crate::upstream::openai::protocol::sse::SseError;
use crate::upstream::openai::protocol::websocket::{
    websocket_audit_artifact_from_attempt, websocket_payload_audit_snapshot,
};

use super::diagnostics::CodexUpstreamDiagnostics;
use super::endpoints::{endpoint_url, CODEX_RESPONSES_COMPACT_PATH, CODEX_RESPONSES_PATH};
use super::headers::{
    build_ordered_codex_base_headers, insert_optional_header, insert_ordered_headers,
    websocket_header_pairs,
};
use super::response_meta;
use super::tls::{build_reqwest_client_with_custom_ca, custom_ca_env_cache_key, CustomCaError};
use super::websocket::{
    execute_response_create_request_stream_with_pool, execute_response_create_request_with_pool,
    write_websocket_audit_artifact_from_env, CodexWebSocketConnection, CodexWebSocketExchangeError,
    CodexWebSocketRateLimitHeaderUpdates, CodexWebSocketTurnStateUpdate,
};
use super::websocket_pool::{
    CodexWebSocketPool, CodexWebSocketPoolKey, WebSocketPoolDecision, DEFAULT_FIRST_TOKEN_TIMEOUT,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_UPSTREAM_ERROR_BODY_BYTES: usize = 1024 * 1024;
const X_CODEX_WS_STREAM_REQUEST_START_MS_CLIENT_METADATA_KEY: &str =
    "x-codex-ws-stream-request-start-ms";
type ReqwestClientCacheKey = (bool, Option<String>);
type ReqwestClientCache = Mutex<HashMap<ReqwestClientCacheKey, Client>>;

/// 构建带缓存的 reqwest Client。若 `force_http11` 为 true 则强制 HTTP/1.1。
pub fn build_reqwest_client(force_http11: bool) -> Result<Client, CustomCaError> {
    let cache_key = (force_http11, custom_ca_env_cache_key());
    static CLIENTS: OnceLock<ReqwestClientCache> = OnceLock::new();
    let cache = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut clients = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if let Some(client) = clients.get(&cache_key) {
        return Ok(client.clone());
    }

    let builder = Client::builder()
        .use_rustls_tls()
        .no_proxy()
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Duration::from_secs(30))
        .gzip(true)
        .brotli(true)
        .zstd(true)
        .deflate(true);
    let builder = if force_http11 {
        builder.http1_only()
    } else {
        builder
    };
    let client = build_reqwest_client_with_custom_ca(builder)?;
    clients.insert(cache_key, client.clone());
    Ok(client)
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
    },
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

/// 判断 Codex 上游错误是否表示账号已封禁或停用。
pub fn is_banned_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status.as_u16() == 403 && !is_html_error_body(body)
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
    /// 请求 ID。
    pub request_id: &'a str,
    /// x-codex-turn-state。
    pub turn_state: Option<&'a str>,
    /// x-codex-turn-metadata。
    pub turn_metadata: Option<&'a str>,
    /// x-codex-beta-features。
    pub beta_features: Option<&'a str>,
    /// x-responsesapi-include-timing-metrics。
    pub include_timing_metrics: Option<&'a str>,
    /// version。
    pub version: Option<&'a str>,
    /// x-codex-window-id。
    pub codex_window_id: Option<&'a str>,
    /// x-codex-parent-thread-id。
    pub parent_thread_id: Option<&'a str>,
    /// cookie 头。
    pub cookie_header: Option<&'a str>,
    /// x-codex-installation-id。
    pub installation_id: Option<&'a str>,
    /// session_id。
    pub session_id: Option<&'a str>,
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
}

/// Codex Responses 实际使用的上游传输。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexBackendTransport {
    /// HTTP SSE transport.
    HttpSse,
    /// WebSocket transport.
    WebSocket,
}

/// Map a Codex Responses request to the concrete backend transport.
pub fn backend_transport_for_response_request(
    request: &CodexResponsesRequest,
) -> CodexBackendTransport {
    match transport_for_request(request) {
        CodexTransport::HttpSse => CodexBackendTransport::HttpSse,
        CodexTransport::WebSocketPreferred | CodexTransport::WebSocketRequired => {
            CodexBackendTransport::WebSocket
        }
    }
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
}

/// Codex compact 端点响应。
#[derive(Debug, Clone, PartialEq)]
pub struct CodexCompactResponse {
    /// 上游返回的 JSON。
    pub body: Value,
    /// 上游透传的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游透传的限流头。
    pub rate_limit_headers: Vec<(String, String)>,
    /// 上游诊断元数据。
    pub diagnostics: CodexUpstreamDiagnostics,
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
    websocket_first_token_timeout: Option<Duration>,
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
    if let Some(reason) = decision.reason() {
        tracing::info!(
            request_id = %request_id,
            rid = %rid_short,
            account_id = account_id.unwrap_or_default(),
            ws_pool = decision.kind(),
            ws_pool_reason = reason,
            conversation_id_hash = pool_context.map_or("", |context| context.conversation_id_hash.as_str()),
            ws_pool_key_hash = pool_context.map_or("", |context| context.pool_key_hash.as_str()),
            "websocket pool decision"
        );
    } else {
        tracing::info!(
            request_id = %request_id,
            rid = %rid_short,
            account_id = account_id.unwrap_or_default(),
            ws_pool = decision.kind(),
            conversation_id_hash = pool_context.map_or("", |context| context.conversation_id_hash.as_str()),
            ws_pool_key_hash = pool_context.map_or("", |context| context.pool_key_hash.as_str()),
            "websocket pool decision"
        );
    }
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

fn response_upstream_request(
    request: &CodexResponsesRequest,
    context: CodexRequestContext<'_>,
) -> CodexResponsesRequest {
    let mut upstream = request.clone();
    if let Some(session_id) = context.session_id {
        upstream.set_prompt_cache_key(Some(session_id.to_string()));
    }
    upstream.set_client_metadata(response_client_metadata(request.client_metadata(), context));
    upstream
}

fn websocket_upstream_request(request: &CodexResponsesRequest) -> CodexResponsesRequest {
    let mut request = request.clone();
    stamp_ws_stream_request_start_ms(&mut request);
    request
}

fn stamp_ws_stream_request_start_ms(request: &mut CodexResponsesRequest) {
    let mut metadata = match request.client_metadata() {
        Some(Value::Object(metadata)) => metadata.clone(),
        _ => Map::new(),
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

fn response_client_metadata(
    client_metadata: Option<&Value>,
    context: CodexRequestContext<'_>,
) -> Option<Value> {
    // 以客户端原始 client_metadata 为基础（保留 number/bool/object 等所有值类型），
    // 在其上追加代理自身的上下文字段。
    let mut metadata = match client_metadata {
        Some(Value::Object(input)) => input.clone(),
        _ => Map::new(),
    };

    insert_metadata_string(
        &mut metadata,
        "x-codex-installation-id",
        context.installation_id,
    );
    insert_metadata_string(&mut metadata, "session_id", context.session_id);
    insert_metadata_string(&mut metadata, "thread_id", context.session_id);
    insert_metadata_string(&mut metadata, "x-codex-window-id", context.codex_window_id);
    insert_metadata_string(
        &mut metadata,
        "x-codex-turn-metadata",
        context.turn_metadata,
    );
    insert_metadata_string(
        &mut metadata,
        "x-codex-parent-thread-id",
        context.parent_thread_id,
    );

    if metadata.is_empty() {
        None
    } else {
        Some(Value::Object(metadata))
    }
}

fn insert_metadata_string(metadata: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        metadata.insert(key.to_string(), Value::String(value.to_string()));
    }
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
            }
        }
        error => CodexClientError::WebSocket(error),
    }
}

fn websocket_error_allows_http_fallback(error: &CodexClientError) -> bool {
    !matches!(error, CodexClientError::Upstream { .. })
}
