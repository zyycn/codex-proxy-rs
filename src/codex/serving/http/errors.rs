use axum::{http::StatusCode, Json};
use serde_json::{json, Value};

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

pub(crate) fn codex_client_error_response_with_status_and_message(
    error: CodexClientError,
    status: StatusCode,
    message: &str,
) -> (StatusCode, Json<Value>) {
    let code = codex_client_error_code(&error);
    (status, Json(openai_error(message, code)))
}

pub(crate) fn responses_codex_client_error_response(
    error: CodexClientError,
) -> (StatusCode, Json<Value>) {
    let status = codex_client_error_status(&error);
    let message = codex_client_error_message(&error);
    responses_error_response(status, &message)
}

pub(crate) fn responses_error_response(
    status: StatusCode,
    message: &str,
) -> (StatusCode, Json<Value>) {
    let (error_type, code) = if status == StatusCode::TOO_MANY_REQUESTS {
        ("rate_limit_error", "rate_limit_exceeded")
    } else {
        ("server_error", "codex_api_error")
    };
    (
        status,
        Json(json!({
            "type": "error",
            "error": {
                "type": error_type,
                "code": code,
                "message": message,
            }
        })),
    )
}

pub(crate) fn responses_no_available_accounts_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "type": "error",
            "error": {
                "type": "server_error",
                "code": "no_available_accounts",
                "message": "No available accounts. All accounts are expired or rate-limited.",
            }
        })),
    )
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
