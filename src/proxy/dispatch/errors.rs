//! Shared upstream error classification for dispatch routes.

use chrono::{DateTime, Duration, Utc};

use crate::{
    upstream::accounts::model::AccountStatus,
    upstream::transport::{is_banned_auth_signal, CodexBackendTransport, CodexClientError},
};

pub(crate) fn is_rate_limit_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, .. } if status_code_is_rate_limited(status.as_u16())
    )
}

pub(crate) fn is_retryable_upstream_5xx_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status_code_is_transient_upstream(status.as_u16())
                && !is_history_recovery_signal(body)
    )
}

pub(crate) fn is_quota_exhausted_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, .. } if status_code_is_quota_exhausted(status.as_u16())
    )
}

pub(crate) fn is_auth_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, .. } if status.as_u16() == 401
    )
}

pub(crate) fn is_model_unsupported_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status.is_client_error()
                && !matches!(status.as_u16(), 401 | 402 | 403 | 404 | 429)
                && is_model_unsupported_signal(body)
    )
}

pub(crate) fn is_history_recovery_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { body, .. } if is_history_recovery_signal(body)
    )
}

pub(crate) fn auth_failure_account_status(error: &CodexClientError) -> AccountStatus {
    match error {
        CodexClientError::Upstream { body, .. } if is_banned_auth_signal(body) => {
            AccountStatus::Banned
        }
        _ => AccountStatus::Expired,
    }
}

pub(crate) fn upstream_error_body(error: &CodexClientError) -> String {
    match error {
        CodexClientError::Upstream { body, .. } => body.clone(),
        error => error.to_string(),
    }
}

pub(crate) fn upstream_error_set_cookie_headers(error: &CodexClientError) -> &[String] {
    match error {
        CodexClientError::Upstream {
            set_cookie_headers, ..
        } => set_cookie_headers,
        _ => &[],
    }
}

pub(crate) fn is_model_unsupported_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("model_not_supported")
        || value.contains("model_not_available")
        || (value.contains("model")
            && (value.contains("not supported")
                || value.contains("not available")
                || value.contains("not_supported")
                || value.contains("not_available")))
}

pub(crate) fn is_history_recovery_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("previous_response_not_found")
        || (value.contains("previous response") && value.contains("not found"))
        || value.contains("no tool output found for function call")
        || is_invalid_encrypted_content_signal(&value)
}

pub(crate) fn is_invalid_encrypted_content_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("invalid_encrypted_content")
        || (value.contains("invalid") && value.contains("encrypted") && value.contains("content"))
}

pub(crate) fn rate_limit_cooldown_until(
    error: &CodexClientError,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let retry_after_seconds = match error {
        CodexClientError::Upstream {
            retry_after_seconds,
            ..
        } => retry_after_seconds.unwrap_or(60),
        _ => 60,
    };
    now + Duration::seconds(retry_after_seconds.min(i64::MAX as u64) as i64)
}

pub(crate) fn upstream_error_http_status(error: &CodexClientError) -> u16 {
    match error {
        CodexClientError::Upstream { status, .. } => status.as_u16(),
        _ => 502,
    }
}

pub(crate) fn backend_transport_name(transport: CodexBackendTransport) -> &'static str {
    match transport {
        CodexBackendTransport::HttpSse => "http_sse",
        CodexBackendTransport::WebSocket => "websocket",
    }
}

fn status_code_is_rate_limited(status_code: u16) -> bool {
    status_code == 429
}

fn status_code_is_quota_exhausted(status_code: u16) -> bool {
    status_code == 402
}

fn status_code_is_transient_upstream(status_code: u16) -> bool {
    matches!(status_code, 500..=599)
}
