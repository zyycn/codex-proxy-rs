//! OpenAI 路由共享错误响应。

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::{
    dispatch::{
        errors::{DispatchFailureClass, ResponseDispatchError},
        recovery::cyber_policy::is_cyber_policy_failure,
    },
    upstream::openai::protocol::sse::response_failed_sse_event as encode_response_failed_sse_event,
    upstream::openai::transport::CodexClientError,
};

const NO_ACTIVE_UPSTREAM_ACCOUNT_MESSAGE: &str = "No active upstream account is available";
const NO_AVAILABLE_RESPONSES_ACCOUNTS_MESSAGE: &str =
    "No available accounts. All accounts are expired or rate-limited.";
const UPSTREAM_CODEX_REQUEST_FAILED_MESSAGE: &str = "Upstream Codex request failed";
const INVALID_UPSTREAM_CODEX_RESPONSE_MESSAGE: &str = "Invalid upstream Codex response";
const UPSTREAM_CODEX_RESPONSE_FAILED_MESSAGE: &str = "Upstream Codex response failed";
const HISTORY_UNAVAILABLE_MESSAGE: &str = "Previous response context is unavailable. Start a new conversation or resend the complete input without previous_response_id.";
const CONTINUATION_BUSY_MESSAGE: &str =
    "The previous response connection is busy. Retry the request.";

#[derive(Clone, Copy)]
enum ResponseDispatchMessageStyle {
    Standard,
    ResponsesStream,
}

struct DispatchHttpError {
    status: StatusCode,
    message: String,
}

struct OpenAiErrorDetails {
    status: StatusCode,
    message: String,
    error_type: &'static str,
    code: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct OpenAiErrorKind {
    error_type: &'static str,
    code: &'static str,
}

const NO_AVAILABLE_ACCOUNTS_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "server_error",
    code: "no_available_accounts",
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
const HISTORY_UNAVAILABLE_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "invalid_request_error",
    code: "previous_response_unavailable",
};
const CONTINUATION_BUSY_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "server_error",
    code: "continuation_connection_busy",
};
const INVALID_UPSTREAM_RESPONSE_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "server_error",
    code: "invalid_upstream_response",
};
const CODEX_API_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "server_error",
    code: "codex_api_error",
};
const CODEX_CLIENT_ERROR: OpenAiErrorKind = OpenAiErrorKind {
    error_type: "invalid_request_error",
    code: "codex_client_error",
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
    responses_dispatch_error_response_ref(&error)
}

pub fn responses_dispatch_error_response_ref(error: &ResponseDispatchError) -> Response {
    if let ResponseDispatchError::Upstream(CodexClientError::Upstream { status, body, .. }) = error
        && status.is_client_error()
        && let Ok(body) = serde_json::from_str::<Value>(body)
    {
        return (*status, Json(body)).into_response();
    }

    if let ResponseDispatchError::Failed(failure) = error
        && is_cyber_policy_failure(failure)
    {
        let status = status_from_u16(error.client_http_status_code(), StatusCode::BAD_GATEWAY);
        let kind = responses_error_kind_for_status(status);
        return openai_error_response(
            status,
            &failure.message,
            kind.error_type,
            failure.upstream_code.as_deref().unwrap_or(kind.code),
        )
        .into_response();
    }

    match error {
        ResponseDispatchError::NoActiveAccount => {
            responses_no_available_accounts_response().into_response()
        }
        error => response_dispatch_openai_error_response(error),
    }
}

pub fn responses_stream_dispatch_failed_sse_event(error: &ResponseDispatchError) -> String {
    if let ResponseDispatchError::Failed(failure) = error
        && is_cyber_policy_failure(failure)
    {
        let status = status_from_u16(error.client_http_status_code(), StatusCode::BAD_GATEWAY);
        let kind = responses_error_kind_for_status(status);
        return encode_response_failed_sse_event(
            kind.error_type,
            failure.upstream_code.as_deref().unwrap_or(kind.code),
            &failure.message,
        );
    }
    let http_error =
        response_dispatch_http_error(error, ResponseDispatchMessageStyle::ResponsesStream);
    let kind = response_dispatch_error_kind(error);
    responses_failed_sse_event(kind, &http_error.message)
}

pub(super) fn responses_websocket_dispatch_error_event(
    error: &ResponseDispatchError,
    request_id: &str,
) -> String {
    if let ResponseDispatchError::Failed(failure) = error
        && is_cyber_policy_failure(failure)
    {
        let status = status_from_u16(error.client_http_status_code(), StatusCode::BAD_GATEWAY);
        let kind = responses_error_kind_for_status(status);
        return responses_websocket_error_event(
            status,
            kind.error_type,
            failure.upstream_code.as_deref().unwrap_or(kind.code),
            &failure.message,
            request_id,
        );
    }
    if let Some(event) = upstream_client_websocket_error_event(error, request_id) {
        return event;
    }
    let http_error =
        response_dispatch_http_error(error, ResponseDispatchMessageStyle::ResponsesStream);
    let kind = response_dispatch_error_kind(error);
    responses_websocket_error_event(
        http_error.status,
        kind.error_type,
        kind.code,
        &http_error.message,
        request_id,
    )
}

pub(super) fn responses_websocket_error_event(
    status: StatusCode,
    error_type: &str,
    code: &str,
    message: &str,
    request_id: &str,
) -> String {
    json!({
        "type": "error",
        "status": status.as_u16(),
        "error": {
            "type": error_type,
            "code": code,
            "message": message,
        },
        "headers": {
            "x-request-id": request_id,
        },
    })
    .to_string()
}

fn upstream_client_websocket_error_event(
    error: &ResponseDispatchError,
    request_id: &str,
) -> Option<String> {
    let ResponseDispatchError::Upstream(CodexClientError::Upstream { status, body, .. }) = error
    else {
        return None;
    };
    if !status.is_client_error() {
        return None;
    }
    let Value::Object(mut payload) = serde_json::from_str::<Value>(body).ok()? else {
        return None;
    };
    if !payload.get("error").is_some_and(Value::is_object) {
        return None;
    }
    payload.insert("type".to_string(), Value::String("error".to_string()));
    payload.insert("status".to_string(), Value::Number(status.as_u16().into()));
    let headers = payload
        .entry("headers".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !headers.is_object() {
        *headers = Value::Object(serde_json::Map::new());
    }
    headers.as_object_mut()?.insert(
        "x-request-id".to_string(),
        Value::String(request_id.to_string()),
    );
    Some(Value::Object(payload).to_string())
}

fn responses_no_available_accounts_response() -> (StatusCode, Json<Value>) {
    let kind = NO_AVAILABLE_ACCOUNTS_ERROR;
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "type": kind.error_type,
                "code": kind.code,
                "message": NO_AVAILABLE_RESPONSES_ACCOUNTS_MESSAGE,
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
    let DispatchHttpError { status, message } =
        response_dispatch_http_error(error, ResponseDispatchMessageStyle::Standard);
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
        status_from_u16(error.client_http_status_code(), StatusCode::BAD_GATEWAY),
    )
}

fn response_dispatch_http_error(
    error: &ResponseDispatchError,
    message_style: ResponseDispatchMessageStyle,
) -> DispatchHttpError {
    DispatchHttpError {
        status: status_from_u16(error.client_http_status_code(), StatusCode::BAD_GATEWAY),
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
        CODEX_CLIENT_ERROR
    } else {
        CODEX_API_ERROR
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
        DispatchFailureClass::HistoryUnavailable => HISTORY_UNAVAILABLE_MESSAGE.to_owned(),
        DispatchFailureClass::ContinuationBusy => CONTINUATION_BUSY_MESSAGE.to_owned(),
        _ => UPSTREAM_CODEX_REQUEST_FAILED_MESSAGE.to_owned(),
    }
}

fn dispatch_failure_openai_error_kind(
    failure_class: DispatchFailureClass,
    status: StatusCode,
) -> OpenAiErrorKind {
    match failure_class {
        DispatchFailureClass::NoAvailableAccounts => NO_AVAILABLE_ACCOUNTS_ERROR,
        DispatchFailureClass::QuotaExhausted => INSUFFICIENT_QUOTA_ERROR,
        DispatchFailureClass::RateLimited => RATE_LIMIT_ERROR,
        DispatchFailureClass::Expired
        | DispatchFailureClass::Disabled
        | DispatchFailureClass::Banned => AUTHENTICATION_ERROR,
        DispatchFailureClass::ModelUnsupported => MODEL_NOT_FOUND_ERROR,
        DispatchFailureClass::HistoryUnavailable => HISTORY_UNAVAILABLE_ERROR,
        DispatchFailureClass::ContinuationBusy => CONTINUATION_BUSY_ERROR,
        DispatchFailureClass::InvalidSse
        | DispatchFailureClass::MissingCompleted
        | DispatchFailureClass::EmptyUpstreamResponse => INVALID_UPSTREAM_RESPONSE_ERROR,
        DispatchFailureClass::ResponseFailed => responses_error_kind_for_status(status),
        DispatchFailureClass::Upstream if status.is_client_error() => {
            responses_error_kind_for_status(status)
        }
        DispatchFailureClass::Upstream | DispatchFailureClass::UpstreamUnavailable => {
            UPSTREAM_ERROR
        }
        DispatchFailureClass::CloudflareChallenge | DispatchFailureClass::CloudflarePathBlocked => {
            UPSTREAM_ERROR
        }
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
