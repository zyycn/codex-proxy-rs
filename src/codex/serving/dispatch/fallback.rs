use axum::http::StatusCode;
use chrono::{Duration, Utc};
use serde_json::{json, Value};

use crate::codex::{
    accounts::{
        model::{Account, AccountStatus},
        pool::AccountAcquireRequest,
    },
    gateway::transport::{http_client::CodexClientError, rate_limits::cooldown_with_jitter},
};

use super::{usage::record_request_attempt, CodexUpstreamDependencies};

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

pub(super) async fn apply_upstream_retry_and_acquire_fallback_with_deps(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    retry: UpstreamAccountRetry,
    model: &str,
    excluded_account_ids: &mut Vec<String>,
) -> Option<Account> {
    apply_upstream_account_retry_with_deps(deps, account, retry).await;
    excluded_account_ids.push(account.id.clone());
    deps.account_pool
        .lock()
        .await
        .acquire_with(
            AccountAcquireRequest::new(model, Utc::now())
                .with_exclude_account_ids(excluded_account_ids.iter().cloned()),
        )
        .map(|fallback| fallback.account)
}

pub(super) async fn apply_upstream_account_retry_with_deps(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    retry: UpstreamAccountRetry,
) {
    match retry {
        UpstreamAccountRetry::RateLimited {
            retry_after_seconds,
        } => {
            let cooldown_until = Utc::now() + cooldown_with_jitter(retry_after_seconds, 2_000);
            if let Some(repo) = deps.account_repository.as_ref() {
                if let Err(error) = repo
                    .set_quota_cooldown_until(&account.id, cooldown_until)
                    .await
                {
                    tracing::warn!(
                        error = %error,
                        account_id = %account.id,
                        cooldown_until = %cooldown_until,
                        "持久化 quota cooldown 失败"
                    );
                }
            }
            deps.account_pool
                .lock()
                .await
                .mark_quota_limited_until(&account.id, cooldown_until);
            if let Err(error) = record_request_attempt(deps, &account.id).await {
                tracing::warn!(
                    error = ?error,
                    account_id = %account.id,
                    "记录被 rate limit 的账户请求尝试失败"
                );
            }
        }
        UpstreamAccountRetry::QuotaExhausted => {
            set_account_status(deps, account, AccountStatus::QuotaExhausted).await;
        }
        UpstreamAccountRetry::CloudflareChallenge { cooldown_seconds } => {
            let cooldown_until = Utc::now() + Duration::seconds(cooldown_seconds as i64);
            if let Some(cookie_repo) = deps.cookie_repository.as_ref() {
                if let Err(error) = cookie_repo.delete_account_cookies(&account.id).await {
                    tracing::warn!(
                        error = %error,
                        account_id = %account.id,
                        "清理 Cloudflare 阻断账户 cookies 失败"
                    );
                }
            }
            if let Some(repo) = deps.account_repository.as_ref() {
                if let Err(error) = repo
                    .set_cloudflare_cooldown_until(&account.id, cooldown_until)
                    .await
                {
                    tracing::warn!(
                        error = %error,
                        account_id = %account.id,
                        cooldown_until = %cooldown_until,
                        "持久化 Cloudflare cooldown 失败"
                    );
                }
            }
            deps.account_pool
                .lock()
                .await
                .set_cloudflare_cooldown_until(&account.id, cooldown_until);
        }
        UpstreamAccountRetry::Banned => {
            set_account_status(deps, account, AccountStatus::Banned).await;
        }
    }
}

async fn set_account_status(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    status: AccountStatus,
) {
    if let Some(repo) = deps.account_repository.as_ref() {
        if let Err(error) = repo.set_status(&account.id, status).await {
            tracing::warn!(
                error = %error,
                account_id = %account.id,
                status = ?status,
                "持久化上游账户状态失败"
            );
        }
    }
    deps.account_pool
        .lock()
        .await
        .set_status(&account.id, status);
    deps.websocket_pool.evict_account(&account.id).await;
}
