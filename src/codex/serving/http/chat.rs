use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use std::time::Instant;

use crate::{
    codex::gateway::protocol::{
        codex_to_openai::openai_error, openai_to_codex::ChatCompletionRequest,
    },
    platform::http::middleware::RequestId,
    runtime::state::AppState,
};

use super::{auth::authorize_client_api_key, errors::missing_client_api_key_response};

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
    state
        .services
        .chat
        .handle(request_id.as_str(), chat_request, started_at)
        .await
}
