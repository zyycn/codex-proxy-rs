use axum::{http::StatusCode, Json};
use serde_json::Value;

use crate::{
    codex::gateway::protocol::codex_to_openai::openai_error,
    codex::gateway::transport::http_client::CodexClientError,
};

pub(crate) fn codex_client_error_response(error: CodexClientError) -> (StatusCode, Json<Value>) {
    let status = codex_client_error_status(&error);
    let code = codex_client_error_code(&error);
    let message = codex_client_error_message(&error);
    (status, Json(openai_error(&message, code)))
}

pub(crate) fn codex_client_error_response_with_message(
    error: CodexClientError,
    message: &str,
) -> (StatusCode, Json<Value>) {
    let status = codex_client_error_status(&error);
    let code = codex_client_error_code(&error);
    (status, Json(openai_error(message, code)))
}

pub(crate) fn codex_client_error_message(error: &CodexClientError) -> String {
    match error {
        CodexClientError::UnsupportedTransport(_) => {
            "previous_response_id requires Codex WebSocket transport".to_string()
        }
        CodexClientError::Upstream { status, body, .. }
            if *status == StatusCode::NOT_FOUND && body.trim().is_empty() =>
        {
            "Upstream blocked the request (Cloudflare path-block)".to_string()
        }
        CodexClientError::Upstream { body, .. } => format!(
            "Codex upstream error: {}",
            body.chars().take(300).collect::<String>()
        ),
        _ => "Codex upstream request failed".to_string(),
    }
}

fn codex_client_error_status(error: &CodexClientError) -> StatusCode {
    match error {
        CodexClientError::UnsupportedTransport(_) => StatusCode::BAD_REQUEST,
        CodexClientError::Upstream { status, body, .. }
            if *status == StatusCode::NOT_FOUND && body.trim().is_empty() =>
        {
            StatusCode::BAD_GATEWAY
        }
        CodexClientError::Upstream { status, .. } => *status,
        _ => StatusCode::BAD_GATEWAY,
    }
}

fn codex_client_error_code(error: &CodexClientError) -> &'static str {
    match error {
        CodexClientError::UnsupportedTransport(_) => "websocket_required",
        _ => "upstream_error",
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
