//! Codex HTTP/SSE 上游客户端与请求头构造。

use std::{
    collections::HashMap,
    env, fs, io,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use bytes::Bytes;
use futures::{Stream, TryStreamExt};
use indexmap::IndexMap;
use reqwest::{
    header::{
        HeaderMap, HeaderName, HeaderValue, ACCEPT, ACCEPT_ENCODING, CONTENT_TYPE, COOKIE,
        RETRY_AFTER, SET_COOKIE,
    },
    Client, Response as ReqwestResponse, StatusCode,
};
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::{
    pem::{self, PemObject, SectionKind},
    CertificateDer,
};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

use codex_proxy_core::{
    gateway::{
        fingerprint::Fingerprint,
        ports::{CodexModelCatalogClient, CodexModelCatalogClientError, CodexModelCatalogRequest},
    },
    models::model::BackendModelEntry,
    protocol::codex::{
        events::{extract_sse_usage, retry_after_seconds_from_body, TokenUsage},
        responses::{CodexCompactRequest, CodexResponsesRequest},
        sse::SseError,
        websocket::{websocket_audit_artifact_from_attempt, websocket_payload_audit_snapshot},
    },
    serving::responses::{http_sse_fallback_allowed, transport_for_request, CodexTransport},
};

use async_trait::async_trait;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;

use super::websocket::connect::{
    execute_response_create_request, execute_response_create_request_stream_with_pool,
    execute_response_create_request_with_pool, CodexWebSocketConnection,
    CodexWebSocketExchangeError, CodexWebSocketRateLimitHeaderUpdates,
    CodexWebSocketTurnStateUpdate,
};
use super::websocket::opening::write_websocket_audit_artifact_from_env;
use super::websocket::pool::{CodexWebSocketPool, CodexWebSocketPoolKey};

const MAX_UPSTREAM_ERROR_BODY_BYTES: usize = 1024 * 1024;
const CA_CERT_HINT: &str = "If you set CODEX_CA_CERTIFICATE or SSL_CERT_FILE, ensure it points to a PEM file containing one or more CERTIFICATE blocks, or unset it to use system roots.";

type PemSection = (SectionKind, Vec<u8>);

type ReqwestClientCacheKey = (bool, Option<String>);
type ReqwestClientCache = Mutex<HashMap<ReqwestClientCacheKey, Client>>;

/// Codex HTTP 客户端类型。
pub type HttpClient = Client;

/// `/codex/responses`
pub const CODEX_RESPONSES_PATH: &str = "/codex/responses";
/// `/codex/responses/compact`
pub const CODEX_RESPONSES_COMPACT_PATH: &str = "/codex/responses/compact";
/// `/codex/usage`
pub const CODEX_USAGE_PATH: &str = "/codex/usage";
/// `/api/codex/usage`
pub const CODEX_USAGE_API_PATH: &str = "/api/codex/usage";
/// `/wham/usage`
pub const WHAM_USAGE_PATH: &str = "/wham/usage";
/// 自定义 CA 证书环境变量名。
pub const CODEX_CA_CERT_ENV: &str = "CODEX_CA_CERTIFICATE";
/// 系统 CA 文件环境变量名。
pub const SSL_CERT_FILE_ENV: &str = "SSL_CERT_FILE";

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
        /// 上游透传的 `set-cookie` 列表。
        set_cookie_headers: Vec<String>,
    },
}

/// Codex 客户端结果类型。
pub type CodexClientResult<T> = Result<T, CodexClientError>;

/// Codex SSE 字节流。
pub type CodexBackendSseStream =
    Pin<Box<dyn Stream<Item = CodexClientResult<Bytes>> + Send + 'static>>;

/// 自定义 CA 错误。
#[derive(Debug, Error)]
pub enum CustomCaError {
    /// 读取 CA 证书文件失败。
    #[error(
        "Failed to read CA certificate file {} selected by {}: {source}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    ReadCaFile {
        /// 来源环境变量名。
        source_env: &'static str,
        /// 证书路径。
        path: PathBuf,
        /// 底层 IO 错误。
        source: io::Error,
    },
    /// CA 证书文件格式无效。
    #[error(
        "Failed to load CA certificates from {} selected by {}: {detail}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    InvalidCaFile {
        /// 来源环境变量名。
        source_env: &'static str,
        /// 证书路径。
        path: PathBuf,
        /// 详细错误。
        detail: String,
    },
    /// 证书无法注册为 reqwest 根证书。
    #[error(
        "Failed to parse certificate #{certificate_index} from {} selected by {}: {source}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    RegisterCertificate {
        /// 来源环境变量名。
        source_env: &'static str,
        /// 证书路径。
        path: PathBuf,
        /// 证书序号。
        certificate_index: usize,
        /// 底层错误。
        source: reqwest::Error,
    },
    /// 证书无法注册到 rustls root store。
    #[error(
        "Failed to register certificate #{certificate_index} from {} selected by {} in rustls root store: {source}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    RegisterRustlsCertificate {
        /// 来源环境变量名。
        source_env: &'static str,
        /// 证书路径。
        path: PathBuf,
        /// 证书序号。
        certificate_index: usize,
        /// 底层错误。
        source: rustls::Error,
    },
    /// 使用自定义 CA 构建 reqwest client 失败。
    #[error("Failed to build HTTP client while using CA bundle from {} ({}): {source}", source_env, path.display())]
    BuildClientWithCustomCa {
        /// 来源环境变量名。
        source_env: &'static str,
        /// 证书路径。
        path: PathBuf,
        /// 底层错误。
        source: reqwest::Error,
    },
    /// 使用系统根证书构建 reqwest client 失败。
    #[error("Failed to build HTTP client while using system root certificates: {0}")]
    BuildClientWithSystemRoots(reqwest::Error),
    /// 读取系统根证书失败。
    #[error("Failed to load native root certificates for custom CA transport: {0}")]
    LoadNativeRoots(io::Error),
}

/// 自定义 CA 结果类型。
pub type CustomCaResult<T> = Result<T, CustomCaError>;

/// 拼接完整 endpoint URL。
pub fn endpoint_url(base_url: &str, endpoint_path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        endpoint_path.trim_start_matches('/')
    )
}

/// 计算请求层路径。
pub fn endpoint_request_path(base_url: &str, endpoint_path: &str) -> String {
    let endpoint_path = endpoint_path.trim_start_matches('/');
    let base_path = reqwest::Url::parse(base_url)
        .ok()
        .map(|url| url.path().trim_end_matches('/').to_string())
        .filter(|path| !path.is_empty())
        .unwrap_or_default();

    if base_path.is_empty() {
        format!("/{endpoint_path}")
    } else {
        format!("{base_path}/{endpoint_path}")
    }
}

/// 返回 usage 相关 endpoint 的完整 URL 列表。
pub fn usage_endpoint_urls(base_url: &str) -> Vec<String> {
    usage_endpoint_paths(base_url)
        .into_iter()
        .map(|path| endpoint_url(base_url, path))
        .collect()
}

/// 返回 usage 主请求路径。
pub fn primary_usage_request_path(base_url: &str) -> String {
    let endpoint_path = usage_endpoint_paths(base_url)
        .into_iter()
        .next()
        .unwrap_or(CODEX_USAGE_API_PATH);
    endpoint_request_path(base_url, endpoint_path)
}

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
    pub usage: Option<TokenUsage>,
    /// 响应头里的最新 turn state。
    pub turn_state: Option<String>,
    /// 上游透传的 `set-cookie` 列表。
    pub set_cookie_headers: Vec<String>,
    /// 上游透传的限流头。
    pub rate_limit_headers: Vec<(String, String)>,
}

/// Codex Responses 实际使用的上游传输。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexBackendTransport {
    /// HTTP SSE transport.
    HttpSse,
    /// WebSocket transport.
    WebSocket,
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
}

/// 上游模型端点探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConnectivityProbe {
    /// 请求的完整端点。
    pub endpoint: String,
    /// 返回状态码。
    pub status: StatusCode,
}

/// Codex HTTP/SSE 上游客户端。
#[derive(Clone)]
pub struct CodexBackendClient {
    client: Client,
    base_url: String,
    fingerprint: Fingerprint,
    websocket_pool: Option<Arc<CodexWebSocketPool>>,
}

impl CodexBackendClient {
    /// 构造客户端。
    pub fn new(client: Client, base_url: impl Into<String>, fingerprint: Fingerprint) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            fingerprint,
            websocket_pool: None,
        }
    }

    /// 为 Responses WebSocket 请求启用连接池。
    pub fn with_websocket_pool(mut self, pool: Arc<CodexWebSocketPool>) -> Self {
        self.websocket_pool = Some(pool);
        self
    }

    /// 驱逐指定账号的 Responses WebSocket 池连接。
    pub async fn evict_websocket_account(&self, account_id: &str) {
        if let Some(pool) = &self.websocket_pool {
            pool.evict_account(account_id).await;
        }
    }

    /// 发送 Responses SSE 请求并读取完整响应。
    pub async fn create_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendResponse> {
        let upstream_request = response_upstream_request(request, context);
        match transport_for_request(&upstream_request) {
            CodexTransport::HttpSse => {
                self.create_response_http_sse(&upstream_request, context)
                    .await
            }
            CodexTransport::WebSocketPreferred | CodexTransport::WebSocketRequired => {
                match self
                    .create_response_websocket(&upstream_request, context)
                    .await
                {
                    Ok(response) => Ok(response),
                    Err(error)
                        if http_sse_fallback_allowed(&upstream_request)
                            && websocket_error_allows_http_fallback(&error) =>
                    {
                        tracing::warn!(error = %error, "websocket response failed; falling back to HTTP SSE");
                        self.create_response_http_sse(&upstream_request, context)
                            .await
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    /// 发送 Responses HTTP SSE 请求并返回 live 字节流。
    pub async fn create_response_stream(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let upstream_request = response_upstream_request(request, context);
        match transport_for_request(&upstream_request) {
            CodexTransport::HttpSse => {
                self.create_response_http_sse_stream(&upstream_request, context)
                    .await
            }
            CodexTransport::WebSocketPreferred | CodexTransport::WebSocketRequired => {
                match self
                    .create_response_websocket_stream(&upstream_request, context)
                    .await
                {
                    Ok(response) => Ok(response),
                    Err(error)
                        if http_sse_fallback_allowed(&upstream_request)
                            && websocket_error_allows_http_fallback(&error) =>
                    {
                        tracing::warn!(error = %error, "websocket response stream failed; falling back to HTTP SSE");
                        self.create_response_http_sse_stream(&upstream_request, context)
                            .await
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    async fn create_response_http_sse(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendResponse> {
        let headers = self.request_headers_for_http_response(upstream_request, context)?;
        let response = self
            .client
            .post(endpoint_url(&self.base_url, CODEX_RESPONSES_PATH))
            .headers(headers)
            .json(&upstream_request)
            .send()
            .await?;
        let status = response.status();
        let turn_state = turn_state(&response);
        let set_cookie_headers = set_cookie_headers(&response);
        let rate_limit_headers = rate_limit_headers(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);

        if !status.is_success() {
            let body = read_capped_error_body(response).await?;
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                set_cookie_headers,
            });
        }

        let body = response.text().await?;
        let usage = extract_sse_usage(&body).map_err(CodexClientError::InvalidSse)?;
        Ok(CodexBackendResponse {
            body,
            transport: CodexBackendTransport::HttpSse,
            usage,
            turn_state,
            set_cookie_headers,
            rate_limit_headers,
        })
    }

    async fn create_response_http_sse_stream(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let headers = self.request_headers_for_http_response(upstream_request, context)?;
        let response = self
            .client
            .post(endpoint_url(&self.base_url, CODEX_RESPONSES_PATH))
            .headers(headers)
            .json(&upstream_request)
            .send()
            .await?;
        let status = response.status();
        let turn_state = turn_state(&response);
        let set_cookie_headers = set_cookie_headers(&response);
        let rate_limit_headers = rate_limit_headers(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);

        if !status.is_success() {
            let body = read_capped_error_body(response).await?;
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                set_cookie_headers,
            });
        }

        Ok(CodexBackendStreamingResponse {
            body: Box::pin(response.bytes_stream().map_err(CodexClientError::Http)),
            transport: CodexBackendTransport::HttpSse,
            turn_state,
            set_cookie_headers,
            rate_limit_headers,
            rate_limit_header_updates: None,
            turn_state_update: None,
        })
    }

    async fn create_response_websocket(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendResponse> {
        let headers = self.request_headers_for_http_response(upstream_request, context)?;
        let prepared = CodexWebSocketConnection::responses_create_request(
            &self.base_url,
            &generate_key(),
            websocket_header_pairs(&headers),
            upstream_request,
        )
        .map_err(CodexClientError::WebSocketEncode)?;
        let artifact = websocket_audit_artifact_from_attempt(
            upstream_request,
            prepared.connection().opening_audit_snapshot(),
            websocket_payload_audit_snapshot(upstream_request),
        );
        if let Err(error) = write_websocket_audit_artifact_from_env(&artifact).await {
            tracing::warn!(error = %error, "failed to write Codex WebSocket audit artifact");
        }
        let pool_key = self.websocket_pool_key(upstream_request, context);
        let exchange = match (self.websocket_pool.as_deref(), pool_key) {
            (Some(pool), Some(key)) => {
                execute_response_create_request_with_pool(&prepared, Some((pool, key))).await
            }
            _ => execute_response_create_request(&prepared).await,
        }
        .map_err(websocket_exchange_error_to_client_error)?;

        Ok(CodexBackendResponse {
            body: exchange.body,
            transport: CodexBackendTransport::WebSocket,
            usage: exchange.usage,
            turn_state: exchange.turn_state,
            set_cookie_headers: exchange.set_cookie_headers,
            rate_limit_headers: exchange.rate_limit_headers,
        })
    }

    async fn create_response_websocket_stream(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let headers = self.request_headers_for_http_response(upstream_request, context)?;
        let prepared = CodexWebSocketConnection::responses_create_request(
            &self.base_url,
            &generate_key(),
            websocket_header_pairs(&headers),
            upstream_request,
        )
        .map_err(CodexClientError::WebSocketEncode)?;
        let artifact = websocket_audit_artifact_from_attempt(
            upstream_request,
            prepared.connection().opening_audit_snapshot(),
            websocket_payload_audit_snapshot(upstream_request),
        );
        if let Err(error) = write_websocket_audit_artifact_from_env(&artifact).await {
            tracing::warn!(error = %error, "failed to write Codex WebSocket audit artifact");
        }
        let pool_key = self.websocket_pool_key(upstream_request, context);
        let exchange = match (self.websocket_pool.as_deref(), pool_key) {
            (Some(pool), Some(key)) => {
                execute_response_create_request_stream_with_pool(&prepared, Some((pool, key))).await
            }
            _ => execute_response_create_request_stream_with_pool(&prepared, None).await,
        }
        .map_err(websocket_exchange_error_to_client_error)?;

        Ok(CodexBackendStreamingResponse {
            body: Box::pin(
                exchange
                    .body
                    .map_err(websocket_exchange_error_to_client_error),
            ),
            transport: CodexBackendTransport::WebSocket,
            turn_state: exchange.turn_state,
            set_cookie_headers: exchange.set_cookie_headers,
            rate_limit_headers: exchange.rate_limit_headers,
            rate_limit_header_updates: Some(exchange.rate_limit_header_updates),
            turn_state_update: Some(exchange.turn_state_update),
        })
    }

    fn websocket_pool_key(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> Option<CodexWebSocketPoolKey> {
        let account_id = context.account_id?;
        let conversation_id = request
            .prompt_cache_key
            .as_deref()
            .or(request.client_conversation_id.as_deref())
            .or(request.previous_response_id.as_deref())?;
        Some(CodexWebSocketPoolKey::new(
            &self.base_url,
            account_id,
            conversation_id,
        ))
    }

    /// 发送 compact JSON 请求并读取完整响应。
    pub async fn create_compact_response(
        &self,
        request: &CodexCompactRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexCompactResponse> {
        let headers = self.compact_request_headers(context)?;
        let response = self
            .client
            .post(endpoint_url(&self.base_url, CODEX_RESPONSES_COMPACT_PATH))
            .headers(headers)
            .json(request)
            .send()
            .await?;
        let status = response.status();
        let set_cookie_headers = set_cookie_headers(&response);
        let rate_limit_headers = rate_limit_headers(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let body = response.text().await?;

        if !status.is_success() {
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                set_cookie_headers,
            });
        }

        let parsed =
            serde_json::from_str::<Value>(&body).map_err(|_| CodexClientError::Upstream {
                status: StatusCode::BAD_GATEWAY,
                retry_after_seconds: None,
                body: format!(
                    "Compact response is not valid JSON: {}",
                    truncate_for_error(&body)
                ),
                set_cookie_headers: set_cookie_headers.clone(),
            })?;

        Ok(CodexCompactResponse {
            body: parsed,
            set_cookie_headers,
            rate_limit_headers,
        })
    }

    /// 获取 Codex usage JSON。
    pub async fn fetch_usage(&self, context: CodexRequestContext<'_>) -> CodexClientResult<Value> {
        let mut last_invalid_body = None;

        for endpoint in usage_endpoint_urls(&self.base_url) {
            let headers = self.auxiliary_request_headers(context)?;
            let response = self.client.get(endpoint).headers(headers).send().await?;
            let status = response.status();
            let retry_after_seconds = retry_after_seconds(response.headers(), None);

            if status == StatusCode::NOT_FOUND {
                last_invalid_body = Some(read_capped_error_body(response).await?);
                continue;
            }
            if !status.is_success() {
                let body = read_capped_error_body(response).await?;
                return Err(CodexClientError::Upstream {
                    status,
                    retry_after_seconds: retry_after_seconds
                        .or_else(|| retry_after_seconds_from_body(&body)),
                    body,
                    set_cookie_headers: Vec::new(),
                });
            }

            let body = response.text().await?;
            match serde_json::from_str::<Value>(&body) {
                Ok(parsed) if parsed.get("rate_limit").is_some() => return Ok(parsed),
                _ => last_invalid_body = Some(body),
            }
        }

        Err(CodexClientError::Upstream {
            status: StatusCode::BAD_GATEWAY,
            retry_after_seconds: None,
            body: last_invalid_body
                .map(|body| format!("invalid usage response: {}", truncate_for_error(&body)))
                .unwrap_or_else(|| "usage endpoint is unavailable".to_string()),
            set_cookie_headers: Vec::new(),
        })
    }

    /// 获取后端模型目录条目。
    pub async fn fetch_models(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<Vec<BackendModelEntry>> {
        let endpoints = [
            format!(
                "{}/codex/models?client_version={}",
                self.base_url, self.fingerprint.app_version
            ),
            format!("{}/models", self.base_url),
            format!("{}/sentinel/chat-requirements", self.base_url),
        ];

        for endpoint in endpoints {
            let headers = self.auxiliary_request_headers(context)?;
            let response = self.client.get(endpoint).headers(headers).send().await?;
            if !response.status().is_success() {
                continue;
            }
            let parsed = response.json::<Value>().await?;
            let models = extract_backend_model_entries(&parsed);
            if !models.is_empty() {
                return Ok(models);
            }
        }

        Err(CodexClientError::Upstream {
            status: StatusCode::BAD_GATEWAY,
            retry_after_seconds: None,
            body: "backend model catalog is unavailable".to_string(),
            set_cookie_headers: Vec::new(),
        })
    }

    /// 探测主模型端点是否可达。
    pub async fn probe_models_endpoint(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexConnectivityProbe> {
        let endpoint = format!(
            "{}/codex/models?client_version={}",
            self.base_url, self.fingerprint.app_version
        );
        let headers = self.auxiliary_request_headers(context)?;
        let response = self.client.get(&endpoint).headers(headers).send().await?;

        Ok(CodexConnectivityProbe {
            endpoint,
            status: response.status(),
        })
    }

    fn request_headers_for_http_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = self.request_headers(context)?;
        if let Some(subagent) = openai_subagent_from_metadata(request.client_metadata.as_ref()) {
            headers.insert(
                HeaderName::from_static("x-openai-subagent"),
                HeaderValue::from_str(&subagent)?,
            );
        }
        Ok(headers)
    }

    fn request_headers(&self, context: CodexRequestContext<'_>) -> CodexClientResult<HeaderMap> {
        let request_id = context.session_id.unwrap_or(context.request_id);
        let ordered_headers = build_ordered_codex_base_headers(
            &self.fingerprint,
            context.access_token,
            context.account_id,
        );

        let mut headers = HeaderMap::new();
        insert_ordered_headers(&mut headers, &ordered_headers)?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        insert_optional_header(&mut headers, "cookie", context.cookie_header)?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses_websockets=2026-02-06"),
        );
        headers.insert(
            HeaderName::from_static("x-openai-internal-codex-residency"),
            HeaderValue::from_static("us"),
        );
        headers.insert(
            HeaderName::from_static("x-client-request-id"),
            HeaderValue::from_str(request_id)?,
        );
        insert_optional_header(
            &mut headers,
            "x-codex-installation-id",
            context.installation_id,
        )?;
        insert_optional_header(&mut headers, "session_id", context.session_id)?;
        insert_optional_header(&mut headers, "x-codex-window-id", context.codex_window_id)?;
        insert_optional_header(&mut headers, "x-codex-turn-state", context.turn_state)?;
        insert_optional_header(&mut headers, "x-codex-turn-metadata", context.turn_metadata)?;
        insert_optional_header(&mut headers, "x-codex-beta-features", context.beta_features)?;
        insert_optional_header(
            &mut headers,
            "x-responsesapi-include-timing-metrics",
            context.include_timing_metrics,
        )?;
        insert_optional_header(&mut headers, "version", context.version)?;
        insert_optional_header(
            &mut headers,
            "x-codex-parent-thread-id",
            context.parent_thread_id,
        )?;

        Ok(headers)
    }

    fn auxiliary_request_headers(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let ordered_headers = build_ordered_codex_base_headers(
            &self.fingerprint,
            context.access_token,
            context.account_id,
        );

        let mut headers = HeaderMap::new();
        insert_ordered_headers(&mut headers, &ordered_headers)?;
        if let Some(cookie_header) = context.cookie_header {
            headers.insert(COOKIE, HeaderValue::from_str(cookie_header)?);
        }
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip, deflate"));
        insert_optional_header(
            &mut headers,
            "x-codex-installation-id",
            context.installation_id,
        )?;
        Ok(headers)
    }

    fn compact_request_headers(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let ordered_headers = build_ordered_codex_base_headers(
            &self.fingerprint,
            context.access_token,
            context.account_id,
        );

        let mut headers = HeaderMap::new();
        insert_ordered_headers(&mut headers, &ordered_headers)?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        insert_optional_header(&mut headers, "cookie", context.cookie_header)?;
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses_websockets=2026-02-06"),
        );
        headers.insert(
            HeaderName::from_static("x-openai-internal-codex-residency"),
            HeaderValue::from_static("us"),
        );
        headers.insert(
            HeaderName::from_static("x-client-request-id"),
            HeaderValue::from_str(&Uuid::new_v4().to_string())?,
        );
        insert_optional_header(
            &mut headers,
            "x-codex-installation-id",
            context.installation_id,
        )?;

        Ok(headers)
    }
}

#[async_trait]
impl CodexModelCatalogClient for CodexBackendClient {
    async fn fetch_models(
        &self,
        request: &CodexModelCatalogRequest<'_>,
    ) -> Result<Vec<BackendModelEntry>, CodexModelCatalogClientError> {
        Self::fetch_models(
            self,
            CodexRequestContext {
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
            },
        )
        .await
        .map_err(|error| CodexModelCatalogClientError::RequestFailed {
            message: error.to_string(),
        })
    }
}

/// 构建基础的 Codex 请求头集合。
pub fn build_codex_base_headers(
    fingerprint: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
) -> IndexMap<String, String> {
    let mut headers = IndexMap::new();

    headers.insert(
        "authorization".to_string(),
        format!("Bearer {access_token}"),
    );
    if let Some(account_id) = account_id {
        headers.insert("chatgpt-account-id".to_string(), account_id.to_string());
    }
    headers.insert("originator".to_string(), fingerprint.originator.clone());
    headers.insert("user-agent".to_string(), fingerprint.user_agent());
    headers.insert("sec-ch-ua".to_string(), fingerprint.sec_ch_ua());

    for (key, value) in &fingerprint.default_headers {
        let key_lower = key.to_ascii_lowercase();
        if !headers.contains_key(&key_lower) {
            headers.insert(key_lower, value.clone());
        }
    }

    headers
}

/// 构造 Responses HTTP/SSE 请求头集合。
pub fn build_codex_headers(
    fingerprint: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
    turn_state: Option<&str>,
    request_id: &str,
) -> IndexMap<String, String> {
    let mut headers = build_codex_base_headers(fingerprint, access_token, account_id);
    headers.insert(
        "x-openai-internal-codex-residency".to_string(),
        "us".to_string(),
    );
    headers.insert("x-client-request-id".to_string(), request_id.to_string());

    if let Some(turn_state) = turn_state {
        headers.insert("x-codex-turn-state".to_string(), turn_state.to_string());
    }

    headers.insert("accept".to_string(), "text/event-stream".to_string());
    headers
}

/// 按指纹声明顺序重排请求头。
pub fn order_headers(
    headers: IndexMap<String, String>,
    order: &[String],
) -> IndexMap<String, String> {
    let mut ordered = IndexMap::new();

    for key in order {
        if let Some(value) = headers.get(key) {
            ordered.insert(key.clone(), value.clone());
        }
    }

    for (key, value) in headers {
        if !ordered.contains_key(&key) {
            ordered.insert(key, value);
        }
    }

    ordered
}

/// 构造并按指纹顺序重排 Responses 请求头。
pub fn build_ordered_codex_headers(
    fingerprint: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
    turn_state: Option<&str>,
    request_id: &str,
) -> IndexMap<String, String> {
    let headers = build_codex_headers(
        fingerprint,
        access_token,
        account_id,
        turn_state,
        request_id,
    );
    order_headers(headers, &fingerprint.header_order)
}

/// 构造并按指纹顺序重排基础请求头。
pub fn build_ordered_codex_base_headers(
    fingerprint: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
) -> IndexMap<String, String> {
    let headers = build_codex_base_headers(fingerprint, access_token, account_id);
    order_headers(headers, &fingerprint.header_order)
}

/// 按 TLS 配置和自定义 CA 环境复用 reqwest client。
pub fn build_reqwest_client(force_http11: bool) -> Result<Client, CustomCaError> {
    let cache_key = (force_http11, custom_ca_env_cache_key());
    static CLIENTS: OnceLock<ReqwestClientCache> = OnceLock::new();
    let cache = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut clients = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

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

fn response_upstream_request(
    request: &CodexResponsesRequest,
    context: CodexRequestContext<'_>,
) -> CodexResponsesRequest {
    let mut upstream = request.clone();
    if let Some(session_id) = context.session_id {
        upstream.prompt_cache_key = Some(session_id.to_string());
    }
    upstream.client_metadata = response_client_metadata(request.client_metadata.as_ref(), context);
    upstream
}

fn response_client_metadata(
    client_metadata: Option<&Value>,
    context: CodexRequestContext<'_>,
) -> Option<Value> {
    let mut metadata = Map::new();
    if let Some(Value::Object(input)) = client_metadata {
        for (key, value) in input {
            if let Some(value) = value.as_str() {
                metadata.insert(key.clone(), Value::String(value.to_string()));
            }
        }
    }

    insert_metadata_string(
        &mut metadata,
        "x-codex-installation-id",
        context.installation_id,
    );
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

fn extract_backend_model_entries(value: &Value) -> Vec<BackendModelEntry> {
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
            entries.extend(nested.iter().filter_map(parse_backend_model_entry));
        } else if let Some(entry) = parse_backend_model_entry(model) {
            entries.push(entry);
        }
    }
    entries
}

fn parse_backend_model_entry(value: &Value) -> Option<BackendModelEntry> {
    let entry = serde_json::from_value::<BackendModelEntry>(value.clone()).ok()?;
    (entry.slug.is_some()
        || entry.id.is_some()
        || entry.name.is_some()
        || entry.display_name.is_some()
        || entry.title.is_some())
    .then_some(entry)
}

fn insert_optional_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: Option<&str>,
) -> CodexClientResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    headers.insert(HeaderName::from_static(name), HeaderValue::from_str(value)?);
    Ok(())
}

fn insert_ordered_headers(
    headers: &mut HeaderMap,
    ordered_headers: &IndexMap<String, String>,
) -> CodexClientResult<()> {
    for (name, value) in ordered_headers {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(())
}

fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

fn websocket_header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    let pairs = header_pairs(headers)
        .into_iter()
        .filter(|(name, _)| {
            !name.eq_ignore_ascii_case("content-type") && !name.eq_ignore_ascii_case("accept")
        })
        .collect::<Vec<_>>();

    pairs
}

fn websocket_exchange_error_to_client_error(
    error: CodexWebSocketExchangeError,
) -> CodexClientError {
    match error {
        CodexWebSocketExchangeError::Upstream {
            status_code,
            retry_after_seconds,
            body,
            set_cookie_headers,
        } => CodexClientError::Upstream {
            status: StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY),
            body,
            retry_after_seconds,
            set_cookie_headers,
        },
        error => CodexClientError::WebSocket(error),
    }
}

fn websocket_error_allows_http_fallback(error: &CodexClientError) -> bool {
    !matches!(error, CodexClientError::Upstream { .. })
}

async fn read_capped_error_body(response: ReqwestResponse) -> Result<String, reqwest::Error> {
    let body = response.bytes().await?;
    let len = body.len().min(MAX_UPSTREAM_ERROR_BODY_BYTES);
    Ok(String::from_utf8_lossy(&body[..len]).into_owned())
}

fn turn_state(response: &ReqwestResponse) -> Option<String> {
    response
        .headers()
        .get("x-codex-turn-state")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn set_cookie_headers(response: &ReqwestResponse) -> Vec<String> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToString::to_string))
        .collect()
}

fn rate_limit_headers(headers: &HeaderMap) -> Vec<(String, String)> {
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

fn retry_after_seconds(headers: &HeaderMap, body: Option<&str>) -> Option<u64> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .or_else(|| body.and_then(retry_after_seconds_from_body))
}

fn truncate_for_error(body: &str) -> String {
    body.chars().take(200).collect()
}

fn usage_endpoint_paths(base_url: &str) -> Vec<&'static str> {
    if has_backend_api_base_path(base_url) {
        vec![WHAM_USAGE_PATH, CODEX_USAGE_PATH]
    } else {
        vec![CODEX_USAGE_API_PATH, CODEX_USAGE_PATH]
    }
}

fn has_backend_api_base_path(base_url: &str) -> bool {
    reqwest::Url::parse(base_url).ok().is_some_and(|url| {
        url.path()
            .split('/')
            .any(|segment| segment == "backend-api")
    })
}

/// 在 reqwest builder 上应用自定义 CA。
pub fn build_reqwest_client_with_custom_ca(
    builder: reqwest::ClientBuilder,
) -> CustomCaResult<reqwest::Client> {
    build_reqwest_client_with_env(&ProcessEnv, builder)
}

/// 返回当前自定义 CA 的缓存键。
pub fn custom_ca_env_cache_key() -> Option<String> {
    ProcessEnv
        .configured_ca_bundle()
        .map(|bundle| format!("{}={}", bundle.source_env, bundle.path.display()))
}

/// 构建 rustls client config，若未配置自定义 CA 则返回 `None`。
pub fn maybe_build_rustls_client_config_with_custom_ca(
) -> CustomCaResult<Option<std::sync::Arc<ClientConfig>>> {
    maybe_build_rustls_client_config_with_env(&ProcessEnv)
}

fn build_reqwest_client_with_env(
    env_source: &dyn EnvSource,
    mut builder: reqwest::ClientBuilder,
) -> CustomCaResult<reqwest::Client> {
    let Some(bundle) = env_source.configured_ca_bundle() else {
        return builder
            .build()
            .map_err(CustomCaError::BuildClientWithSystemRoots);
    };

    builder = builder.use_rustls_tls();
    for (idx, cert) in bundle.load_certificates()?.iter().enumerate() {
        let certificate = reqwest::Certificate::from_der(cert.as_ref()).map_err(|source| {
            CustomCaError::RegisterCertificate {
                source_env: bundle.source_env,
                path: bundle.path.clone(),
                certificate_index: idx + 1,
                source,
            }
        })?;
        builder = builder.add_root_certificate(certificate);
    }

    builder
        .build()
        .map_err(|source| CustomCaError::BuildClientWithCustomCa {
            source_env: bundle.source_env,
            path: bundle.path,
            source,
        })
}

fn maybe_build_rustls_client_config_with_env(
    env_source: &dyn EnvSource,
) -> CustomCaResult<Option<std::sync::Arc<ClientConfig>>> {
    let Some(bundle) = env_source.configured_ca_bundle() else {
        return Ok(None);
    };

    let mut root_store = native_root_store().map_err(CustomCaError::LoadNativeRoots)?;
    for (idx, cert) in bundle.load_certificates()?.into_iter().enumerate() {
        root_store
            .add(cert)
            .map_err(|source| CustomCaError::RegisterRustlsCertificate {
                source_env: bundle.source_env,
                path: bundle.path.clone(),
                certificate_index: idx + 1,
                source,
            })?;
    }

    Ok(Some(std::sync::Arc::new(
        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth(),
    )))
}

pub(crate) fn native_root_store() -> Result<RootCertStore, io::Error> {
    let mut root_store = RootCertStore::empty();
    let rustls_native_certs::CertificateResult { certs, errors, .. } =
        rustls_native_certs::load_native_certs();
    if !errors.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to load native root certificates: {errors:?}"),
        ));
    }

    let (added, _) = root_store.add_parsable_certificates(certs);
    if added == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no native root certificates found",
        ));
    }

    Ok(root_store)
}

trait EnvSource {
    fn var(&self, key: &str) -> Option<String>;

    fn non_empty_path(&self, key: &str) -> Option<PathBuf> {
        self.var(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    fn configured_ca_bundle(&self) -> Option<ConfiguredCaBundle> {
        self.non_empty_path(CODEX_CA_CERT_ENV)
            .map(|path| ConfiguredCaBundle {
                source_env: CODEX_CA_CERT_ENV,
                path,
            })
            .or_else(|| {
                self.non_empty_path(SSL_CERT_FILE_ENV)
                    .map(|path| ConfiguredCaBundle {
                        source_env: SSL_CERT_FILE_ENV,
                        path,
                    })
            })
    }
}

struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }
}

struct ConfiguredCaBundle {
    source_env: &'static str,
    path: PathBuf,
}

impl ConfiguredCaBundle {
    fn load_certificates(&self) -> CustomCaResult<Vec<CertificateDer<'static>>> {
        let pem_data = fs::read(&self.path).map_err(|source| CustomCaError::ReadCaFile {
            source_env: self.source_env,
            path: self.path.clone(),
            source,
        })?;
        let normalized = normalize_trusted_certificate_labels(&pem_data);
        let mut certificates = Vec::new();
        for section in PemSection::pem_slice_iter(normalized.as_bytes()) {
            let (kind, der) = section.map_err(|error| self.pem_parse_error(&error))?;
            if kind == SectionKind::Certificate {
                certificates.push(CertificateDer::from(der));
            }
        }
        if certificates.is_empty() {
            return Err(self.pem_parse_error(&pem::Error::NoItemsFound));
        }
        Ok(certificates)
    }

    fn pem_parse_error(&self, error: &pem::Error) -> CustomCaError {
        let detail = match error {
            pem::Error::NoItemsFound => "no certificates found in PEM file".to_string(),
            _ => format!("failed to parse PEM file: {error}"),
        };
        CustomCaError::InvalidCaFile {
            source_env: self.source_env,
            path: self.path.clone(),
            detail,
        }
    }
}

fn normalize_trusted_certificate_labels(input: &[u8]) -> String {
    String::from_utf8_lossy(input).replace("TRUSTED CERTIFICATE", "CERTIFICATE")
}
