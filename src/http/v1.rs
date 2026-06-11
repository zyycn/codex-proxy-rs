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
use chrono::Utc;
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
    },
    fingerprint::model::Fingerprint,
    http::{auth::client_api_key, middleware::RequestId},
    logs::event::{EventLevel, EventLog},
    models::catalog::ModelCatalog,
    state::AppState,
    translation::codex_to_openai::openai_error,
};

const MODEL_CREATED_TIMESTAMP: i64 = 1_700_000_000;

#[derive(Deserialize)]
struct ResponsesBody {
    model: Option<String>,
    input: Option<Vec<Value>>,
    instructions: Option<String>,
    reasoning: Option<Value>,
    tools: Option<Vec<Value>>,
    previous_response_id: Option<String>,
    stream: Option<bool>,
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
    let client_stream = body.stream.unwrap_or(false);
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
    let codex_request = CodexResponsesRequest {
        model: parsed_model.model_id.clone(),
        instructions: body.instructions.unwrap_or_default(),
        input: body.input.unwrap_or_default(),
        stream: true,
        store: false,
        reasoning: body.reasoning,
        tools: body.tools,
        previous_response_id: body.previous_response_id,
        use_websocket: false,
    };
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

    if client_stream {
        return responses_stream(state, codex_request, account, log_context).await;
    }

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
                "v1 responses upstream request failed",
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
        previous_response_id: None,
        stream: None,
    }
}

async fn send_codex_request_with_refresh_retry(
    state: &AppState,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<crate::codex::client::CodexBackendResponse, CodexClientError> {
    match send_codex_request(state, request, account, request_id).await {
        Err(CodexClientError::Upstream { status, body }) if status == StatusCode::UNAUTHORIZED => {
            let Some(refreshed) = refresh_account_after_unauthorized(state, account).await else {
                return Err(CodexClientError::Upstream { status, body });
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
                turn_state: None,
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
        Err(CodexClientError::Upstream { status, body }) if status == StatusCode::UNAUTHORIZED => {
            let Some(refreshed) = refresh_account_after_unauthorized(state, account).await else {
                return Err(CodexClientError::Upstream { status, body });
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
                turn_state: None,
                cookie_header: cookie_header.as_deref(),
            },
        )
        .await
}

async fn responses_stream(
    state: AppState,
    request: CodexResponsesRequest,
    account: Account,
    log_context: V1LogContext,
) -> Response {
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

async fn refresh_account_after_unauthorized(
    state: &AppState,
    account: &Account,
) -> Option<Account> {
    if !state.config().auth.refresh_enabled {
        return None;
    }
    let refresh_token = account.refresh_token.as_deref()?;
    let refresher = state.token_refresher()?;
    match refresher.refresh(refresh_token).await {
        Ok(tokens) => persist_refreshed_account(state, account, tokens).await,
        Err(failure) => {
            mark_refresh_failure(state, account, failure).await;
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

async fn mark_refresh_failure(state: &AppState, account: &Account, failure: RefreshFailure) {
    let Some(status) = status_for_refresh_failure(failure) else {
        return;
    };
    if let Some(repo) = state.account_repository() {
        let _ = repo.set_status(&account.id, status).await;
    }
    let mut updated = account.clone();
    updated.status = status;
    state.account_pool().lock().await.insert(updated);
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
    for event in events {
        if event.event.as_deref() != Some("response.completed") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        if let Some(response) = value.get("response") {
            return Ok(Some(response.clone()));
        }
    }
    Ok(None)
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
        CodexClientError::Upstream { status, body } => (
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
