use std::time::Instant;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue},
    response::{IntoResponse, Response},
    Extension,
};

use crate::{platform::http::request_id::RequestId, runtime::state::AppState};

use super::{auth::authorize_client_api_key, errors::missing_client_api_key_response};

const OPENAI_SUBAGENT_HEADER: HeaderName = HeaderName::from_static("x-openai-subagent");

pub async fn responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_responses(state, request_id, headers, body).await
}

pub async fn review_responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    mut headers: HeaderMap,
    body: Bytes,
) -> Response {
    headers.insert(OPENAI_SUBAGENT_HEADER, HeaderValue::from_static("review"));
    handle_responses(state, request_id, headers, body).await
}

pub async fn compact_responses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started_at = Instant::now();
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response().into_response();
    }

    state
        .services
        .responses
        .handle_compact(request_id.as_str(), body, started_at)
        .await
}

async fn handle_responses(
    state: AppState,
    request_id: RequestId,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started_at = Instant::now();
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response().into_response();
    }

    state
        .services
        .responses
        .handle(request_id.as_str(), headers, body, started_at)
        .await
}
