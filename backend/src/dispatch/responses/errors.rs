use serde_json::Value;
use thiserror::Error;

use crate::{
    dispatch::{
        errors::{
            insert_dispatch_error_metadata, upstream_error_http_status, DispatchErrorMetadata,
            DispatchFailureClass,
        },
        exhaustion::{ExhaustedAccount, ExhaustedAccountKind, ExhaustedAccountRef},
    },
    upstream::openai::{
        protocol::{responses::ResponsesSseFailure, sse::SseError},
        transport::CodexClientError,
    },
};

use super::sse_failure::{sse_failure_error_body, stream_failure_http_status};

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
            Self::Failed(failure) => stream_failure_http_status(failure),
            Self::ModelUnsupported { .. } | Self::HistoryUnavailable { .. } => 400,
            Self::Upstream(error) => upstream_error_http_status(error),
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
