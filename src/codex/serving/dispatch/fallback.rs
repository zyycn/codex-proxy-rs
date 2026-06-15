use axum::http::StatusCode;
use chrono::{Duration, Utc};
use serde_json::{json, Value};

use crate::codex::{
    accounts::{
        model::{Account, AccountStatus},
        pool::{AccountAcquireRequest, AccountPoolStatusSummary, AcquiredAccount},
    },
    gateway::transport::{http_client::CodexClientError, rate_limits::cooldown_with_jitter},
};

use super::{usage::record_request_attempt, CodexUpstreamDependencies};

const DEFAULT_RATE_LIMIT_BACKOFF_SECONDS: u64 = 60;
const MAX_RATE_LIMIT_BACKOFF_SECONDS: u64 = 86_400 * 7;
const CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS: u64 = 10;

#[derive(Debug, Clone, Copy)]
pub(crate) enum UpstreamAccountRetry {
    ModelUnsupported { status: StatusCode },
    RateLimited { retry_after_seconds: u64 },
    QuotaExhausted,
    CloudflareChallenge { cooldown_seconds: u64 },
    CloudflarePathBlock,
    TokenInvalid { account_status: AccountStatus },
    Banned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpstreamRequestRecovery {
    StripPreviousResponse { reason: HistoryRecoveryReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryRecoveryReason {
    PreviousResponseNotFound,
    UnansweredFunctionCall,
}

impl UpstreamAccountRetry {
    pub(crate) fn status(self) -> StatusCode {
        match self {
            Self::ModelUnsupported { status } => status,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::QuotaExhausted => StatusCode::PAYMENT_REQUIRED,
            Self::CloudflareChallenge { .. } => StatusCode::BAD_GATEWAY,
            Self::CloudflarePathBlock => StatusCode::BAD_GATEWAY,
            Self::TokenInvalid { .. } => StatusCode::UNAUTHORIZED,
            Self::Banned => StatusCode::FORBIDDEN,
        }
    }

    pub(crate) fn metadata(self, stream: bool) -> Value {
        match self {
            Self::ModelUnsupported { .. } => json!({
                "stream": stream,
                "retry": true,
                "reason": "modelUnsupported",
            }),
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
            Self::CloudflarePathBlock => json!({
                "stream": stream,
                "retry": true,
                "reason": "cloudflarePathBlock",
            }),
            Self::TokenInvalid { account_status } => json!({
                "stream": stream,
                "retry": true,
                "reason": "tokenInvalid",
                "accountStatus": account_status_metadata(account_status),
            }),
            Self::Banned => json!({
                "stream": stream,
                "retry": true,
                "reason": "banned",
            }),
        }
    }

    pub(crate) fn is_model_unsupported(self) -> bool {
        matches!(self, Self::ModelUnsupported { .. })
    }

    pub(crate) fn preserve_history_account_affinity(self) -> bool {
        !self.is_model_unsupported()
    }

    pub(crate) fn fallback_response_message(self, upstream_message: String) -> String {
        match self {
            Self::CloudflareChallenge { .. } => {
                "Upstream blocked the request (Cloudflare challenge)".to_string()
            }
            Self::CloudflarePathBlock => {
                "Upstream blocked the request (Cloudflare path-block)".to_string()
            }
            _ => upstream_message,
        }
    }
}

impl UpstreamRequestRecovery {
    pub(crate) fn metadata(self, stream: bool, stale_response_id: Option<&str>) -> Value {
        match self {
            Self::StripPreviousResponse { reason } => json!({
                "stream": stream,
                "retry": true,
                "reason": reason.as_metadata_value(),
                "previousResponseId": stale_response_id,
            }),
        }
    }
}

impl HistoryRecoveryReason {
    fn as_metadata_value(self) -> &'static str {
        match self {
            Self::PreviousResponseNotFound => "previousResponseNotFound",
            Self::UnansweredFunctionCall => "unansweredFunctionCall",
        }
    }
}

pub(crate) fn classify_upstream_request_recovery(
    error: &CodexClientError,
    recovery_already_used: bool,
) -> Option<UpstreamRequestRecovery> {
    if recovery_already_used {
        return None;
    }
    if is_previous_response_not_found_error(error) {
        return Some(UpstreamRequestRecovery::StripPreviousResponse {
            reason: HistoryRecoveryReason::PreviousResponseNotFound,
        });
    }
    if is_unanswered_function_call_error(error) {
        return Some(UpstreamRequestRecovery::StripPreviousResponse {
            reason: HistoryRecoveryReason::UnansweredFunctionCall,
        });
    }
    None
}

pub(crate) fn classify_upstream_account_retry(
    error: &CodexClientError,
    model_unsupported_retry_used: bool,
) -> Option<UpstreamAccountRetry> {
    match error {
        CodexClientError::Upstream { status, .. }
            if is_model_not_supported_error(error) && !model_unsupported_retry_used =>
        {
            Some(UpstreamAccountRetry::ModelUnsupported { status: *status })
        }
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
        CodexClientError::Upstream { status, body, .. }
            if *status == StatusCode::NOT_FOUND && body.trim().is_empty() =>
        {
            Some(UpstreamAccountRetry::CloudflarePathBlock)
        }
        CodexClientError::Upstream { status, body, .. } if *status == StatusCode::UNAUTHORIZED => {
            Some(UpstreamAccountRetry::TokenInvalid {
                account_status: account_status_for_unauthorized(body),
            })
        }
        _ => None,
    }
}

fn is_model_not_supported_error(error: &CodexClientError) -> bool {
    let CodexClientError::Upstream { status, body, .. } = error else {
        return false;
    };
    if !status.is_client_error() || *status == StatusCode::TOO_MANY_REQUESTS {
        return false;
    }
    let haystack = format!(
        "{} {} {}",
        error_code(body).unwrap_or_default(),
        error_message(body).unwrap_or_default(),
        body
    )
    .to_ascii_lowercase();
    haystack.contains("model")
        && (haystack.contains("not supported")
            || haystack.contains("not_supported")
            || haystack.contains("not available")
            || haystack.contains("not_available"))
}

fn account_status_for_unauthorized(body: &str) -> AccountStatus {
    if body.to_ascii_lowercase().contains("deactivated") {
        AccountStatus::Banned
    } else {
        AccountStatus::Expired
    }
}

fn is_previous_response_not_found_error(error: &CodexClientError) -> bool {
    let CodexClientError::Upstream { body, .. } = error else {
        return false;
    };
    if error_code(body).as_deref() == Some("previous_response_not_found") {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("previous_response_not_found")
        || (lower.contains("previous response with id") && lower.contains("not found"))
}

fn is_unanswered_function_call_error(error: &CodexClientError) -> bool {
    let CodexClientError::Upstream { status, body, .. } = error else {
        return false;
    };
    if *status != StatusCode::BAD_REQUEST {
        return false;
    }
    body.to_ascii_lowercase()
        .contains("no tool output found for function call")
}

fn error_code(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/response/error/type"))
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.pointer("/error/type"))
        .and_then(Value::as_str)
        .map(|code| code.to_ascii_lowercase())
}

fn error_message(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/error/message"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub(crate) fn websocket_history_retry_metadata(retry: UpstreamAccountRetry, stream: bool) -> Value {
    match retry {
        UpstreamAccountRetry::ModelUnsupported { .. } => json!({
            "stream": stream,
            "transport": "websocket",
            "retry": false,
            "reason": "modelUnsupported",
            "accountAffinity": "previousResponseId",
        }),
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
        UpstreamAccountRetry::CloudflarePathBlock => json!({
            "stream": stream,
            "transport": "websocket",
            "retry": false,
            "reason": "cloudflarePathBlock",
            "accountAffinity": "previousResponseId",
        }),
        UpstreamAccountRetry::TokenInvalid { account_status } => json!({
            "stream": stream,
            "transport": "websocket",
            "retry": false,
            "reason": "tokenInvalid",
            "accountStatus": account_status_metadata(account_status),
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

pub(super) fn build_account_exhaustion_detail(
    summary: AccountPoolStatusSummary,
    message: &str,
) -> String {
    let mut parts = Vec::new();
    if summary.rate_limited > 0 {
        parts.push(format!("{} rate-limited", summary.rate_limited));
    }
    if summary.expired > 0 {
        parts.push(format!("{} expired", summary.expired));
    }
    if summary.banned > 0 {
        parts.push(format!("{} banned", summary.banned));
    }
    if summary.disabled > 0 {
        parts.push(format!("{} disabled", summary.disabled));
    }
    if summary.quota_exhausted > 0 {
        parts.push(format!("{} quota-exhausted", summary.quota_exhausted));
    }
    if summary.refreshing > 0 {
        parts.push(format!("{} refreshing", summary.refreshing));
    }
    if parts.is_empty() {
        format!("No accounts available. {message}")
    } else {
        format!("All accounts exhausted ({}). {message}", parts.join(", "))
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
) -> Option<AcquiredAccount> {
    apply_upstream_account_retry_with_deps(deps, account, retry).await;
    excluded_account_ids.push(account.id.clone());
    deps.account_pool.lock().await.acquire_with(
        AccountAcquireRequest::new(model, Utc::now())
            .with_exclude_account_ids(excluded_account_ids.iter().cloned()),
    )
}

pub(super) async fn apply_upstream_account_retry_with_deps(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    retry: UpstreamAccountRetry,
) {
    let evict_websocket_pool = match retry {
        UpstreamAccountRetry::ModelUnsupported { .. } => {
            tracing::warn!(
                account_id = %account.id,
                "上游账号不支持当前 model，将尝试备用账号"
            );
            false
        }
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
            true
        }
        UpstreamAccountRetry::QuotaExhausted => {
            set_account_status(deps, account, AccountStatus::QuotaExhausted).await;
            true
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
            true
        }
        UpstreamAccountRetry::CloudflarePathBlock => {
            if let Some(cookie_repo) = deps.cookie_repository.as_ref() {
                if let Err(error) = cookie_repo.delete_account_cookies(&account.id).await {
                    tracing::warn!(
                        error = %error,
                        account_id = %account.id,
                        "清理 Cloudflare path-block 账户 cookies 失败"
                    );
                }
            }
            let block_count = deps
                .cf_path_block_tracker
                .record_path_block(&account.id)
                .await;
            if deps.cf_path_block_tracker.should_disable(&account.id).await {
                set_account_status(deps, account, AccountStatus::Disabled).await;
            }
            tracing::warn!(
                account_id = %account.id,
                block_count,
                "上游返回 Cloudflare path-block 404，已清理 cookies"
            );
            true
        }
        UpstreamAccountRetry::TokenInvalid { account_status } => {
            set_account_status(deps, account, account_status).await;
            true
        }
        UpstreamAccountRetry::Banned => {
            set_account_status(deps, account, AccountStatus::Banned).await;
            true
        }
    };
    if evict_websocket_pool {
        deps.websocket_pool.evict_account(&account.id).await;
    }
}

fn account_status_metadata(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Active => "active",
        AccountStatus::Expired => "expired",
        AccountStatus::QuotaExhausted => "quota_exhausted",
        AccountStatus::Refreshing => "refreshing",
        AccountStatus::Disabled => "disabled",
        AccountStatus::Banned => "banned",
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
}
