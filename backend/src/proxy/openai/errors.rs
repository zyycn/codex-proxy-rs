//! OpenAI 路由共享错误响应。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::proxy::{
    dispatch::{errors::DispatchFailureClass, responses::errors::ResponseDispatchError},
    openai::responses::response_failed_sse_event as encode_response_failed_sse_event,
};

const NO_ACTIVE_UPSTREAM_ACCOUNT_MESSAGE: &str = "No active upstream account is available";
const NO_AVAILABLE_RESPONSES_ACCOUNTS_MESSAGE: &str =
    "No available accounts. All accounts are expired or rate-limited.";
const UPSTREAM_CODEX_REQUEST_FAILED_MESSAGE: &str = "Upstream Codex request failed";
const INVALID_UPSTREAM_CODEX_RESPONSE_MESSAGE: &str = "Invalid upstream Codex response";
const UPSTREAM_CODEX_RESPONSE_FAILED_MESSAGE: &str = "Upstream Codex response failed";

#[derive(Clone, Copy)]
pub enum ResponseDispatchMessageStyle {
    Standard,
    ResponsesStream,
}

#[derive(Clone, Copy)]
pub enum ResponseDispatchStatusMode {
    UpstreamFailureStatus,
    Client,
}

pub struct DispatchHttpError {
    pub status: StatusCode,
    pub message: String,
}

pub struct OpenAiErrorDetails {
    pub status: StatusCode,
    pub message: String,
    pub error_type: &'static str,
    pub code: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct OpenAiErrorKind {
    error_type: &'static str,
    code: &'static str,
}

const UPSTREAM_UNAVAILABLE_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "server_error",
    code: "upstream_unavailable",
};
const UPSTREAM_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "server_error",
    code: "upstream_error",
};
const INSUFFICIENT_QUOTA_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "insufficient_quota",
    code: "insufficient_quota",
};
const RATE_LIMIT_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "rate_limit_error",
    code: "rate_limit_exceeded",
};
const AUTHENTICATION_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "invalid_request_error",
    code: "invalid_api_key",
};
const MODEL_NOT_FOUND_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "invalid_request_error",
    code: "model_not_found",
};
const INVALID_UPSTREAM_RESPONSE_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "server_error",
    code: "invalid_upstream_response",
};
const CODEX_API_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "server_error",
    code: "codex_api_error",
};

/// OpenAI 兼容错误响应。
pub fn openai_error_response(
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

pub fn responses_dispatch_error_response(error: ResponseDispatchError) -> Response {
    match error {
        ResponseDispatchError::NoActiveAccount => {
            responses_no_available_accounts_response().into_response()
        }
        error => response_dispatch_openai_error_response(&error),
    }
}

pub fn responses_compact_dispatch_error_response(error: ResponseDispatchError) -> Response {
    match error {
        ResponseDispatchError::NoActiveAccount => {
            responses_no_available_accounts_response().into_response()
        }
        error => {
            let error = response_dispatch_http_error(
                &error,
                ResponseDispatchStatusMode::Client,
                ResponseDispatchMessageStyle::Standard,
            );
            responses_error_response(error.status, &error.message).into_response()
        }
    }
}

pub fn responses_stream_dispatch_failed_sse_event(error: &ResponseDispatchError) -> String {
    let http_error = response_dispatch_http_error(
        error,
        ResponseDispatchStatusMode::UpstreamFailureStatus,
        ResponseDispatchMessageStyle::ResponsesStream,
    );
    let kind = response_dispatch_error_kind(error);
    responses_failed_sse_event(kind, &http_error.message)
}

fn responses_no_available_accounts_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "type": "server_error",
                "code": "no_available_accounts",
                "message": NO_AVAILABLE_RESPONSES_ACCOUNTS_MESSAGE,
            }
        })),
    )
}

fn responses_error_response(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    let kind = responses_error_kind_for_status(status);
    (
        status,
        Json(json!({
            "error": {
                "type": kind.error_type,
                "code": kind.code,
                "message": message,
            }
        })),
    )
}

fn response_dispatch_openai_error_response(error: &ResponseDispatchError) -> Response {
    let error = response_dispatch_openai_error(error);
    openai_error_response(error.status, &error.message, error.error_type, error.code)
        .into_response()
}

fn responses_failed_sse_event(kind: OpenAiErrorKind, message: &str) -> String {
    encode_response_failed_sse_event(kind.error_type, kind.code, message)
}

/// 缺失 client API key。
pub fn missing_client_api_key_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::UNAUTHORIZED,
        "Missing client API key",
        "invalid_request_error",
        "invalid_api_key",
    )
}

/// 模型不存在。
pub fn model_not_found_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::NOT_FOUND,
        "Model not found",
        "invalid_request_error",
        "model_not_found",
    )
}

/// Responses 请求无效。
pub fn invalid_responses_request_response() -> (StatusCode, Json<Value>) {
    invalid_openai_request_response("Invalid responses request")
}

fn invalid_openai_request_response(message: &str) -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::BAD_REQUEST,
        message,
        "invalid_request_error",
        "invalid_request",
    )
}

fn response_dispatch_openai_error(error: &ResponseDispatchError) -> OpenAiErrorDetails {
    let DispatchHttpError { status, message } = response_dispatch_http_error(
        error,
        ResponseDispatchStatusMode::UpstreamFailureStatus,
        ResponseDispatchMessageStyle::Standard,
    );
    let kind = response_dispatch_error_kind(error);

    OpenAiErrorDetails {
        status,
        message,
        error_type: kind.error_type,
        code: kind.code,
    }
}

fn response_dispatch_error_kind(error: &ResponseDispatchError) -> OpenAiErrorKind {
    dispatch_failure_openai_error_kind(
        error.metadata().failure_class,
        status_from_u16(error.http_status_code(), StatusCode::BAD_GATEWAY),
    )
}

pub fn response_dispatch_http_error(
    error: &ResponseDispatchError,
    status_mode: ResponseDispatchStatusMode,
    message_style: ResponseDispatchMessageStyle,
) -> DispatchHttpError {
    DispatchHttpError {
        status: response_dispatch_status(error, status_mode),
        message: response_dispatch_message(error, message_style),
    }
}

fn responses_error_kind_for_status(status: StatusCode) -> OpenAiErrorKind {
    if status == StatusCode::TOO_MANY_REQUESTS {
        RATE_LIMIT_ERROR
    } else if status == StatusCode::PAYMENT_REQUIRED {
        INSUFFICIENT_QUOTA_ERROR
    } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        AUTHENTICATION_ERROR
    } else if status.is_client_error() {
        OpenAiErrorKind {
            error_type: "invalid_request_error",
            code: "codex_api_error",
        }
    } else {
        CODEX_API_ERROR
    }
}

fn response_dispatch_status(
    error: &ResponseDispatchError,
    mode: ResponseDispatchStatusMode,
) -> StatusCode {
    match (mode, error) {
        (ResponseDispatchStatusMode::UpstreamFailureStatus, ResponseDispatchError::Upstream(_)) => {
            StatusCode::BAD_GATEWAY
        }
        _ => status_from_u16(error.http_status_code(), StatusCode::BAD_GATEWAY),
    }
}

fn response_dispatch_message(
    error: &ResponseDispatchError,
    style: ResponseDispatchMessageStyle,
) -> String {
    let metadata = error.metadata();
    if let Some(exhausted) = error.exhausted_account() {
        return exhausted_dispatch_message(
            exhausted.count,
            exhausted.kind.message_reason(),
            exhausted.upstream_error,
            matches!(
                (metadata.failure_class, style),
                (
                    DispatchFailureClass::CloudflarePathBlocked
                        | DispatchFailureClass::ModelUnsupported,
                    ResponseDispatchMessageStyle::ResponsesStream,
                )
            ),
        );
    }
    dispatch_failure_message(
        metadata.failure_class,
        match style {
            ResponseDispatchMessageStyle::Standard => DispatchFailureMessageStyle::Standard,
            ResponseDispatchMessageStyle::ResponsesStream => {
                DispatchFailureMessageStyle::ResponsesStream
            }
        },
    )
}

#[derive(Clone, Copy)]
enum DispatchFailureMessageStyle {
    Standard,
    ResponsesStream,
}

fn dispatch_failure_message(
    failure_class: DispatchFailureClass,
    _style: DispatchFailureMessageStyle,
) -> String {
    match failure_class {
        DispatchFailureClass::NoAvailableAccounts => NO_ACTIVE_UPSTREAM_ACCOUNT_MESSAGE.to_owned(),
        DispatchFailureClass::Upstream => UPSTREAM_CODEX_REQUEST_FAILED_MESSAGE.to_owned(),
        DispatchFailureClass::InvalidSse
        | DispatchFailureClass::MissingCompleted
        | DispatchFailureClass::EmptyUpstreamResponse => {
            INVALID_UPSTREAM_CODEX_RESPONSE_MESSAGE.to_owned()
        }
        DispatchFailureClass::ResponseFailed => UPSTREAM_CODEX_RESPONSE_FAILED_MESSAGE.to_owned(),
        _ => UPSTREAM_CODEX_REQUEST_FAILED_MESSAGE.to_owned(),
    }
}

fn dispatch_failure_openai_error_kind(
    failure_class: DispatchFailureClass,
    status: StatusCode,
) -> OpenAiErrorKind {
    match failure_class {
        DispatchFailureClass::NoAvailableAccounts => UPSTREAM_UNAVAILABLE_ERROR,
        DispatchFailureClass::QuotaExhausted => INSUFFICIENT_QUOTA_ERROR,
        DispatchFailureClass::RateLimited => RATE_LIMIT_ERROR,
        DispatchFailureClass::Expired
        | DispatchFailureClass::Disabled
        | DispatchFailureClass::Banned => AUTHENTICATION_ERROR,
        DispatchFailureClass::ModelUnsupported => MODEL_NOT_FOUND_ERROR,
        DispatchFailureClass::InvalidSse
        | DispatchFailureClass::MissingCompleted
        | DispatchFailureClass::EmptyUpstreamResponse => INVALID_UPSTREAM_RESPONSE_ERROR,
        DispatchFailureClass::ResponseFailed => responses_error_kind_for_status(status),
        DispatchFailureClass::Upstream
        | DispatchFailureClass::CloudflareChallenge
        | DispatchFailureClass::CloudflarePathBlocked => UPSTREAM_ERROR,
    }
}

fn exhausted_dispatch_message(
    count: usize,
    reason: &str,
    upstream_error: &str,
    no_accounts_prefix: bool,
) -> String {
    if no_accounts_prefix {
        format!(
            "No accounts available. All accounts exhausted ({count} {reason}). Codex upstream error: {upstream_error}"
        )
    } else {
        format!("All accounts exhausted ({count} {reason}). Codex upstream error: {upstream_error}")
    }
}

fn status_from_u16(status: u16, fallback: StatusCode) -> StatusCode {
    StatusCode::from_u16(status).unwrap_or(fallback)
}
