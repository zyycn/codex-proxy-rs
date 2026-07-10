//! Shared upstream error classification for dispatch routes.

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};

use crate::{
    accounts::account::AccountStatus,
    dispatch::exhaustion::{ExhaustedAccountKind, ExhaustedAccountRef},
    upstream::openai::transport::{
        is_banned_auth_signal, CodexBackendTransport, CodexClientError, CodexUpstreamDiagnostics,
    },
};

#[derive(Clone, Debug)]
pub(crate) struct DispatchErrorMetadata {
    pub failure_class: DispatchFailureClass,
    pub exhausted_count: Option<usize>,
    pub upstream_error: Option<String>,
    pub upstream_status: Option<u16>,
    pub diagnostics: Option<CodexUpstreamDiagnostics>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum DispatchFailureClass {
    NoAvailableAccounts,
    QuotaExhausted,
    RateLimited,
    Expired,
    Disabled,
    Banned,
    CloudflareChallenge,
    CloudflarePathBlocked,
    ModelUnsupported,
    HistoryUnavailable,
    Upstream,
    InvalidSse,
    MissingCompleted,
    EmptyUpstreamResponse,
    ResponseFailed,
}

impl DispatchFailureClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoAvailableAccounts => "no_available_accounts",
            Self::QuotaExhausted => "quota_exhausted",
            Self::RateLimited => "rate_limited",
            Self::Expired => "expired",
            Self::Disabled => "disabled",
            Self::Banned => "banned",
            Self::CloudflareChallenge => "cloudflare_challenge",
            Self::CloudflarePathBlocked => "cloudflare_path_blocked",
            Self::ModelUnsupported => "model_unsupported",
            Self::HistoryUnavailable => "history_unavailable",
            Self::Upstream => "upstream",
            Self::InvalidSse => "invalid_sse",
            Self::MissingCompleted => "missing_completed",
            Self::EmptyUpstreamResponse => "empty_upstream_response",
            Self::ResponseFailed => "response_failed",
        }
    }
}

impl DispatchErrorMetadata {
    pub(crate) fn no_available_accounts() -> Self {
        Self {
            failure_class: DispatchFailureClass::NoAvailableAccounts,
            exhausted_count: None,
            upstream_error: None,
            upstream_status: None,
            diagnostics: None,
        }
    }

    pub(crate) fn exhausted_ref(exhausted: ExhaustedAccountRef<'_>) -> Self {
        Self {
            failure_class: DispatchFailureClass::from(exhausted.kind),
            exhausted_count: Some(exhausted.count),
            upstream_error: Some(exhausted.upstream_error.to_string()),
            upstream_status: None,
            diagnostics: None,
        }
    }

    pub(crate) fn upstream(error: &CodexClientError) -> Self {
        Self {
            failure_class: DispatchFailureClass::Upstream,
            exhausted_count: None,
            upstream_error: Some(upstream_error_body(error)),
            upstream_status: match error {
                CodexClientError::Upstream { status, .. } => Some(status.as_u16()),
                _ => None,
            },
            diagnostics: upstream_error_diagnostics(error).cloned(),
        }
    }

    pub(crate) fn simple(failure_class: DispatchFailureClass) -> Self {
        Self {
            failure_class,
            exhausted_count: None,
            upstream_error: None,
            upstream_status: None,
            diagnostics: None,
        }
    }
}

impl From<ExhaustedAccountKind> for DispatchFailureClass {
    fn from(kind: ExhaustedAccountKind) -> Self {
        match kind {
            ExhaustedAccountKind::QuotaExhausted => Self::QuotaExhausted,
            ExhaustedAccountKind::RateLimited => Self::RateLimited,
            ExhaustedAccountKind::Expired => Self::Expired,
            ExhaustedAccountKind::Disabled => Self::Disabled,
            ExhaustedAccountKind::Banned => Self::Banned,
            ExhaustedAccountKind::CloudflareChallenge => Self::CloudflareChallenge,
            ExhaustedAccountKind::CloudflarePathBlocked => Self::CloudflarePathBlocked,
            ExhaustedAccountKind::ModelUnsupported => Self::ModelUnsupported,
        }
    }
}

pub(crate) fn insert_dispatch_error_metadata(
    object: &mut Map<String, Value>,
    metadata: DispatchErrorMetadata,
) {
    object.insert(
        "failureClass".to_string(),
        Value::String(metadata.failure_class.as_str().to_string()),
    );
    if let Some(count) = metadata.exhausted_count {
        object.insert("exhaustedCount".to_string(), json!(count));
    }
    if let Some(error) = metadata.upstream_error {
        object.insert("upstreamError".to_string(), Value::String(error));
    }
    if let Some(status) = metadata.upstream_status {
        object.insert("upstreamStatus".to_string(), json!(status));
    }
    if let Some(diagnostics) = metadata.diagnostics {
        insert_upstream_diagnostics_metadata(object, &diagnostics);
    }
}

pub(crate) fn insert_upstream_diagnostics_metadata(
    object: &mut Map<String, Value>,
    diagnostics: &CodexUpstreamDiagnostics,
) {
    if diagnostics.is_empty() {
        return;
    }
    if let Some(status_code) = diagnostics.status_code {
        object.insert("upstreamStatus".to_string(), json!(status_code));
    }
    if let Some(request_id) = &diagnostics.request_id {
        object.insert(
            "upstreamRequestId".to_string(),
            Value::String(request_id.clone()),
        );
    }
    if !diagnostics.trace_headers.is_empty() {
        object.insert(
            "upstreamTraceHeaders".to_string(),
            json!(diagnostics.trace_headers),
        );
    }
    if let Some(cf_ray) = diagnostics.cf_ray() {
        object.insert("cfRay".to_string(), Value::String(cf_ray.to_string()));
    }
}

pub(crate) fn upstream_error_diagnostics(
    error: &CodexClientError,
) -> Option<&CodexUpstreamDiagnostics> {
    match error {
        CodexClientError::Upstream { diagnostics, .. } => Some(diagnostics),
        _ => None,
    }
}

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
