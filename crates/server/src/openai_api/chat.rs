//! OpenAI 聊天处理器。

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use codex_proxy_core::protocol::openai::chat::{translate_chat_to_codex, ChatCompletionRequest};
use codex_proxy_runtime::{services::ChatDispatchError, state::AppState};
use serde_json::{json, Value};

use crate::middleware::request_id::RequestId;

use super::{auth::authorize_client_api_key, models::model_catalog_for_state};

/// `POST /v1/chat/completions`
pub async fn chat_completions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response().into_response();
    }

    let Ok(chat_request) = serde_json::from_slice::<ChatCompletionRequest>(&body) else {
        return openai_error_response(
            StatusCode::BAD_REQUEST,
            "Invalid chat completion request",
            "invalid_request_error",
            "invalid_request",
        )
        .into_response();
    };
    let model = chat_request.model.clone();
    let catalog = model_catalog_for_state(&state).await;
    if !catalog.is_recognized_model_name(&model) {
        return model_not_found_response().into_response();
    }
    let codex_request = match translate_chat_to_codex(chat_request) {
        Ok(request) => request,
        Err(_) => {
            return openai_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid chat completion request",
                "invalid_request_error",
                "invalid_request",
            )
            .into_response();
        }
    };

    match state
        .services
        .chat
        .complete(request_id.as_str(), codex_request, &model)
        .await
    {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(ChatDispatchError::NoActiveAccount | ChatDispatchError::AccountStore) => {
            openai_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "No active upstream account is available",
                "server_error",
                "upstream_unavailable",
            )
            .into_response()
        }
        Err(ChatDispatchError::Upstream(_)) => openai_error_response(
            StatusCode::BAD_GATEWAY,
            "Upstream Codex request failed",
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ChatDispatchError::QuotaExhausted {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::PAYMENT_REQUIRED,
            &format!(
                "All accounts exhausted ({count} quota-exhausted). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ChatDispatchError::RateLimited {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::TOO_MANY_REQUESTS,
            &format!(
                "All accounts exhausted ({count} rate-limited). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ChatDispatchError::Expired {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::UNAUTHORIZED,
            &format!(
                "All accounts exhausted ({count} expired). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ChatDispatchError::CloudflareChallenge {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::BAD_GATEWAY,
            &format!(
                "All accounts exhausted ({count} cloudflare-challenge). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ChatDispatchError::CloudflarePathBlocked {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::BAD_GATEWAY,
            &format!(
                "All accounts exhausted ({count} cloudflare-path-block). Codex upstream error: {upstream_error}"
            ),
            "server_error",
            "upstream_error",
        )
        .into_response(),
        Err(ChatDispatchError::ModelUnsupported {
            count,
            upstream_error,
        }) => openai_error_response(
            StatusCode::BAD_REQUEST,
            &format!(
                "All accounts exhausted ({count} model-unsupported). Codex upstream error: {upstream_error}"
            ),
            "invalid_request_error",
            "upstream_error",
        )
        .into_response(),
        Err(ChatDispatchError::InvalidSse(_) | ChatDispatchError::EmptyUpstreamResponse) => {
            openai_error_response(
                StatusCode::BAD_GATEWAY,
                "Invalid upstream Codex response",
                "server_error",
                "invalid_upstream_response",
            )
            .into_response()
        }
    }
}

fn missing_client_api_key_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::UNAUTHORIZED,
        "Missing client API key",
        "invalid_request_error",
        "invalid_api_key",
    )
}

fn model_not_found_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::NOT_FOUND,
        "Model not found",
        "invalid_request_error",
        "model_not_found",
    )
}

fn openai_error_response(
    status: StatusCode,
    message: &str,
    error_type: &str,
    code: &str,
) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": code
            }
        })),
    )
}
