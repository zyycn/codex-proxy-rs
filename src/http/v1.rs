use axum::{http::StatusCode, response::IntoResponse, Json};

use crate::translation::codex_to_openai::openai_error;

pub async fn responses() -> impl IntoResponse {
    (
        StatusCode::UNAUTHORIZED,
        Json(openai_error("Missing client API key", "invalid_api_key")),
    )
}
