//! Shared upstream error classification for dispatch routes.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::{
    dispatch::{
        recovery::exhaustion::{ExhaustedAccount, ExhaustedAccountKind, ExhaustedAccountRef},
        stream::sse_failure::{sse_failure_error_body, stream_failure_http_status},
    },
    upstream::openai::{
        protocol::{responses::ResponsesSseFailure, sse::SseError},
        transport::{
            CodexBackendTransport, CodexClientError, CodexUpstreamDiagnostics,
            is_banned_upstream_error, is_cyber_policy_upstream_error,
        },
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
    UpstreamUnavailable,
    ContinuationBusy,
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
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::ContinuationBusy => "continuation_busy",
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
            ExhaustedAccountKind::UpstreamUnavailable => Self::UpstreamUnavailable,
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
    if is_cyber_policy_upstream_error(error) {
        return false;
    }
    matches!(
        error,
        CodexClientError::Upstream { status, .. } if status_code_is_rate_limited(status.as_u16())
    )
}

pub(crate) fn is_retryable_upstream_5xx_error(error: &CodexClientError) -> bool {
    if is_cyber_policy_upstream_error(error) {
        return false;
    }
    matches!(
        error,
        CodexClientError::Upstream { status, .. }
            if status_code_is_transient_upstream(status.as_u16())
    )
}

pub(crate) fn is_retryable_account_transport_error(error: &CodexClientError) -> bool {
    match error {
        CodexClientError::Http(error) => error.is_connect() || error.is_timeout(),
        CodexClientError::StreamIdleTimeout { .. } => true,
        CodexClientError::WebSocket(
            crate::upstream::openai::transport::websocket::CodexWebSocketExchangeError::Transport(_)
            | crate::upstream::openai::transport::websocket::CodexWebSocketExchangeError::ConnectTimeout { .. }
            | crate::upstream::openai::transport::websocket::CodexWebSocketExchangeError::SendTimeout { .. }
            | crate::upstream::openai::transport::websocket::CodexWebSocketExchangeError::ClosedBeforeTerminal
            | crate::upstream::openai::transport::websocket::CodexWebSocketExchangeError::ReceiveIdleTimeout { .. }
            | crate::upstream::openai::transport::websocket::CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstOutput { .. }
            | crate::upstream::openai::transport::websocket::CodexWebSocketExchangeError::InitialEventTimeout { .. },
        ) => true,
        _ => is_retryable_upstream_5xx_error(error),
    }
}

pub(crate) fn is_quota_exhausted_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, .. }
            if status_code_is_quota_exhausted(status.as_u16())
                && !is_banned_upstream_error(error)
                && !is_cyber_policy_upstream_error(error)
    )
}

pub(crate) fn is_model_unsupported_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status.is_client_error()
                && !matches!(status.as_u16(), 401 | 402 | 403 | 404 | 429)
                && !is_cyber_policy_upstream_error(error)
                && is_model_unsupported_signal(body)
    )
}

pub(crate) fn is_history_recovery_upstream_error(error: &CodexClientError) -> bool {
    upstream_error_code(error).is_some_and(|code| is_history_recovery_code(&code))
        || matches!(
            error,
            CodexClientError::WebSocket(
                crate::upstream::openai::transport::websocket::CodexWebSocketExchangeError::ContinuationUnavailable { reason }
            ) if !matches!(
                reason,
                crate::upstream::openai::transport::websocket::PreviousResponseUnavailableReason::ConnectionBusy
            )
        )
}

pub(crate) fn is_continuation_busy_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::WebSocket(
            crate::upstream::openai::transport::websocket::CodexWebSocketExchangeError::ContinuationUnavailable {
                reason: crate::upstream::openai::transport::websocket::PreviousResponseUnavailableReason::ConnectionBusy,
            }
        )
    )
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

pub(crate) fn is_history_recovery_code(code: &str) -> bool {
    matches!(
        code,
        "previous_response_not_found"
            | "invalid_encrypted_content"
            | "missing_tool_output"
            | "no_tool_output"
    )
}

pub(crate) fn upstream_error_code(error: &CodexClientError) -> Option<String> {
    let CodexClientError::Upstream { body, .. } = error else {
        return None;
    };
    let value = serde_json::from_str::<Value>(body).ok()?;
    value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
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
/// Responses 调度错误。
#[derive(Debug, Error)]
pub enum ResponseDispatchError {
    #[error("no active account is available")]
    NoActiveAccount,
    #[error("all accounts exhausted by quota")]
    QuotaExhausted {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by rate limit")]
    RateLimited {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by expired auth")]
    Expired {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by disabled auth")]
    Disabled {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by banned auth")]
    Banned {
        count: usize,
        upstream_error: String,
        status_code: u16,
    },
    #[error("all accounts exhausted by Cloudflare challenge")]
    CloudflareChallenge {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by Cloudflare path-block")]
    CloudflarePathBlocked {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by unsupported model")]
    ModelUnsupported {
        count: usize,
        upstream_error: String,
    },
    #[error("all account candidates failed with transient upstream errors")]
    UpstreamUnavailable {
        count: usize,
        upstream_error: String,
    },
    #[error("the previous response connection is busy; retry the request")]
    ContinuationBusy,
    #[error("previous response context is unavailable: {upstream_error}")]
    HistoryUnavailable { upstream_error: String },
    #[error("upstream request failed: {0}")]
    Upstream(#[from] CodexClientError),
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
    #[error("upstream response did not include response.completed")]
    MissingCompleted,
    #[error("upstream response did not include visible output")]
    EmptyUpstreamResponse,
    #[error("upstream response failed: {0:?}")]
    Failed(ResponsesSseFailure),
}

impl ResponseDispatchError {
    pub(crate) fn from_exhausted_account(exhausted: ExhaustedAccount) -> Self {
        match exhausted.kind {
            ExhaustedAccountKind::QuotaExhausted => Self::QuotaExhausted {
                count: exhausted.count,
                upstream_error: exhausted.upstream_error,
            },
            ExhaustedAccountKind::RateLimited => Self::RateLimited {
                count: exhausted.count,
                upstream_error: exhausted.upstream_error,
            },
            ExhaustedAccountKind::Expired => Self::Expired {
                count: exhausted.count,
                upstream_error: exhausted.upstream_error,
            },
            ExhaustedAccountKind::Disabled => Self::Disabled {
                count: exhausted.count,
                upstream_error: exhausted.upstream_error,
            },
            ExhaustedAccountKind::Banned => Self::Banned {
                count: exhausted.count,
                upstream_error: exhausted.upstream_error,
                status_code: exhausted.status_code.unwrap_or(403),
            },
            ExhaustedAccountKind::CloudflareChallenge => Self::CloudflareChallenge {
                count: exhausted.count,
                upstream_error: exhausted.upstream_error,
            },
            ExhaustedAccountKind::CloudflarePathBlocked => Self::CloudflarePathBlocked {
                count: exhausted.count,
                upstream_error: exhausted.upstream_error,
            },
            ExhaustedAccountKind::ModelUnsupported => Self::ModelUnsupported {
                count: exhausted.count,
                upstream_error: exhausted.upstream_error,
            },
            ExhaustedAccountKind::UpstreamUnavailable => Self::UpstreamUnavailable {
                count: exhausted.count,
                upstream_error: exhausted.upstream_error,
            },
        }
    }

    pub fn http_status_code(&self) -> u16 {
        match self {
            Self::NoActiveAccount => 503,
            Self::QuotaExhausted { .. } => 429,
            Self::RateLimited { .. } => 429,
            Self::Expired { .. } | Self::Disabled { .. } => 401,
            Self::Banned { status_code, .. } => *status_code,
            Self::CloudflareChallenge { .. }
            | Self::CloudflarePathBlocked { .. }
            | Self::InvalidSse(_)
            | Self::MissingCompleted
            | Self::EmptyUpstreamResponse => 502,
            Self::UpstreamUnavailable { .. } => 503,
            Self::ContinuationBusy => 503,
            Self::Failed(failure) => stream_failure_http_status(failure),
            Self::ModelUnsupported { .. } | Self::HistoryUnavailable { .. } => 400,
            Self::Upstream(error) => upstream_error_http_status(error),
        }
    }

    pub fn client_http_status_code(&self) -> u16 {
        match self {
            Self::Upstream(_) => client_upstream_http_status_code(self.http_status_code()),
            _ => self.http_status_code(),
        }
    }

    pub(crate) fn metadata(&self) -> DispatchErrorMetadata {
        match self {
            Self::NoActiveAccount => DispatchErrorMetadata::no_available_accounts(),
            Self::QuotaExhausted {
                count,
                upstream_error,
            } => Self::exhausted_metadata(
                ExhaustedAccountKind::QuotaExhausted,
                *count,
                upstream_error,
            ),
            Self::RateLimited {
                count,
                upstream_error,
            } => {
                Self::exhausted_metadata(ExhaustedAccountKind::RateLimited, *count, upstream_error)
            }
            Self::Expired {
                count,
                upstream_error,
            } => Self::exhausted_metadata(ExhaustedAccountKind::Expired, *count, upstream_error),
            Self::Disabled {
                count,
                upstream_error,
            } => Self::exhausted_metadata(ExhaustedAccountKind::Disabled, *count, upstream_error),
            Self::Banned {
                count,
                upstream_error,
                ..
            } => Self::exhausted_metadata(ExhaustedAccountKind::Banned, *count, upstream_error),
            Self::CloudflareChallenge {
                count,
                upstream_error,
            } => Self::exhausted_metadata(
                ExhaustedAccountKind::CloudflareChallenge,
                *count,
                upstream_error,
            ),
            Self::CloudflarePathBlocked {
                count,
                upstream_error,
            } => Self::exhausted_metadata(
                ExhaustedAccountKind::CloudflarePathBlocked,
                *count,
                upstream_error,
            ),
            Self::ModelUnsupported {
                count,
                upstream_error,
            } => Self::exhausted_metadata(
                ExhaustedAccountKind::ModelUnsupported,
                *count,
                upstream_error,
            ),
            Self::UpstreamUnavailable {
                count,
                upstream_error,
            } => Self::exhausted_metadata(
                ExhaustedAccountKind::UpstreamUnavailable,
                *count,
                upstream_error,
            ),
            Self::ContinuationBusy => {
                DispatchErrorMetadata::simple(DispatchFailureClass::ContinuationBusy)
            }
            Self::HistoryUnavailable { upstream_error } => DispatchErrorMetadata {
                failure_class: DispatchFailureClass::HistoryUnavailable,
                exhausted_count: None,
                upstream_error: Some(upstream_error.clone()),
                upstream_status: Some(400),
                diagnostics: None,
            },
            Self::Upstream(error) => DispatchErrorMetadata::upstream(error),
            Self::InvalidSse(_) => DispatchErrorMetadata::simple(DispatchFailureClass::InvalidSse),
            Self::MissingCompleted => {
                DispatchErrorMetadata::simple(DispatchFailureClass::MissingCompleted)
            }
            Self::EmptyUpstreamResponse => {
                DispatchErrorMetadata::simple(DispatchFailureClass::EmptyUpstreamResponse)
            }
            Self::Failed(failure) => DispatchErrorMetadata {
                failure_class: DispatchFailureClass::ResponseFailed,
                exhausted_count: None,
                upstream_error: Some(sse_failure_error_body(failure)),
                upstream_status: None,
                diagnostics: None,
            },
        }
    }

    pub(crate) fn exhausted_account(&self) -> Option<ExhaustedAccountRef<'_>> {
        match self {
            Self::QuotaExhausted {
                count,
                upstream_error,
            } => Some(ExhaustedAccountRef {
                kind: ExhaustedAccountKind::QuotaExhausted,
                count: *count,
                upstream_error,
            }),
            Self::RateLimited {
                count,
                upstream_error,
            } => Some(ExhaustedAccountRef {
                kind: ExhaustedAccountKind::RateLimited,
                count: *count,
                upstream_error,
            }),
            Self::Expired {
                count,
                upstream_error,
            } => Some(ExhaustedAccountRef {
                kind: ExhaustedAccountKind::Expired,
                count: *count,
                upstream_error,
            }),
            Self::Disabled {
                count,
                upstream_error,
            } => Some(ExhaustedAccountRef {
                kind: ExhaustedAccountKind::Disabled,
                count: *count,
                upstream_error,
            }),
            Self::Banned {
                count,
                upstream_error,
                ..
            } => Some(ExhaustedAccountRef {
                kind: ExhaustedAccountKind::Banned,
                count: *count,
                upstream_error,
            }),
            Self::CloudflareChallenge {
                count,
                upstream_error,
            } => Some(ExhaustedAccountRef {
                kind: ExhaustedAccountKind::CloudflareChallenge,
                count: *count,
                upstream_error,
            }),
            Self::CloudflarePathBlocked {
                count,
                upstream_error,
            } => Some(ExhaustedAccountRef {
                kind: ExhaustedAccountKind::CloudflarePathBlocked,
                count: *count,
                upstream_error,
            }),
            Self::ModelUnsupported {
                count,
                upstream_error,
            } => Some(ExhaustedAccountRef {
                kind: ExhaustedAccountKind::ModelUnsupported,
                count: *count,
                upstream_error,
            }),
            Self::UpstreamUnavailable {
                count,
                upstream_error,
            } => Some(ExhaustedAccountRef {
                kind: ExhaustedAccountKind::UpstreamUnavailable,
                count: *count,
                upstream_error,
            }),
            _ => None,
        }
    }

    fn exhausted_metadata(
        kind: ExhaustedAccountKind,
        count: usize,
        upstream_error: &str,
    ) -> DispatchErrorMetadata {
        DispatchErrorMetadata::exhausted_ref(ExhaustedAccountRef {
            kind,
            count,
            upstream_error,
        })
    }
}

pub(crate) fn client_upstream_http_status_code(status: u16) -> u16 {
    match status {
        400..=499 | 503 => status,
        _ => 502,
    }
}

/// Responses live SSE body stream error.
#[derive(Debug, Error)]
pub enum ResponseDispatchStreamError {
    #[error("upstream stream failed: {0}")]
    Upstream(#[from] CodexClientError),
}

pub(super) fn dispatch_error_metadata(
    error: impl std::fmt::Display,
    stream: bool,
    compact: bool,
    transport: Option<&str>,
) -> Value {
    let mut metadata = serde_json::json!({
        "stream": stream,
        "failed": true,
        "errorKind": "dispatch",
        "error": error.to_string(),
    });
    let Some(object) = metadata.as_object_mut() else {
        return metadata;
    };
    if compact {
        object.insert("compact".to_string(), Value::Bool(true));
    }
    if let Some(transport) = transport {
        object.insert(
            "transport".to_string(),
            Value::String(transport.to_string()),
        );
    }
    metadata
}

pub(super) fn enrich_response_dispatch_error_metadata(
    metadata: &mut Value,
    error: &ResponseDispatchError,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    insert_dispatch_error_metadata(object, error.metadata());
}
