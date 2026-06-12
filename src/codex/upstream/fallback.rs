use axum::http::StatusCode;
use serde_json::{json, Value};

use crate::codex::transport::client::CodexClientError;

const DEFAULT_RATE_LIMIT_BACKOFF_SECONDS: u64 = 60;
const MAX_RATE_LIMIT_BACKOFF_SECONDS: u64 = 86_400 * 7;
const CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS: u64 = 10;

#[derive(Debug, Clone, Copy)]
pub(crate) enum UpstreamAccountRetry {
    RateLimited { retry_after_seconds: u64 },
    QuotaExhausted,
    CloudflareChallenge { cooldown_seconds: u64 },
    Banned,
}

impl UpstreamAccountRetry {
    pub(crate) fn status(self) -> StatusCode {
        match self {
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::QuotaExhausted => StatusCode::PAYMENT_REQUIRED,
            Self::CloudflareChallenge { .. } => StatusCode::FORBIDDEN,
            Self::Banned => StatusCode::FORBIDDEN,
        }
    }

    pub(crate) fn metadata(self, stream: bool) -> Value {
        match self {
            Self::RateLimited {
                retry_after_seconds,
            } => json!({
                "stream": stream,
                "retry": true,
                "reason": "rateLimited",
                "retryAfterSeconds": retry_after_seconds,
            }),
            Self::QuotaExhausted => json!({
                "stream": stream,
                "retry": true,
                "reason": "quotaExhausted",
            }),
            Self::CloudflareChallenge { cooldown_seconds } => json!({
                "stream": stream,
                "retry": true,
                "reason": "cloudflareChallenge",
                "cooldownSeconds": cooldown_seconds,
            }),
            Self::Banned => json!({
                "stream": stream,
                "retry": true,
                "reason": "banned",
            }),
        }
    }
}

pub(crate) fn classify_upstream_account_retry(
    error: &CodexClientError,
) -> Option<UpstreamAccountRetry> {
    match error {
        CodexClientError::Upstream {
            status,
            retry_after_seconds,
            ..
        } if *status == StatusCode::TOO_MANY_REQUESTS => Some(UpstreamAccountRetry::RateLimited {
            retry_after_seconds: retry_after_seconds
                .unwrap_or(DEFAULT_RATE_LIMIT_BACKOFF_SECONDS)
                .min(MAX_RATE_LIMIT_BACKOFF_SECONDS),
        }),
        CodexClientError::Upstream { status, .. } if *status == StatusCode::PAYMENT_REQUIRED => {
            Some(UpstreamAccountRetry::QuotaExhausted)
        }
        CodexClientError::Upstream { status, body, .. } if *status == StatusCode::FORBIDDEN => {
            if is_cloudflare_challenge(body) {
                Some(UpstreamAccountRetry::CloudflareChallenge {
                    cooldown_seconds: CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS,
                })
            } else {
                Some(UpstreamAccountRetry::Banned)
            }
        }
        _ => None,
    }
}

pub(crate) fn websocket_history_retry_metadata(retry: UpstreamAccountRetry, stream: bool) -> Value {
    match retry {
        UpstreamAccountRetry::RateLimited {
            retry_after_seconds,
        } => json!({
            "stream": stream,
            "transport": "websocket",
            "retry": false,
            "reason": "rateLimited",
            "retryAfterSeconds": retry_after_seconds,
            "accountAffinity": "previousResponseId",
        }),
        UpstreamAccountRetry::QuotaExhausted => json!({
            "stream": stream,
            "transport": "websocket",
            "retry": false,
            "reason": "quotaExhausted",
            "accountAffinity": "previousResponseId",
        }),
        UpstreamAccountRetry::CloudflareChallenge { cooldown_seconds } => json!({
            "stream": stream,
            "transport": "websocket",
            "retry": false,
            "reason": "cloudflareChallenge",
            "cooldownSeconds": cooldown_seconds,
            "accountAffinity": "previousResponseId",
        }),
        UpstreamAccountRetry::Banned => json!({
            "stream": stream,
            "transport": "websocket",
            "retry": false,
            "reason": "banned",
            "accountAffinity": "previousResponseId",
        }),
    }
}

fn is_cloudflare_challenge(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("cf-mitigated")
        || lower.contains("cf-chl-bypass")
        || lower.contains("_cf_chl")
        || lower.contains("cf_chl")
        || lower.contains("attention required")
        || lower.contains("just a moment")
}
