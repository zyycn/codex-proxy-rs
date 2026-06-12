use reqwest::{
    header::{
        HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE, COOKIE, RETRY_AFTER, SET_COOKIE,
    },
    Client, Response as ReqwestResponse, StatusCode,
};
use serde_json::Value;
use thiserror::Error;

use crate::{
    codex::{
        headers::build_codex_headers,
        sse::SseError,
        types::CodexResponsesRequest,
        usage::{extract_sse_usage, TokenUsage},
        websocket::{
            create_response_via_websocket, ensure_http_sse_supported, transport_for_request,
            CodexTransport, CodexWebSocketError, WebSocketSupportError,
        },
    },
    fingerprint::model::Fingerprint,
    models::catalog::BackendModelEntry,
};

#[derive(Debug, Error)]
pub enum CodexClientError {
    #[error("http transport error: {0}")]
    Http(#[from] reqwest::Error),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexBackendResponse {
    pub body: String,
    pub usage: Option<TokenUsage>,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
}

pub struct CodexBackendStream {
    pub response: ReqwestResponse,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
}

#[derive(Clone)]
pub struct CodexBackendClient {
    client: Client,
    base_url: String,
    fingerprint: Fingerprint,
}

impl CodexBackendClient {
    pub fn new(client: Client, base_url: impl Into<String>, fingerprint: Fingerprint) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            fingerprint,
        }
    }

    pub async fn create_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendResponse> {
        if transport_for_request(request) == CodexTransport::WebSocketRequired {
            let body = create_response_via_websocket(
                &self.base_url,
                request,
                self.request_headers(context)?,
            )
            .await?;
            let usage = extract_sse_usage(&body)?;
            return Ok(CodexBackendResponse {
                body,
                usage,
                turn_state: None,
                set_cookie_headers: Vec::new(),
            });
        }

        let response = self.send_response_request(request, context).await?;
        let status = response.status();
        let turn_state = turn_state(&response);
        let set_cookie_headers = set_cookie_headers(&response);
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
        let usage = extract_sse_usage(&body)?;

        Ok(CodexBackendResponse {
            body,
            usage,
            turn_state,
            set_cookie_headers,
        })
    }

    pub async fn stream_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendStream> {
        let response = self.send_response_request(request, context).await?;
        let status = response.status();
        let turn_state = turn_state(&response);
        let set_cookie_headers = set_cookie_headers(&response);
        if !status.is_success() {
            let retry_after_seconds = retry_after_seconds(response.headers(), None);
            let body = response.text().await?;
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
        })
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
            let mut headers = self.request_headers(context)?;
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
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

    pub async fn fetch_usage(&self, context: CodexRequestContext<'_>) -> CodexClientResult<Value> {
        let mut last_invalid_body = None;
        for endpoint in self.usage_endpoints() {
            let mut headers = self.request_headers(context)?;
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
            let response = self.client.get(endpoint).headers(headers).send().await?;
            let status = response.status();
            let retry_after_seconds = retry_after_seconds(response.headers(), None);
            let body = response.text().await?;
            if status == StatusCode::NOT_FOUND {
                last_invalid_body = Some(body);
                continue;
            }
            if !status.is_success() {
                return Err(CodexClientError::Upstream {
                    status,
                    retry_after_seconds: retry_after_seconds
                        .or_else(|| retry_after_seconds_from_body(&body)),
                    body,
                });
            }
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
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<ReqwestResponse> {
        ensure_http_sse_supported(request)?;
        let url = format!("{}/codex/responses", self.base_url);
        Ok(self
            .client
            .post(url)
            .headers(self.request_headers(context)?)
            .json(request)
            .send()
            .await?)
    }

    fn usage_endpoints(&self) -> Vec<String> {
        if self.base_url.contains("/backend-api") {
            vec![
                format!("{}/wham/usage", self.base_url),
                format!("{}/codex/usage", self.base_url),
            ]
        } else {
            vec![
                format!("{}/api/codex/usage", self.base_url),
                format!("{}/codex/usage", self.base_url),
            ]
        }
    }

    fn request_headers(&self, context: CodexRequestContext<'_>) -> CodexClientResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        for (name, value) in build_codex_headers(
            &self.fingerprint,
            context.access_token,
            context.account_id,
            context.turn_state,
            context.request_id,
        ) {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(&value)?,
            );
        }
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses_websockets=2026-02-06"),
        );
        headers.insert(
            HeaderName::from_static("x-openai-internal-codex-residency"),
            HeaderValue::from_static("us"),
        );
        if let Some(cookie_header) = context.cookie_header {
            headers.insert(COOKIE, HeaderValue::from_str(cookie_header)?);
        }
        insert_optional_header(&mut headers, "x-codex-turn-metadata", context.turn_metadata)?;
        insert_optional_header(&mut headers, "x-codex-beta-features", context.beta_features)?;
        insert_optional_header(
            &mut headers,
            "x-responsesapi-include-timing-metrics",
            context.include_timing_metrics,
        )?;
        insert_optional_header(&mut headers, "version", context.version)?;
        insert_optional_header(&mut headers, "x-codex-window-id", context.codex_window_id)?;
        insert_optional_header(
            &mut headers,
            "x-codex-parent-thread-id",
            context.parent_thread_id,
        )?;
        Ok(headers)
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

pub fn build_reqwest_client(force_http11: bool) -> Result<Client, reqwest::Error> {
    // Codex Desktop 指纹依赖 reqwest/rustls 组合，升级前必须重新验证 TLS 行为。
    let builder = Client::builder()
        .use_rustls_tls()
        .no_proxy()
        .gzip(true)
        .brotli(true)
        .zstd(true)
        .deflate(true);
    let builder = if force_http11 {
        builder.http1_only()
    } else {
        builder
    };
    builder.build()
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

fn retry_after_seconds(headers: &HeaderMap, body: Option<&str>) -> Option<u64> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .or_else(|| body.and_then(retry_after_seconds_from_body))
}

fn retry_after_seconds_from_body(body: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
    if let Some(seconds) = error
        .get("resets_in_seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0)
    {
        return Some(seconds);
    }
    let resets_at = error.get("resets_at").and_then(Value::as_u64)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    (resets_at > now).then_some(resets_at - now)
}

fn truncate_for_error(body: &str) -> String {
    body.chars().take(200).collect()
}
