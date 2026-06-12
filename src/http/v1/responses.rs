use std::time::Instant;

use axum::{
    body::Bytes,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    Extension,
};

use crate::{app::state::AppState, http::middleware::RequestId};

use super::{auth::authorize_client_api_key, errors::missing_client_api_key_response};

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

    state
        .services
        .responses
        .handle(request_id.as_str(), headers, body, started_at)
        .await
}
