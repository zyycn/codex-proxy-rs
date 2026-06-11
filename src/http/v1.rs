use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::{http::auth::client_api_key, translation::codex_to_openai::openai_error};

const DEFAULT_CODEX_MODEL: &str = "gpt-5.5";

#[derive(Deserialize)]
struct ResponsesBody {
    model: Option<String>,
}

pub async fn responses(headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    if !has_client_api_key(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(openai_error("Missing client API key", "invalid_api_key")),
        );
    }

    let body = serde_json::from_slice::<ResponsesBody>(&body).unwrap_or_else(|_| ResponsesBody {
        model: Some(DEFAULT_CODEX_MODEL.to_string()),
    });
    let model = body
        .model
        .unwrap_or_else(|| DEFAULT_CODEX_MODEL.to_string());
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

pub async fn models(headers: HeaderMap) -> impl IntoResponse {
    if !has_client_api_key(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(openai_error("Missing client API key", "invalid_api_key")),
        );
    }

    (
        StatusCode::OK,
        Json(json!({
            "object": "list",
            "data": [
                {
                    "id": DEFAULT_CODEX_MODEL,
                    "object": "model",
                    "created": 0,
                    "owned_by": "openai"
                }
            ]
        })),
    )
}

fn has_client_api_key(headers: &HeaderMap) -> bool {
    client_api_key(headers).is_some()
}
