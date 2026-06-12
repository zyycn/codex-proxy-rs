use std::{sync::Arc, time::Instant};

use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{Duration, Utc};
use futures::{stream, StreamExt};
use reqwest::Url;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::{
    accounts::{
        model::{Account, AccountStatus},
        pool::{AccountAcquireRequest, AccountPool},
        repository::{TokenUpdate, UsageDelta},
    },
    auth::refresh::RefreshFailure,
    codex::{
        client::{
            build_reqwest_client, CodexBackendClient, CodexBackendStream, CodexClientError,
            CodexRequestContext,
        },
        sse::parse_sse_events,
        types::CodexResponsesRequest,
        usage::{extract_sse_usage, TokenUsage},
        websocket::{transport_for_request, CodexTransport},
    },
    fingerprint::model::Fingerprint,
    http::{auth::client_api_key, middleware::RequestId},
    logs::event::{EventLevel, EventLog},
    models::catalog::ModelCatalog,
    state::AppState,
    translation::{
        codex_to_openai::{
            chat_completion_from_codex_sse, chat_completion_stream_from_codex_sse, openai_error,
        },
        openai_to_codex::{translate_chat_to_codex, ChatCompletionRequest},
    },
};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;
const DEFAULT_RATE_LIMIT_BACKOFF_SECONDS: u64 = 60;
const MAX_RATE_LIMIT_BACKOFF_SECONDS: u64 = 86_400 * 7;
const CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS: u64 = 10;

#[derive(Deserialize)]
struct ResponsesBody {
    model: Option<String>,
    input: Option<Vec<Value>>,
    instructions: Option<String>,
    reasoning: Option<Value>,
    tools: Option<Vec<Value>>,
    service_tier: Option<String>,
    tool_choice: Option<Value>,
    parallel_tool_calls: Option<bool>,
    text: Option<Value>,
    prompt_cache_key: Option<String>,
    include: Option<Vec<String>>,
    client_metadata: Option<Value>,
    previous_response_id: Option<String>,
    stream: Option<bool>,
    #[serde(rename = "turnState")]
    turn_state: Option<String>,
    #[serde(rename = "turnMetadata")]
    turn_metadata: Option<String>,
    #[serde(rename = "betaFeatures")]
    beta_features: Option<String>,
    #[serde(rename = "includeTimingMetrics")]
    include_timing_metrics: Option<String>,
    #[serde(rename = "codexWindowId")]
    codex_window_id: Option<String>,
    #[serde(rename = "parentThreadId")]
    parent_thread_id: Option<String>,
}

pub async fn responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started_at = Instant::now();
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response().into_response();
    }

    let default_model = state.config().model.default_model.clone();
    let body = serde_json::from_slice::<ResponsesBody>(&body)
        .unwrap_or_else(|_| default_body(default_model.clone()));
    let client_stream = body.stream.unwrap_or(true);
    let requested_model = body.model.clone().unwrap_or(default_model);
    let catalog = ModelCatalog::from_config(&state.config().model);
    if !catalog.is_recognized_model_name(&requested_model) {
        return (
            StatusCode::NOT_FOUND,
            Json(openai_error("Model not found", "model_not_found")),
        )
            .into_response();
    }
    let parsed_model = catalog.parse_model_name(&requested_model);
    let mut codex_request = CodexResponsesRequest::new_http_sse(
        parsed_model.model_id.clone(),
        body.instructions.unwrap_or_default(),
        body.input.unwrap_or_default(),
    );
    codex_request.reasoning = body.reasoning;
    codex_request.tools = body.tools;
    codex_request.tool_choice = body.tool_choice;
    codex_request.parallel_tool_calls = body.parallel_tool_calls;
    codex_request.text = body.text;
    codex_request.prompt_cache_key = body.prompt_cache_key;
    codex_request.include = body.include;
    codex_request.client_metadata = body.client_metadata;
    codex_request.previous_response_id = body.previous_response_id;
    codex_request.reasoning = responses_reasoning(
        codex_request.reasoning.take(),
        parsed_model.reasoning_effort.as_deref(),
        state.config().model.default_reasoning_effort.as_deref(),
    );
    codex_request.service_tier = body
        .service_tier
        .or(parsed_model.service_tier)
        .or_else(|| state.config().model.service_tier.clone())
        .map(normalize_service_tier_for_upstream);
    ensure_reasoning_include(&mut codex_request);
    codex_request.turn_state = body
        .turn_state
        .or_else(|| header_string(&headers, "x-codex-turn-state"));
    codex_request.turn_metadata = body
        .turn_metadata
        .or_else(|| header_string(&headers, "x-codex-turn-metadata"));
    codex_request.beta_features = body
        .beta_features
        .or_else(|| header_string(&headers, "x-codex-beta-features"));
    codex_request.include_timing_metrics = body
        .include_timing_metrics
        .or_else(|| header_string(&headers, "x-responsesapi-include-timing-metrics"));
    codex_request.version = header_string(&headers, "version");
    codex_request.codex_window_id = body
        .codex_window_id
        .or_else(|| header_string(&headers, "x-codex-window-id"));
    codex_request.parent_thread_id = body
        .parent_thread_id
        .or_else(|| header_string(&headers, "x-codex-parent-thread-id"));
    let acquired = {
        state
            .account_pool()
            .lock()
            .await
            .acquire_with(AccountAcquireRequest::new(&codex_request.model, Utc::now()))
    };
    let Some(acquired) = acquired else {
        return no_available_accounts_response().into_response();
    };
    let mut account = acquired.account;
    let mut log_context = V1LogContext::new(
        request_id.as_str(),
        &account.id,
        &codex_request.model,
        client_stream,
        started_at,
    );

    if client_stream {
        return responses_stream(state, codex_request, account, log_context).await;
    }

    let mut excluded_account_ids = Vec::new();
    let response = loop {
        let response = send_codex_request_with_refresh_retry(
            &state,
            &codex_request,
            &account,
            request_id.as_str(),
        )
        .await;
        state.account_pool().lock().await.release(&account.id);

        match response {
            Ok(response) => break response,
            Err(error) => {
                if let Some(retry) = classify_upstream_account_retry(&error) {
                    apply_upstream_account_retry(&state, &account, retry).await;
                    excluded_account_ids.push(account.id.clone());
                    log_v1_response(
                        &state,
                        &log_context,
                        retry.status(),
                        EventLevel::Warn,
                        "v1 responses upstream retrying with fallback account",
                        retry.metadata(false),
                    )
                    .await;
                    let fallback = {
                        state.account_pool().lock().await.acquire_with(
                            AccountAcquireRequest::new(&codex_request.model, Utc::now())
                                .with_exclude_account_ids(excluded_account_ids.iter().cloned()),
                        )
                    };
                    if let Some(fallback) = fallback {
                        account = fallback.account;
                        log_context = V1LogContext::new(
                            request_id.as_str(),
                            &account.id,
                            &codex_request.model,
                            client_stream,
                            started_at,
                        );
                        continue;
                    }
                }
                let error_response = codex_client_error_response(error);
                log_v1_response(
                    &state,
                    &log_context,
                    error_response.0,
                    EventLevel::Error,
                    "v1 responses upstream request failed",
                    json!({"stream": false}),
                )
                .await;
                return error_response.into_response();
            }
        }
    };
    if persist_upstream_cookies(&state, &account.id, &response.set_cookie_headers)
        .await
        .is_err()
    {
        log_v1_response(
            &state,
            &log_context,
            StatusCode::INTERNAL_SERVER_ERROR,
            EventLevel::Error,
            "v1 responses cookie persistence failed",
            json!({"stream": false, "cookieStoreError": true}),
        )
        .await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(openai_error(
                "Failed to persist upstream cookies",
                "cookie_store_error",
            )),
        )
            .into_response();
    }
    if let Some(usage) = response.usage {
        if record_usage(&state, &account.id, usage).await.is_err() {
            log_v1_response(
                &state,
                &log_context,
                StatusCode::INTERNAL_SERVER_ERROR,
                EventLevel::Error,
                "v1 responses usage persistence failed",
                json!({"stream": false, "usage": usage, "usageStoreError": true}),
            )
            .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(openai_error(
                    "Failed to record account usage",
                    "usage_store_error",
                )),
            )
                .into_response();
        }
    }

    match completed_response_json(&response.body) {
        Ok(Some(body)) => {
            log_v1_response(
                &state,
                &log_context,
                StatusCode::OK,
                EventLevel::Info,
                "v1 responses completed",
                json!({"stream": false, "usage": response.usage}),
            )
            .await;
            (StatusCode::OK, Json(body)).into_response()
        }
        Ok(None) => {
            log_v1_response(
                &state,
                &log_context,
                StatusCode::BAD_GATEWAY,
                EventLevel::Warn,
                "v1 responses completed event missing",
                json!({"stream": false, "usage": response.usage}),
            )
            .await;
            (
                StatusCode::BAD_GATEWAY,
                Json(openai_error(
                    "Codex response did not include response.completed",
                    "empty_upstream_response",
                )),
            )
                .into_response()
        }
        Err(error) => {
            log_v1_response(
                &state,
                &log_context,
                StatusCode::BAD_GATEWAY,
                EventLevel::Warn,
                "v1 responses invalid SSE response",
                json!({"stream": false, "sseParseError": error.to_string()}),
            )
            .await;
            (
                StatusCode::BAD_GATEWAY,
                Json(openai_error(
                    "Invalid Codex SSE response",
                    "invalid_upstream_sse",
                )),
            )
                .into_response()
        }
    }
}

pub async fn chat_completions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started_at = Instant::now();
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response().into_response();
    }

    let Ok(chat_request) = serde_json::from_slice::<ChatCompletionRequest>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(openai_error(
                "Invalid chat completion request",
                "invalid_request",
            )),
        )
            .into_response();
    };
    let client_stream = chat_request.stream;
    let requested_model = chat_request.model.clone();
    let catalog = ModelCatalog::from_config(&state.config().model);
    if !catalog.is_recognized_model_name(&requested_model) {
        return model_not_found_response().into_response();
    }
    let parsed_model = catalog.parse_model_name(&requested_model);
    let display_model = ModelCatalog::build_display_model_name(&parsed_model);
    let mut codex_request = match translate_chat_to_codex(chat_request) {
        Ok(request) => request,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(openai_error(
                    "Invalid chat completion request",
                    "invalid_request",
                )),
            )
                .into_response();
        }
    };
    codex_request.model = parsed_model.model_id.clone();
    if codex_request.reasoning.is_none() {
        let effort = parsed_model
            .reasoning_effort
            .clone()
            .or_else(|| state.config().model.default_reasoning_effort.clone());
        if let Some(effort) = effort {
            codex_request.reasoning = Some(json!({"effort": effort, "summary": "auto"}));
        }
    }
    if codex_request.service_tier.is_none() {
        codex_request.service_tier = parsed_model
            .service_tier
            .clone()
            .or_else(|| state.config().model.service_tier.clone());
    }
    codex_request.service_tier = codex_request
        .service_tier
        .map(normalize_service_tier_for_upstream);
    let include_reasoning = codex_request.reasoning.is_some();

    let acquired = {
        state
            .account_pool()
            .lock()
            .await
            .acquire_with(AccountAcquireRequest::new(&codex_request.model, Utc::now()))
    };
    let Some(acquired) = acquired else {
        return no_available_accounts_response().into_response();
    };
    let account = acquired.account;
    let log_context = V1LogContext::new(
        request_id.as_str(),
        &account.id,
        &codex_request.model,
        client_stream,
        started_at,
    );

    let response = send_codex_request_with_refresh_retry(
        &state,
        &codex_request,
        &account,
        request_id.as_str(),
    )
    .await;
    state.account_pool().lock().await.release(&account.id);

    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let error_response = codex_client_error_response(error);
            log_v1_response(
                &state,
                &log_context,
                error_response.0,
                EventLevel::Error,
                "v1 chat completions upstream request failed",
                json!({"stream": false}),
            )
            .await;
            return error_response.into_response();
        }
    };
    if persist_upstream_cookies(&state, &account.id, &response.set_cookie_headers)
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(openai_error(
                "Failed to persist upstream cookies",
                "cookie_store_error",
            )),
        )
            .into_response();
    }
    if let Some(usage) = response.usage {
        if record_usage(&state, &account.id, usage).await.is_err() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(openai_error(
                    "Failed to record account usage",
                    "usage_store_error",
                )),
            )
                .into_response();
        }
    }

    if client_stream {
        match chat_completion_stream_from_codex_sse(
            &response.body,
            &display_model,
            include_reasoning,
        ) {
            Ok(Some(body)) => Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/event-stream")
                .header(CACHE_CONTROL, "no-cache")
                .body(Body::from(body))
                .unwrap_or_else(|_| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(openai_error(
                            "Failed to build stream response",
                            "stream_response_error",
                        )),
                    )
                        .into_response()
                }),
            Ok(None) => (
                StatusCode::BAD_GATEWAY,
                Json(openai_error(
                    "Codex response did not include response.completed",
                    "empty_upstream_response",
                )),
            )
                .into_response(),
            Err(_) => (
                StatusCode::BAD_GATEWAY,
                Json(openai_error(
                    "Invalid Codex SSE response",
                    "invalid_upstream_sse",
                )),
            )
                .into_response(),
        }
    } else {
        match chat_completion_from_codex_sse(&response.body, &display_model, include_reasoning) {
            Ok(Some(body)) => (StatusCode::OK, Json(body)).into_response(),
            Ok(None) => (
                StatusCode::BAD_GATEWAY,
                Json(openai_error(
                    "Codex response did not include response.completed",
                    "empty_upstream_response",
                )),
            )
                .into_response(),
            Err(_) => (
                StatusCode::BAD_GATEWAY,
                Json(openai_error(
                    "Invalid Codex SSE response",
                    "invalid_upstream_sse",
                )),
            )
                .into_response(),
        }
    }
}

fn no_available_accounts_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(openai_error(
            "No available Codex accounts",
            "no_available_accounts",
        )),
    )
}

fn default_body(default_model: String) -> ResponsesBody {
    ResponsesBody {
        model: Some(default_model),
        input: Some(Vec::new()),
        instructions: Some(String::new()),
        reasoning: None,
        tools: None,
        service_tier: None,
        tool_choice: None,
        parallel_tool_calls: None,
        text: None,
        prompt_cache_key: None,
        include: None,
        client_metadata: None,
        previous_response_id: None,
        stream: None,
        turn_state: None,
        turn_metadata: None,
        beta_features: None,
        include_timing_metrics: None,
        codex_window_id: None,
        parent_thread_id: None,
    }
}

fn responses_reasoning(
    client_reasoning: Option<Value>,
    suffix_effort: Option<&str>,
    default_effort: Option<&str>,
) -> Option<Value> {
    let effort = client_reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
        .or(suffix_effort)
        .or(default_effort);
    let summary = client_reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.get("summary"))
        .and_then(Value::as_str)
        .unwrap_or("auto");
    if effort.is_none() && client_reasoning.is_none() {
        return None;
    }
    let mut reasoning = json!({"summary": summary});
    if let Some(effort) = effort {
        reasoning["effort"] = Value::String(effort.to_string());
    }
    Some(reasoning)
}

fn normalize_service_tier_for_upstream(service_tier: String) -> String {
    if service_tier == "fast" {
        "priority".to_string()
    } else {
        service_tier
    }
}

fn ensure_reasoning_include(request: &mut CodexResponsesRequest) {
    if request.reasoning.is_none() {
        return;
    }
    let include = request.include.get_or_insert_with(Vec::new);
    if include
        .iter()
        .any(|item| item == "reasoning.encrypted_content")
    {
        return;
    }
    include.push("reasoning.encrypted_content".to_string());
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

async fn send_codex_request_with_refresh_retry(
    state: &AppState,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<crate::codex::client::CodexBackendResponse, CodexClientError> {
    match send_codex_request(state, request, account, request_id).await {
        Err(CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
        }) if status == StatusCode::UNAUTHORIZED => {
            let Some(refreshed) =
                refresh_account_after_unauthorized(state, request, account, request_id).await
            else {
                return Err(CodexClientError::Upstream {
                    status,
                    body,
                    retry_after_seconds,
                });
            };
            send_codex_request(state, request, &refreshed, request_id).await
        }
        result => result,
    }
}

async fn send_codex_request(
    state: &AppState,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<crate::codex::client::CodexBackendResponse, CodexClientError> {
    let request_domain = request_domain(&state.config().api.base_url);
    let cookie_header = match (state.cookie_repository(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };
    let client = CodexBackendClient::new(
        build_reqwest_client(state.config().tls.force_http11)?,
        state.config().api.base_url.clone(),
        Fingerprint::default_codex_desktop(),
    );
    client
        .create_response(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: request.codex_window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
            },
        )
        .await
}

async fn send_codex_stream_request_with_refresh_retry(
    state: &AppState,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendStream, CodexClientError> {
    match send_codex_stream_request(state, request, account, request_id).await {
        Err(CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
        }) if status == StatusCode::UNAUTHORIZED => {
            let Some(refreshed) =
                refresh_account_after_unauthorized(state, request, account, request_id).await
            else {
                return Err(CodexClientError::Upstream {
                    status,
                    body,
                    retry_after_seconds,
                });
            };
            send_codex_stream_request(state, request, &refreshed, request_id).await
        }
        result => result,
    }
}

async fn send_codex_stream_request(
    state: &AppState,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendStream, CodexClientError> {
    let request_domain = request_domain(&state.config().api.base_url);
    let cookie_header = match (state.cookie_repository(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };
    let client = CodexBackendClient::new(
        build_reqwest_client(state.config().tls.force_http11)?,
        state.config().api.base_url.clone(),
        Fingerprint::default_codex_desktop(),
    );
    client
        .stream_response(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: request.codex_window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
            },
        )
        .await
}

#[derive(Debug, Clone, Copy)]
enum UpstreamAccountRetry {
    RateLimited { retry_after_seconds: u64 },
    QuotaExhausted,
    CloudflareChallenge { cooldown_seconds: u64 },
    Banned,
}

impl UpstreamAccountRetry {
    fn status(self) -> StatusCode {
        match self {
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::QuotaExhausted => StatusCode::PAYMENT_REQUIRED,
            Self::CloudflareChallenge { .. } => StatusCode::FORBIDDEN,
            Self::Banned => StatusCode::FORBIDDEN,
        }
    }

    fn metadata(self, stream: bool) -> Value {
        match self {
            Self::RateLimited {
                retry_after_seconds,
            } => json!({
                "stream": stream,
                "retry": true,
                "reason": "rateLimited",
                "retryAfterSeconds": retry_after_seconds,
            }),
            Self::QuotaExhausted => json!({
                "stream": stream,
                "retry": true,
                "reason": "quotaExhausted",
            }),
            Self::CloudflareChallenge { cooldown_seconds } => json!({
                "stream": stream,
                "retry": true,
                "reason": "cloudflareChallenge",
                "cooldownSeconds": cooldown_seconds,
            }),
            Self::Banned => json!({
                "stream": stream,
                "retry": true,
                "reason": "banned",
            }),
        }
    }
}

fn classify_upstream_account_retry(error: &CodexClientError) -> Option<UpstreamAccountRetry> {
    match error {
        CodexClientError::Upstream {
            status,
            retry_after_seconds,
            ..
        } if *status == StatusCode::TOO_MANY_REQUESTS => Some(UpstreamAccountRetry::RateLimited {
            retry_after_seconds: retry_after_seconds
                .unwrap_or(DEFAULT_RATE_LIMIT_BACKOFF_SECONDS)
                .min(MAX_RATE_LIMIT_BACKOFF_SECONDS),
        }),
        CodexClientError::Upstream { status, .. } if *status == StatusCode::PAYMENT_REQUIRED => {
            Some(UpstreamAccountRetry::QuotaExhausted)
        }
        CodexClientError::Upstream { status, body, .. } if *status == StatusCode::FORBIDDEN => {
            if is_cloudflare_challenge(body) {
                Some(UpstreamAccountRetry::CloudflareChallenge {
                    cooldown_seconds: CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS,
                })
            } else {
                Some(UpstreamAccountRetry::Banned)
            }
        }
        _ => None,
    }
}

fn is_cloudflare_challenge(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("cf-mitigated")
        || lower.contains("cf-chl-bypass")
        || lower.contains("_cf_chl")
        || lower.contains("cf_chl")
        || lower.contains("attention required")
        || lower.contains("just a moment")
}

async fn apply_upstream_account_retry(
    state: &AppState,
    account: &Account,
    retry: UpstreamAccountRetry,
) {
    match retry {
        UpstreamAccountRetry::RateLimited {
            retry_after_seconds,
        } => {
            let cooldown_until = Utc::now() + Duration::seconds(retry_after_seconds as i64);
            state
                .account_pool()
                .lock()
                .await
                .mark_quota_limited_until(&account.id, cooldown_until);
            if record_request_attempt(state, &account.id).await.is_err() {
                tracing::warn!(
                    account_id = %account.id,
                    "failed to record rate-limited account attempt"
                );
            }
        }
        UpstreamAccountRetry::QuotaExhausted => {
            set_account_status(state, account, AccountStatus::QuotaExhausted).await;
        }
        UpstreamAccountRetry::CloudflareChallenge { cooldown_seconds } => {
            let cooldown_until = Utc::now() + Duration::seconds(cooldown_seconds as i64);
            state
                .account_pool()
                .lock()
                .await
                .set_cloudflare_cooldown_until(&account.id, cooldown_until);
        }
        UpstreamAccountRetry::Banned => {
            set_account_status(state, account, AccountStatus::Banned).await;
        }
    }
}

async fn set_account_status(state: &AppState, account: &Account, status: AccountStatus) {
    if let Some(repo) = state.account_repository() {
        if repo.set_status(&account.id, status).await.is_err() {
            tracing::warn!(
                account_id = %account.id,
                "failed to persist upstream account status"
            );
        }
    }
    state
        .account_pool()
        .lock()
        .await
        .set_status(&account.id, status);
}

async fn responses_stream(
    state: AppState,
    request: CodexResponsesRequest,
    account: Account,
    log_context: V1LogContext,
) -> Response {
    if transport_for_request(&request) == CodexTransport::WebSocketRequired {
        return responses_websocket_stream(state, request, account, log_context).await;
    }

    let stream_response = send_codex_stream_request_with_refresh_retry(
        &state,
        &request,
        &account,
        log_context.request_id.as_str(),
    )
    .await;
    let stream_response = match stream_response {
        Ok(response) => response,
        Err(error) => {
            state.account_pool().lock().await.release(&account.id);
            let error_response = codex_client_error_response(error);
            log_v1_response(
                &state,
                &log_context,
                error_response.0,
                EventLevel::Error,
                "v1 responses stream upstream request failed",
                json!({"stream": true}),
            )
            .await;
            return error_response.into_response();
        }
    };

    if persist_upstream_cookies(&state, &account.id, &stream_response.set_cookie_headers)
        .await
        .is_err()
    {
        state.account_pool().lock().await.release(&account.id);
        log_v1_response(
            &state,
            &log_context,
            StatusCode::INTERNAL_SERVER_ERROR,
            EventLevel::Error,
            "v1 responses stream cookie persistence failed",
            json!({"stream": true, "cookieStoreError": true}),
        )
        .await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(openai_error(
                "Failed to persist upstream cookies",
                "cookie_store_error",
            )),
        )
            .into_response();
    }

    let upstream = Box::pin(stream_response.response.bytes_stream());
    let audit = StreamAudit::new(state, log_context, account.id);
    let body_stream = stream::unfold(Some((upstream, Vec::new(), audit)), |state| async move {
        let (mut upstream, mut collected, mut audit) = state?;
        match upstream.next().await {
            Some(Ok(chunk)) => {
                collected.extend_from_slice(&chunk);
                Some((Ok(chunk), Some((upstream, collected, audit))))
            }
            Some(Err(error)) => {
                audit.log_transport_error(&error).await;
                Some((Err(error), None))
            }
            None => {
                audit.complete(&collected).await;
                None
            }
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(openai_error(
                    "Failed to build stream response",
                    "stream_response_error",
                )),
            )
                .into_response()
        })
}

async fn responses_websocket_stream(
    state: AppState,
    request: CodexResponsesRequest,
    account: Account,
    log_context: V1LogContext,
) -> Response {
    let response = send_codex_request_with_refresh_retry(
        &state,
        &request,
        &account,
        log_context.request_id.as_str(),
    )
    .await;
    state.account_pool().lock().await.release(&account.id);

    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let error_response = codex_client_error_response(error);
            log_v1_response(
                &state,
                &log_context,
                error_response.0,
                EventLevel::Error,
                "v1 responses websocket stream upstream request failed",
                json!({"stream": true, "transport": "websocket"}),
            )
            .await;
            return error_response.into_response();
        }
    };

    if persist_upstream_cookies(&state, &account.id, &response.set_cookie_headers)
        .await
        .is_err()
    {
        log_v1_response(
            &state,
            &log_context,
            StatusCode::INTERNAL_SERVER_ERROR,
            EventLevel::Error,
            "v1 responses websocket stream cookie persistence failed",
            json!({"stream": true, "transport": "websocket", "cookieStoreError": true}),
        )
        .await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(openai_error(
                "Failed to persist upstream cookies",
                "cookie_store_error",
            )),
        )
            .into_response();
    }

    let mut level = EventLevel::Info;
    let mut message = "v1 responses websocket stream completed";
    let mut metadata = json!({
        "stream": true,
        "transport": "websocket",
        "usage": response.usage,
    });
    if let Some(usage) = response.usage {
        if record_usage(&state, &account.id, usage).await.is_err() {
            level = EventLevel::Warn;
            message = "v1 responses websocket stream completed with usage store error";
            metadata = json!({
                "stream": true,
                "transport": "websocket",
                "usage": usage,
                "usageStoreError": true,
            });
        }
    }
    log_v1_response(
        &state,
        &log_context,
        StatusCode::OK,
        level,
        message,
        metadata,
    )
    .await;

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .body(Body::from(response.body))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(openai_error(
                    "Failed to build stream response",
                    "stream_response_error",
                )),
            )
                .into_response()
        })
}

async fn refresh_account_after_unauthorized(
    state: &AppState,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Option<Account> {
    if !state.config().auth.refresh_enabled {
        return None;
    }
    let refresh_token = account.refresh_token.as_deref()?;
    let refresher = state.token_refresher()?;
    match refresher.refresh(refresh_token).await {
        Ok(tokens) => persist_refreshed_account(state, account, tokens).await,
        Err(failure) => {
            mark_refresh_failure(state, account, failure, request_id, &request.model).await;
            None
        }
    }
}

async fn persist_refreshed_account(
    state: &AppState,
    account: &Account,
    tokens: crate::auth::token::TokenPair,
) -> Option<Account> {
    let repo = state.account_repository()?;
    let access_token = tokens.access_token;
    let refresh_token = tokens.refresh_token;
    repo.update_tokens(
        &account.id,
        TokenUpdate {
            access_token: SecretString::new(access_token.clone().into()),
            refresh_token: refresh_token
                .clone()
                .map(|token| SecretString::new(token.into())),
            access_token_expires_at: None,
        },
    )
    .await
    .ok()?;

    let mut refreshed = account.clone();
    refreshed.access_token = access_token;
    if let Some(refresh_token) = refresh_token {
        refreshed.refresh_token = Some(refresh_token);
    }
    refreshed.status = AccountStatus::Active;
    state.account_pool().lock().await.insert(refreshed.clone());
    Some(refreshed)
}

async fn mark_refresh_failure(
    state: &AppState,
    account: &Account,
    failure: RefreshFailure,
    request_id: &str,
    model: &str,
) {
    let status = status_for_refresh_failure(failure);
    if let Some(status) = status {
        if let Some(repo) = state.account_repository() {
            let _ = repo.set_status(&account.id, status).await;
        }
        let mut updated = account.clone();
        updated.status = status;
        state.account_pool().lock().await.insert(updated);
    }
    log_account_refresh_failure(state, account, failure, status, request_id, model).await;
}

fn status_for_refresh_failure(failure: RefreshFailure) -> Option<AccountStatus> {
    match failure {
        RefreshFailure::InvalidGrant => Some(AccountStatus::Expired),
        RefreshFailure::QuotaExhausted => Some(AccountStatus::QuotaExhausted),
        RefreshFailure::Banned => Some(AccountStatus::Banned),
        RefreshFailure::Disabled => Some(AccountStatus::Disabled),
        RefreshFailure::Transport => None,
    }
}

async fn log_account_refresh_failure(
    state: &AppState,
    account: &Account,
    failure: RefreshFailure,
    status: Option<AccountStatus>,
    request_id: &str,
    model: &str,
) {
    let Some(repo) = state.event_logs() else {
        return;
    };
    let mut event = EventLog::new(
        "account.refresh",
        EventLevel::Warn,
        "account refresh failed after upstream 401",
    );
    event.request_id = Some(request_id.to_string());
    event.account_id = Some(account.id.clone());
    event.route = Some("/v1/responses".to_string());
    event.model = Some(model.to_string());
    event.status_code = Some(i64::from(StatusCode::UNAUTHORIZED.as_u16()));
    event.metadata = json!({
        "trigger": "upstream_401",
        "failure": refresh_failure_value(failure),
        "accountStatus": status.map(account_status_value),
    });
    if let Err(error) = repo.insert(event).await {
        tracing::warn!(?error, "failed to insert account refresh event log");
    }
}

fn refresh_failure_value(failure: RefreshFailure) -> &'static str {
    match failure {
        RefreshFailure::InvalidGrant => "invalidGrant",
        RefreshFailure::QuotaExhausted => "quotaExhausted",
        RefreshFailure::Banned => "banned",
        RefreshFailure::Disabled => "disabled",
        RefreshFailure::Transport => "transport",
    }
}

fn account_status_value(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Active => "active",
        AccountStatus::Expired => "expired",
        AccountStatus::QuotaExhausted => "quotaExhausted",
        AccountStatus::Refreshing => "refreshing",
        AccountStatus::Disabled => "disabled",
        AccountStatus::Banned => "banned",
    }
}

async fn persist_upstream_cookies(
    state: &AppState,
    account_id: &str,
    set_cookie_headers: &[String],
) -> Result<(), ()> {
    let Some(cookie_repo) = state.cookie_repository() else {
        return Ok(());
    };
    for cookie in set_cookie_headers {
        cookie_repo
            .capture_set_cookie(account_id, cookie)
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

async fn record_usage(state: &AppState, account_id: &str, usage: TokenUsage) -> Result<(), ()> {
    let Some(repo) = state.account_repository() else {
        return Ok(());
    };
    repo.record_usage(
        account_id,
        UsageDelta {
            input_tokens: u64_to_i64_saturating(usage.input_tokens),
            output_tokens: u64_to_i64_saturating(usage.output_tokens),
            cached_tokens: u64_to_i64_saturating(usage.cached_tokens),
        },
    )
    .await
    .map_err(|_| ())
}

async fn record_request_attempt(state: &AppState, account_id: &str) -> Result<(), ()> {
    let Some(repo) = state.account_repository() else {
        return Ok(());
    };
    repo.record_usage(
        account_id,
        UsageDelta {
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
        },
    )
    .await
    .map_err(|_| ())
}

#[derive(Clone)]
struct V1LogContext {
    request_id: String,
    account_id: String,
    model: String,
    stream: bool,
    started_at: Instant,
}

impl V1LogContext {
    fn new(
        request_id: &str,
        account_id: &str,
        model: &str,
        stream: bool,
        started_at: Instant,
    ) -> Self {
        Self {
            request_id: request_id.to_string(),
            account_id: account_id.to_string(),
            model: model.to_string(),
            stream,
            started_at,
        }
    }

    fn latency_ms(&self) -> i64 {
        self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
    }
}

struct StreamAudit {
    state: AppState,
    context: V1LogContext,
    account_slot: AccountSlotGuard,
}

impl StreamAudit {
    fn new(state: AppState, context: V1LogContext, account_id: String) -> Self {
        let account_slot = AccountSlotGuard::new(state.account_pool(), account_id);
        Self {
            state,
            context,
            account_slot,
        }
    }

    async fn complete(&mut self, body: &[u8]) {
        let body = String::from_utf8_lossy(body);
        let mut level = EventLevel::Info;
        let mut message = "v1 responses stream completed";
        let mut metadata = match extract_sse_usage(&body) {
            Ok(usage) => {
                if let Some(usage) = usage {
                    if record_usage(&self.state, &self.context.account_id, usage)
                        .await
                        .is_err()
                    {
                        level = EventLevel::Warn;
                        message = "v1 responses stream completed with usage store error";
                        json!({"stream": true, "usage": usage, "usageStoreError": true})
                    } else {
                        json!({"stream": true, "usage": usage})
                    }
                } else {
                    json!({"stream": true, "usage": null})
                }
            }
            Err(error) => {
                level = EventLevel::Warn;
                message = "v1 responses stream completed with invalid SSE usage";
                json!({"stream": true, "sseParseError": error.to_string()})
            }
        };
        ensure_stream_metadata(&mut metadata, true);
        log_v1_response(
            &self.state,
            &self.context,
            StatusCode::OK,
            level,
            message,
            metadata,
        )
        .await;
        self.account_slot.release().await;
    }

    async fn log_transport_error(&mut self, error: &reqwest::Error) {
        log_v1_response(
            &self.state,
            &self.context,
            StatusCode::BAD_GATEWAY,
            EventLevel::Error,
            "v1 responses stream transport failed",
            json!({"stream": true, "transportError": error.to_string()}),
        )
        .await;
        self.account_slot.release().await;
    }
}

struct AccountSlotGuard {
    pool: Arc<Mutex<AccountPool>>,
    account_id: String,
    released: bool,
}

impl AccountSlotGuard {
    fn new(pool: Arc<Mutex<AccountPool>>, account_id: String) -> Self {
        Self {
            pool,
            account_id,
            released: false,
        }
    }

    async fn release(&mut self) {
        if self.released {
            return;
        }
        self.pool.lock().await.release(&self.account_id);
        self.released = true;
    }
}

impl Drop for AccountSlotGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let pool = self.pool.clone();
        let account_id = self.account_id.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            pool.lock().await.release(&account_id);
        });
    }
}

async fn log_v1_response(
    state: &AppState,
    context: &V1LogContext,
    status: StatusCode,
    level: EventLevel,
    message: &str,
    mut metadata: Value,
) {
    let Some(repo) = state.event_logs() else {
        return;
    };
    ensure_stream_metadata(&mut metadata, context.stream);
    let mut event = EventLog::new("v1.response", level, message);
    event.request_id = Some(context.request_id.clone());
    event.account_id = Some(context.account_id.clone());
    event.route = Some("/v1/responses".to_string());
    event.model = Some(context.model.clone());
    event.status_code = Some(i64::from(status.as_u16()));
    event.latency_ms = Some(context.latency_ms());
    event.metadata = metadata;
    if let Err(error) = repo.insert(event).await {
        tracing::warn!(?error, "failed to insert v1 response event log");
    }
}

fn ensure_stream_metadata(metadata: &mut Value, stream_value: bool) {
    let Some(object) = metadata.as_object_mut() else {
        *metadata = json!({"stream": stream_value});
        return;
    };
    object
        .entry("stream".to_string())
        .or_insert_with(|| json!(stream_value));
}

fn completed_response_json(body: &str) -> Result<Option<Value>, crate::codex::sse::SseError> {
    let events = parse_sse_events(body)?;
    let mut output_text = String::new();
    let mut output_items = Vec::new();
    let mut completed_response = None;
    for event in events {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match event.event.as_deref() {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    output_text.push_str(delta);
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = value.get("item") {
                    output_items.push(item.clone());
                }
            }
            Some("response.completed") => {
                if let Some(response) = value.get("response") {
                    completed_response = Some(response.clone());
                }
            }
            _ => {}
        }
    }
    let Some(mut response) = completed_response else {
        return Ok(None);
    };
    ensure_completed_response_output(&mut response, &output_items, &output_text);
    sync_output_text_from_output(&mut response);
    Ok(Some(response))
}

fn ensure_completed_response_output(
    response: &mut Value,
    output_items: &[Value],
    output_text: &str,
) {
    let output_is_empty = response
        .get("output")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);
    if !output_is_empty {
        return;
    }

    if !output_items.is_empty() {
        response["output"] = Value::Array(output_items.to_vec());
        return;
    }
    if output_text.is_empty() {
        return;
    }

    // 原版 passthrough 会用 done item 或文本 delta 回填 completed.output，避免非流式客户端拿到空正文。
    response["output"] = json!([{
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": output_text,
            "annotations": []
        }]
    }]);
}

fn sync_output_text_from_output(response: &mut Value) {
    let output_text_is_empty = response
        .get("output_text")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty);
    if !output_text_is_empty {
        return;
    }
    let Some(items) = response.get("output").and_then(Value::as_array) else {
        return;
    };
    let texts = items
        .iter()
        .filter_map(output_text_from_item)
        .collect::<Vec<_>>();
    if texts.is_empty() {
        return;
    }
    response["output_text"] = Value::String(texts.join("\n\n"));
}

fn output_text_from_item(item: &Value) -> Option<String> {
    let content = item.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter_map(|part| {
            let part_type = part.get("type")?.as_str()?;
            if part_type != "output_text" && part_type != "text" {
                return None;
            }
            part.get("text")?.as_str()
        })
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

fn codex_client_error_response(error: CodexClientError) -> (StatusCode, Json<Value>) {
    match error {
        CodexClientError::UnsupportedTransport(_) => (
            StatusCode::BAD_REQUEST,
            Json(openai_error(
                "previous_response_id requires Codex WebSocket transport",
                "websocket_required",
            )),
        ),
        CodexClientError::Upstream { status, body, .. } => (
            status,
            Json(openai_error(
                &format!(
                    "Codex upstream error: {}",
                    body.chars().take(300).collect::<String>()
                ),
                "upstream_error",
            )),
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            Json(openai_error(
                "Codex upstream request failed",
                "upstream_error",
            )),
        ),
    }
}

fn request_domain(base_url: &str) -> Option<String> {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(ToString::to_string))
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

pub async fn models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
    let data = catalog
        .models()
        .iter()
        .map(|model| openai_model_json(&model.id))
        .collect::<Vec<_>>();
    (
        StatusCode::OK,
        Json(json!({
            "object": "list",
            "data": data
        })),
    )
}

pub async fn model_catalog(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
    (StatusCode::OK, Json(json!(catalog.models())))
}

pub async fn model_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
    if catalog.model_info(&model_id).is_none() {
        return model_not_found_response();
    }
    (StatusCode::OK, Json(openai_model_json(&model_id)))
}

pub async fn model_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
    let Some(info) = catalog.model_info(&model_id) else {
        return model_not_found_response();
    };
    (StatusCode::OK, Json(json!(info)))
}

pub async fn debug_models(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let catalog = ModelCatalog::from_config(&state.config().model);
    (StatusCode::OK, Json(json!(catalog.debug())))
}

async fn authorize_client_api_key(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(api_key) = client_api_key(headers) else {
        return false;
    };
    let Some(repo) = state.client_api_key_repository() else {
        return false;
    };
    let Some(hasher) = state.api_key_hasher().cloned() else {
        return false;
    };
    repo.verify_and_touch(api_key.as_str(), &hasher)
        .await
        .unwrap_or(false)
}

fn missing_client_api_key_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(openai_error("Missing client API key", "invalid_api_key")),
    )
}

fn model_not_found_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(openai_error("Model not found", "model_not_found")),
    )
}

fn openai_model_json(id: &str) -> Value {
    json!({
        "id": id,
        "object": "model",
        "created": MODEL_CREATED_TIMESTAMP,
        "owned_by": "openai"
    })
}
