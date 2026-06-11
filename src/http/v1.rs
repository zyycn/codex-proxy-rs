use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use crate::translation::codex_to_openai::openai_error;

#[derive(Deserialize)]
struct ResponsesBody {
    model: Option<String>,
}

pub async fn responses(headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    let auth = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !auth.starts_with("Bearer cpr_") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(openai_error("Missing client API key", "invalid_api_key")),
        );
    }

    let body = serde_json::from_slice::<ResponsesBody>(&body).unwrap_or_else(|_| ResponsesBody {
        model: Some("gpt-5.4".to_string()),
    });
    let model = body.model.unwrap_or_else(|| "gpt-5.4".to_string());
    if !(model.starts_with("gpt") || model.starts_with("codex") || model.starts_with('o')) {
        return (
            StatusCode::NOT_FOUND,
            Json(openai_error("Model not found", "model_not_found")),
        );
    }

    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(openai_error(
            "No available Codex accounts",
            "no_available_accounts",
        )),
    )
}
