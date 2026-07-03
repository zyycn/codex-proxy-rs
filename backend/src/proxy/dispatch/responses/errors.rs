use serde_json::{json, Value};
use thiserror::Error;

use crate::{
    proxy::dispatch::errors::{upstream_error_body, upstream_error_http_status},
    upstream::{
        protocol::{responses::ResponsesSseFailure, sse::SseError},
        transport::CodexClientError,
    },
};

use super::sse_failure::{sse_failure_error_body, stream_failure_http_status};

/// Responses 调度错误。
#[derive(Debug, Error)]
pub enum ResponseDispatchError {
    #[error("failed to list runtime accounts")]
    AccountStore,
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
    pub fn http_status_code(&self) -> u16 {
        match self {
            Self::NoActiveAccount | Self::AccountStore => 503,
            Self::QuotaExhausted { .. } => 429,
            Self::RateLimited { .. } => 429,
            Self::Expired { .. } | Self::Disabled { .. } => 401,
            Self::Banned { status_code, .. } => *status_code,
            Self::CloudflareChallenge { .. }
            | Self::CloudflarePathBlocked { .. }
            | Self::InvalidSse(_)
            | Self::MissingCompleted
            | Self::EmptyUpstreamResponse => 502,
            Self::Failed(failure) => stream_failure_http_status(failure),
            Self::ModelUnsupported { .. } => 400,
            Self::Upstream(error) => upstream_error_http_status(error),
        }
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
    let (failure_class, exhausted_count, upstream_error, upstream_status) = match error {
        ResponseDispatchError::AccountStore => ("account_store", None, None, None),
        ResponseDispatchError::NoActiveAccount => ("no_available_accounts", None, None, None),
        ResponseDispatchError::QuotaExhausted {
            count,
            upstream_error,
        } => (
            "quota_exhausted",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::RateLimited {
            count,
            upstream_error,
        } => (
            "rate_limited",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::Expired {
            count,
            upstream_error,
        } => ("expired", Some(*count), Some(upstream_error.clone()), None),
        ResponseDispatchError::Disabled {
            count,
            upstream_error,
        } => ("disabled", Some(*count), Some(upstream_error.clone()), None),
        ResponseDispatchError::Banned {
            count,
            upstream_error,
            ..
        } => ("banned", Some(*count), Some(upstream_error.clone()), None),
        ResponseDispatchError::CloudflareChallenge {
            count,
            upstream_error,
        } => (
            "cloudflare_challenge",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::CloudflarePathBlocked {
            count,
            upstream_error,
        } => (
            "cloudflare_path_blocked",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::ModelUnsupported {
            count,
            upstream_error,
        } => (
            "model_unsupported",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::Upstream(error) => {
            let upstream_status = match error {
                CodexClientError::Upstream { status, .. } => Some(status.as_u16()),
                _ => None,
            };
            (
                "upstream",
                None,
                Some(upstream_error_body(error)),
                upstream_status,
            )
        }
        ResponseDispatchError::InvalidSse(_) => ("invalid_sse", None, None, None),
        ResponseDispatchError::MissingCompleted => ("missing_completed", None, None, None),
        ResponseDispatchError::EmptyUpstreamResponse => {
            ("empty_upstream_response", None, None, None)
        }
        ResponseDispatchError::Failed(failure) => (
            "response_failed",
            None,
            Some(sse_failure_error_body(failure)),
            None,
        ),
    };

    object.insert(
        "failureClass".to_string(),
        Value::String(failure_class.to_string()),
    );
    if let Some(count) = exhausted_count {
        object.insert("exhaustedCount".to_string(), json!(count));
    }
    if let Some(error) = upstream_error {
        object.insert("upstreamError".to_string(), Value::String(error));
    }
    if let Some(status) = upstream_status {
        object.insert("upstreamStatus".to_string(), json!(status));
    }
}
