use chrono::Utc;
use serde_json::Value;

use crate::codex::gateway::transport::rate_limits::{parse_rate_limit_headers, rate_limit_quota};

use super::CodexUpstreamDependencies;

pub(super) async fn apply_rate_limit_headers_with_deps(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
    plan_type: Option<&str>,
    rate_limit_headers: &[(String, String)],
) {
    let Some(rate_limits) = parse_rate_limit_headers(rate_limit_headers) else {
        return;
    };

    let existing_quota = existing_quota_json(deps, account_id).await;
    let quota = rate_limit_quota(&rate_limits, plan_type, existing_quota.as_ref());
    if let Some(repo) = deps.account_repository.as_ref() {
        if let Err(error) = repo.update_quota_json(account_id, &quota.to_string()).await {
            tracing::warn!(
                error = %error,
                account_id = %account_id,
                "被动同步 quota 缓存失败"
            );
        }
    }

    let Some(reset_at) = rate_limits.primary_reset_at() else {
        return;
    };
    deps.account_pool.lock().await.sync_rate_limit_window(
        account_id,
        reset_at,
        rate_limits.primary_limit_window_seconds(),
    );
    if let Some(repo) = deps.account_repository.as_ref() {
        if let Err(error) = repo
            .sync_rate_limit_window(
                account_id,
                reset_at,
                rate_limits.primary_limit_window_seconds(),
            )
            .await
        {
            tracing::warn!(
                error = %error,
                account_id = %account_id,
                window_reset_at = %reset_at,
                "持久化 rate-limit window 失败"
            );
        }
    }
    if !rate_limits.primary_limit_reached() || reset_at <= Utc::now() {
        return;
    }

    if let Some(repo) = deps.account_repository.as_ref() {
        if let Err(error) = repo.set_quota_cooldown_until(account_id, reset_at).await {
            tracing::warn!(
                error = %error,
                account_id = %account_id,
                cooldown_until = %reset_at,
                "持久化被动 quota cooldown 失败"
            );
        }
    }
    deps.account_pool
        .lock()
        .await
        .mark_quota_limited_until(account_id, reset_at);
    deps.websocket_pool.evict_account(account_id).await;
}

async fn existing_quota_json(deps: &CodexUpstreamDependencies, account_id: &str) -> Option<Value> {
    let repo = deps.account_repository.as_ref()?;
    let raw = repo.get_quota_json(account_id).await.ok().flatten()?;
    serde_json::from_str(&raw).ok()
}
