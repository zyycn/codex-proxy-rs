use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use futures::StreamExt;
use indexmap::IndexMap;
use reqwest::{
    header::{
        HeaderMap, HeaderName, HeaderValue, ACCEPT, ACCEPT_ENCODING, CONTENT_TYPE, COOKIE,
        RETRY_AFTER, SET_COOKIE,
    },
    Client, Response as ReqwestResponse, StatusCode,
};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    codex::gateway::fingerprint::model::Fingerprint,
    codex::gateway::transport::{
        custom_ca::{build_reqwest_client_with_custom_ca, custom_ca_env_cache_key, CustomCaError},
        endpoints::{
            endpoint_url, usage_endpoint_urls, CODEX_RESPONSES_COMPACT_PATH, CODEX_RESPONSES_PATH,
        },
        headers::build_ordered_codex_base_headers,
        retry_after::retry_after_seconds_from_body,
        sse::SseError,
        types::{CodexCompactRequest, CodexResponsesRequest},
        usage_events::{extract_sse_usage, TokenUsage},
        websocket::{
            append_rate_limit_updates, create_response_via_websocket,
            create_response_via_websocket_stream, create_response_via_websocket_stream_with_pool,
            latest_turn_state, transport_for_request, CodexTransport, CodexWebSocketError,
            CodexWebSocketPool, CodexWebSocketPoolKey, CodexWebSocketSseStream,
            CodexWebSocketStreamResponse, SharedRateLimitUpdates, SharedTurnState,
            WebSocketSupportError,
        },
    },
    codex::models::catalog::BackendModelEntry,
};

#[derive(Debug, Error)]
pub enum CodexClientError {
    #[error("http transport error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("custom CA transport error: {0}")]
    CustomCa(#[from] CustomCaError),
    #[error("invalid request header name: {0}")]
    InvalidHeaderName(#[from] reqwest::header::InvalidHeaderName),
    #[error("invalid request header value: {0}")]
    InvalidHeaderValue(#[from] reqwest::header::InvalidHeaderValue),
    #[error("unsupported transport: {0}")]
    UnsupportedTransport(#[from] WebSocketSupportError),
    #[error("websocket transport error: {0}")]
    WebSocket(#[from] CodexWebSocketError),
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
    #[error("upstream returned status {status}: {body}")]
    Upstream {
        status: StatusCode,
        body: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("backend model catalog is unavailable")]
    ModelsUnavailable,
}

pub type CodexClientResult<T> = Result<T, CodexClientError>;

#[derive(Debug, Clone, Copy)]
pub struct CodexRequestContext<'a> {
    pub access_token: &'a str,
    pub account_id: Option<&'a str>,
    pub request_id: &'a str,
    pub turn_state: Option<&'a str>,
    pub turn_metadata: Option<&'a str>,
    pub beta_features: Option<&'a str>,
    pub include_timing_metrics: Option<&'a str>,
    pub version: Option<&'a str>,
    pub codex_window_id: Option<&'a str>,
    pub parent_thread_id: Option<&'a str>,
    pub cookie_header: Option<&'a str>,
    pub installation_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexBackendResponse {
    pub body: String,
    pub usage: Option<TokenUsage>,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexCompactResponse {
    pub body: Value,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConnectivityProbe {
    pub endpoint: String,
    pub status: StatusCode,
}

pub struct CodexBackendStream {
    pub response: ReqwestResponse,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
}

pub struct CodexBackendWebSocketStream {
    pub body_stream: CodexWebSocketSseStream,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
    pub rate_limit_updates: SharedRateLimitUpdates,
    pub turn_state_updates: SharedTurnState,
}

const MAX_UPSTREAM_ERROR_BODY_BYTES: usize = 1024 * 1024;
type ReqwestClientCacheKey = (bool, Option<String>);
type ReqwestClientCache = Mutex<HashMap<ReqwestClientCacheKey, Client>>;

#[derive(Clone)]
pub struct CodexBackendClient {
    client: Client,
    base_url: String,
    fingerprint: Fingerprint,
    websocket_pool: Option<Arc<CodexWebSocketPool>>,
    websocket_pool_account_id: Option<String>,
}

impl CodexBackendClient {
    pub fn new(client: Client, base_url: impl Into<String>, fingerprint: Fingerprint) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            fingerprint,
            websocket_pool: None,
            websocket_pool_account_id: None,
        }
    }

    pub fn with_websocket_pool(
        mut self,
        pool: Arc<CodexWebSocketPool>,
        account_id: impl Into<String>,
    ) -> Self {
        self.websocket_pool = Some(pool);
        self.websocket_pool_account_id = Some(account_id.into());
        self
    }

    #[tracing::instrument(
        skip(self, request, context),
        fields(
            request_id = %context.request_id,
            account_id = %context.account_id.unwrap_or("unknown"),
            model = %request.model,
            transport = ?transport_for_request(request),
        )
    )]
    pub async fn create_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendResponse> {
        let transport = transport_for_request(request);
        let upstream_request = response_upstream_request(request, context);
        if matches!(
            transport,
            CodexTransport::WebSocketPreferred | CodexTransport::WebSocketRequired
        ) {
            let headers =
                self.request_headers_for_websocket_response(&upstream_request, context)?;
            match self
                .create_response_via_configured_websocket(&upstream_request, headers.clone())
                .await
            {
                Ok(response) => {
                    let usage = extract_sse_usage(&response.body)?;
                    return Ok(CodexBackendResponse {
                        body: response.body,
                        usage,
                        turn_state: response.turn_state,
                        set_cookie_headers: response.set_cookie_headers,
                        rate_limit_headers: response.rate_limit_headers,
                    });
                }
                Err(error)
                    if transport == CodexTransport::WebSocketPreferred
                        && websocket_error_allows_http_sse_fallback(&error) =>
                {
                    tracing::info!(
                        error = %error,
                        "Codex WebSocket 不可用，降级到 HTTP SSE"
                    );
                }
                Err(error) => return Err(codex_websocket_error(error)),
            }
        }

        let headers = self.request_headers_for_http_response(&upstream_request, context)?;
        let response = self
            .send_response_request(&upstream_request, headers)
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
            });
        }
        let body = response.text().await?;
        let usage = extract_sse_usage(&body)?;

        Ok(CodexBackendResponse {
            body,
            usage,
            turn_state,
            set_cookie_headers,
            rate_limit_headers,
        })
    }

    #[tracing::instrument(
        skip(self, request, context),
        fields(
            request_id = %context.request_id,
            account_id = %context.account_id.unwrap_or("unknown"),
            model = %request.model,
            transport = "http_json",
        )
    )]
    pub async fn create_compact_response(
        &self,
        request: &CodexCompactRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexCompactResponse> {
        let headers = self.compact_request_headers(context)?;
        let response = self.send_compact_request(request, headers).await?;
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
            })?;

        Ok(CodexCompactResponse {
            body: parsed,
            set_cookie_headers,
            rate_limit_headers,
        })
    }

    #[tracing::instrument(
        skip(self, request, context),
        fields(
            request_id = %context.request_id,
            account_id = %context.account_id.unwrap_or("unknown"),
            model = %request.model,
            transport = "http_sse",
        )
    )]
    pub async fn stream_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendStream> {
        let upstream_request = response_upstream_request(request, context);
        let headers = self.request_headers_for_http_response(&upstream_request, context)?;
        let response = self
            .send_response_request(&upstream_request, headers)
            .await?;
        let status = response.status();
        let turn_state = turn_state(&response);
        let set_cookie_headers = set_cookie_headers(&response);
        let rate_limit_headers = rate_limit_headers(response.headers());
        if !status.is_success() {
            let retry_after_seconds = retry_after_seconds(response.headers(), None);
            let body = read_capped_error_body(response).await?;
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
            });
        }

        Ok(CodexBackendStream {
            response,
            turn_state,
            set_cookie_headers,
            rate_limit_headers,
        })
    }

    #[tracing::instrument(
        skip(self, request, context),
        fields(
            request_id = %context.request_id,
            account_id = %context.account_id.unwrap_or("unknown"),
            model = %request.model,
            transport = "websocket",
        )
    )]
    pub async fn websocket_stream_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendWebSocketStream> {
        let upstream_request = response_upstream_request(request, context);
        let headers = self.request_headers_for_websocket_response(&upstream_request, context)?;
        let response = self
            .create_stream_via_configured_websocket(&upstream_request, headers)
            .await
            .map_err(codex_websocket_error)?;

        Ok(CodexBackendWebSocketStream {
            body_stream: response.body_stream,
            turn_state: response.turn_state,
            set_cookie_headers: response.set_cookie_headers,
            rate_limit_headers: response.rate_limit_headers,
            rate_limit_updates: response.rate_limit_updates,
            turn_state_updates: response.turn_state_updates,
        })
    }

    async fn create_response_via_configured_websocket(
        &self,
        request: &CodexResponsesRequest,
        headers: HeaderMap,
    ) -> Result<
        crate::codex::gateway::transport::websocket::CodexWebSocketResponse,
        CodexWebSocketError,
    > {
        let Some((pool, pool_key)) = self.websocket_pool_key(request) else {
            return create_response_via_websocket(&self.base_url, request, headers).await;
        };
        let response = create_response_via_websocket_stream_with_pool(
            &self.base_url,
            request,
            headers,
            pool,
            pool_key,
        )
        .await?;
        collect_websocket_stream_response(response).await
    }

    async fn create_stream_via_configured_websocket(
        &self,
        request: &CodexResponsesRequest,
        headers: HeaderMap,
    ) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError> {
        let Some((pool, pool_key)) = self.websocket_pool_key(request) else {
            return create_response_via_websocket_stream(&self.base_url, request, headers).await;
        };
        create_response_via_websocket_stream_with_pool(
            &self.base_url,
            request,
            headers,
            pool,
            pool_key,
        )
        .await
    }

    fn websocket_pool_key(
        &self,
        request: &CodexResponsesRequest,
    ) -> Option<(Arc<CodexWebSocketPool>, CodexWebSocketPoolKey)> {
        let pool = self.websocket_pool.clone()?;
        let account_id = self.websocket_pool_account_id.as_deref()?;
        let conversation_id = request
            .prompt_cache_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        Some((
            pool,
            CodexWebSocketPoolKey::new(&self.base_url, account_id, conversation_id),
        ))
    }

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

        Err(CodexClientError::ModelsUnavailable)
    }

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

    pub async fn fetch_usage(&self, context: CodexRequestContext<'_>) -> CodexClientResult<Value> {
        let mut last_invalid_body = None;
        for endpoint in self.usage_endpoints() {
            let headers = self.auxiliary_request_headers(context)?;
            let response = self.client.get(endpoint).headers(headers).send().await?;
            let status = response.status();
            let retry_after_seconds = retry_after_seconds(response.headers(), None);
            if status == StatusCode::NOT_FOUND {
                let body = read_capped_error_body(response).await?;
                last_invalid_body = Some(body);
                continue;
            }
            if !status.is_success() {
                let body = read_capped_error_body(response).await?;
                return Err(CodexClientError::Upstream {
                    status,
                    retry_after_seconds: retry_after_seconds
                        .or_else(|| retry_after_seconds_from_body(&body)),
                    body,
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
        })
    }

    async fn send_response_request(
        &self,
        request: &CodexResponsesRequest,
        headers: HeaderMap,
    ) -> CodexClientResult<ReqwestResponse> {
        let url = endpoint_url(&self.base_url, CODEX_RESPONSES_PATH);
        Ok(self
            .client
            .post(url)
            .headers(headers)
            .json(request)
            .send()
            .await?)
    }

    async fn send_compact_request(
        &self,
        request: &CodexCompactRequest,
        headers: HeaderMap,
    ) -> CodexClientResult<ReqwestResponse> {
        let url = endpoint_url(&self.base_url, CODEX_RESPONSES_COMPACT_PATH);
        Ok(self
            .client
            .post(url)
            .headers(headers)
            .json(request)
            .send()
            .await?)
    }

    fn usage_endpoints(&self) -> Vec<String> {
        usage_endpoint_urls(&self.base_url)
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

    fn request_headers_for_websocket_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = self.request_headers_for_http_response(request, context)?;
        headers.remove(CONTENT_TYPE);
        headers.remove(ACCEPT);
        Ok(headers)
    }
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

async fn read_capped_error_body(response: ReqwestResponse) -> Result<String, reqwest::Error> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = MAX_UPSTREAM_ERROR_BODY_BYTES.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        if chunk.len() <= remaining {
            body.extend_from_slice(&chunk);
        } else {
            body.extend_from_slice(&chunk[..remaining]);
            break;
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

async fn collect_websocket_stream_response(
    response: CodexWebSocketStreamResponse,
) -> Result<crate::codex::gateway::transport::websocket::CodexWebSocketResponse, CodexWebSocketError>
{
    let CodexWebSocketStreamResponse {
        mut body_stream,
        turn_state,
        set_cookie_headers,
        mut rate_limit_headers,
        rate_limit_updates,
        turn_state_updates,
    } = response;
    let mut body = String::new();
    while let Some(chunk) = body_stream.next().await {
        body.push_str(&chunk?);
    }
    if body.is_empty() {
        return Err(CodexWebSocketError::EmptyResponse);
    }
    append_rate_limit_updates(&mut rate_limit_headers, &rate_limit_updates).await;
    let turn_state = latest_turn_state(&turn_state_updates).await.or(turn_state);
    Ok(
        crate::codex::gateway::transport::websocket::CodexWebSocketResponse {
            body,
            turn_state,
            set_cookie_headers,
            rate_limit_headers,
        },
    )
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

    // 上游安全链路要求 body metadata 与 header 中的派生身份保持一致。
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
    (entry.slug.is_some() || entry.id.is_some() || entry.name.is_some()).then_some(entry)
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

fn websocket_error_allows_http_sse_fallback(error: &CodexWebSocketError) -> bool {
    match error {
        CodexWebSocketError::Upstream { status, .. } => *status == StatusCode::UPGRADE_REQUIRED,
        CodexWebSocketError::InvalidRequest(_)
        | CodexWebSocketError::Encode(_)
        | CodexWebSocketError::Transport(_)
        | CodexWebSocketError::OpenTimeout { .. }
        | CodexWebSocketError::SendIdleTimeout { .. }
        | CodexWebSocketError::ReceiveIdleTimeout { .. }
        | CodexWebSocketError::EmptyResponse
        | CodexWebSocketError::UnexpectedBinaryEvent
        | CodexWebSocketError::ClosedByServerBeforeCompleted
        | CodexWebSocketError::StreamClosedBeforeCompleted
        | CodexWebSocketError::IncompleteResponse { .. }
        | CodexWebSocketError::InvalidCompletedResponse { .. } => false,
    }
}

fn codex_websocket_error(error: CodexWebSocketError) -> CodexClientError {
    match error {
        CodexWebSocketError::Upstream {
            status,
            body,
            retry_after_seconds,
        } => CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
        },
        error => CodexClientError::WebSocket(error),
    }
}
