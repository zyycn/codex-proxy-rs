//! 可换号上游失败的账号风险剥离。

use chrono::Utc;

use crate::{
    dispatch::{
        errors::{
            is_model_unsupported_upstream_error, is_quota_exhausted_upstream_error,
            is_rate_limit_upstream_error, rate_limit_cooldown_until, upstream_error_body,
            upstream_error_http_status,
        },
        recovery::{
            auth::{auth_failure_account_status, is_auth_upstream_error},
            cloudflare::{
                cloudflare_challenge_error_message, cloudflare_path_block_error_message,
                is_cloudflare_challenge_upstream_error, is_cloudflare_path_block_upstream_error,
                CloudflareRecovery,
            },
            exhaustion::AccountExhaustionTracker,
        },
        stream::sse_failure::{
            auth_sse_failure_account_status, is_auth_sse_failure, is_model_unsupported_sse_failure,
            is_quota_exhausted_sse_failure, sse_failure_error_body, stream_failure_http_status,
        },
    },
    fleet::{account::AccountStatus, pool::AccountPoolService},
    upstream::openai::{
        protocol::responses::ResponsesSseFailure,
        transport::{is_banned_upstream_error, CodexClientError},
    },
};

/// 处理需要隔离当前账号并交回调度器选择下一候选的上游失败。
///
/// 本函数不选择下一个账号，也不实现固定轮换策略；调用方继续使用请求级候选账本，
/// 因而始终服从当前运行时调度策略。
pub(crate) async fn isolate_rotatable_account_failure(
    account_pool: &AccountPoolService,
    cloudflare: &CloudflareRecovery,
    exhausted_accounts: &mut AccountExhaustionTracker,
    account_id: &str,
    error: &CodexClientError,
) -> bool {
    if is_rate_limit_upstream_error(error) {
        exhausted_accounts.record_rate_limited(Some(account_id), upstream_error_body(error));
        account_pool
            .mark_quota_limited_until(account_id, rate_limit_cooldown_until(error, Utc::now()))
            .await;
        return true;
    }

    if is_quota_exhausted_upstream_error(error) {
        exhausted_accounts.record_quota_exhausted(Some(account_id), upstream_error_body(error));
        account_pool
            .set_status(account_id, AccountStatus::QuotaExhausted)
            .await;
        return true;
    }

    if is_auth_upstream_error(error) {
        let account_status = auth_failure_account_status(error);
        exhausted_accounts.record_auth_failure(
            Some(account_id),
            account_status,
            upstream_error_body(error),
            Some(upstream_error_http_status(error)),
        );
        account_pool.set_status(account_id, account_status).await;
        return true;
    }

    if is_cloudflare_challenge_upstream_error(error) {
        exhausted_accounts
            .record_cloudflare_challenge(Some(account_id), cloudflare_challenge_error_message());
        cloudflare.apply_challenge(account_pool, account_id).await;
        return true;
    }

    if is_cloudflare_path_block_upstream_error(error) {
        exhausted_accounts.record_cloudflare_path_blocked(
            Some(account_id),
            cloudflare_path_block_error_message(),
        );
        cloudflare.apply_path_block(account_pool, account_id).await;
        return true;
    }

    if is_model_unsupported_upstream_error(error) {
        exhausted_accounts.record_model_unsupported(Some(account_id), upstream_error_body(error));
        return true;
    }

    if is_banned_upstream_error(error) {
        exhausted_accounts.record_auth_failure(
            Some(account_id),
            AccountStatus::Banned,
            upstream_error_body(error),
            Some(upstream_error_http_status(error)),
        );
        account_pool
            .set_status(account_id, AccountStatus::Banned)
            .await;
        return true;
    }

    false
}

/// 处理 SSE `response.failed` 中需要隔离当前账号的失败。
pub(crate) async fn isolate_sse_account_failure(
    account_pool: &AccountPoolService,
    exhausted_accounts: &mut AccountExhaustionTracker,
    account_id: &str,
    failure: &ResponsesSseFailure,
) -> bool {
    if is_model_unsupported_sse_failure(failure) {
        exhausted_accounts
            .record_model_unsupported(Some(account_id), sse_failure_error_body(failure));
        return true;
    }

    if is_quota_exhausted_sse_failure(failure) {
        exhausted_accounts.record_quota_exhausted(Some(account_id), failure.message.clone());
        account_pool
            .set_status(account_id, AccountStatus::QuotaExhausted)
            .await;
        return true;
    }

    if is_auth_sse_failure(failure) {
        let account_status = auth_sse_failure_account_status(failure);
        exhausted_accounts.record_auth_failure(
            Some(account_id),
            account_status,
            sse_failure_error_body(failure),
            Some(stream_failure_http_status(failure)),
        );
        account_pool.set_status(account_id, account_status).await;
        return true;
    }

    false
}
