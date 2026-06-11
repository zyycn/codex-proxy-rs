use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use chrono::Utc;
use reqwest::Url;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    accounts::{pool::AccountAcquireRequest, repository::UsageDelta},
    codex::{
        client::{build_reqwest_client, CodexBackendClient, CodexClientError, CodexRequestContext},
        sse::parse_sse_events,
        types::CodexResponsesRequest,
    },
    fingerprint::model::Fingerprint,
    http::{auth::client_api_key, middleware::RequestId},
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
}

pub async fn responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response();
    }

    let default_model = state.config().model.default_model.clone();
    let body = serde_json::from_slice::<ResponsesBody>(&body)
        .unwrap_or_else(|_| default_body(default_model.clone()));
    let requested_model = body.model.clone().unwrap_or(default_model);
    let catalog = ModelCatalog::from_config(&state.config().model);
    if !catalog.is_recognized_model_name(&requested_model) {
        return (
            StatusCode::NOT_FOUND,
            Json(openai_error("Model not found", "model_not_found")),
        );
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
        return no_available_accounts_response();
    };
    let account = acquired.account;
    let response = send_codex_request(&state, &codex_request, &account, request_id.as_str()).await;
    state.account_pool().lock().await.release(&account.id);

    let response = match response {
        Ok(response) => response,
        Err(error) => return codex_client_error_response(error),
    };
    if let Some(cookie_repo) = state.cookie_repository() {
        for cookie in &response.set_cookie_headers {
            if cookie_repo
                .capture_set_cookie(&account.id, cookie)
                .await
                .is_err()
            {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(openai_error(
                        "Failed to persist upstream cookies",
                        "cookie_store_error",
                    )),
                );
            }
        }
    }
    if let (Some(repo), Some(usage)) = (state.account_repository(), response.usage) {
        if repo
            .record_usage(
                &account.id,
                UsageDelta {
                    input_tokens: u64_to_i64_saturating(usage.input_tokens),
                    output_tokens: u64_to_i64_saturating(usage.output_tokens),
                    cached_tokens: u64_to_i64_saturating(usage.cached_tokens),
                },
            )
            .await
            .is_err()
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(openai_error(
                    "Failed to record account usage",
                    "usage_store_error",
                )),
            );
        }
    }

    match completed_response_json(&response.body) {
        Ok(Some(body)) => (StatusCode::OK, Json(body)),
        Ok(None) => (
            StatusCode::BAD_GATEWAY,
            Json(openai_error(
                "Codex response did not include response.completed",
                "empty_upstream_response",
            )),
        ),
        Err(_) => (
            StatusCode::BAD_GATEWAY,
            Json(openai_error(
                "Invalid Codex SSE response",
                "invalid_upstream_sse",
            )),
        ),
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
    }
}

async fn send_codex_request(
    state: &AppState,
    request: &CodexResponsesRequest,
    account: &crate::accounts::model::Account,
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
