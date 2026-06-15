use axum::{http::StatusCode, Json};
use serde_json::Value;

use crate::{
    codex::gateway::protocol::codex_to_openai::openai_error,
    codex::gateway::transport::http_client::CodexClientError,
};

pub(crate) fn codex_client_error_response(error: CodexClientError) -> (StatusCode, Json<Value>) {
    match error {
        CodexClientError::UnsupportedTransport(_) => (
            StatusCode::BAD_REQUEST,
            Json(openai_error(
                "previous_response_id requires Codex WebSocket transport",
                "websocket_required",
            )),
        ),
        CodexClientError::Upstream { status, body, .. }
            if status == StatusCode::NOT_FOUND && body.trim().is_empty() =>
        {
            (
                StatusCode::BAD_GATEWAY,
                Json(openai_error(
                    "Upstream blocked the request (Cloudflare path-block)",
                    "upstream_error",
                )),
            )
        }
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

pub(super) fn missing_client_api_key_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(openai_error("Missing client API key", "invalid_api_key")),
    )
}

pub(crate) fn model_not_found_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(openai_error("Model not found", "model_not_found")),
    )
}
