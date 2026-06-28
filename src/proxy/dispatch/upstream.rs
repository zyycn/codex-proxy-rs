//! 调度层共享的账号级 Codex 上游调用。

use serde_json::Value;

use crate::{
    proxy::dispatch::{
        cloudflare::CloudflareRecovery, errors::is_retryable_upstream_5xx_error,
        session_affinity::build_conversation_identity,
    },
    upstream::accounts::{
        model::Account,
        pool::{AcquiredAccount, RuntimeAccountPoolService},
        quota::{
            quota_from_usage, quota_snapshot_limit_reached, quota_snapshot_limit_window_seconds,
            quota_snapshot_reset_at,
        },
    },
    upstream::{
        protocol::responses::{CodexCompactRequest, CodexResponsesRequest},
        transport::{
            CodexBackendClient, CodexBackendResponse, CodexBackendStreamingResponse,
            CodexClientError, CodexCompactResponse, CodexRequestContext,
        },
    },
};

const MAX_QUOTA_VERIFY_ATTEMPTS: usize = 5;

pub(crate) const QUOTA_VERIFY_LIMIT_REACHED_MESSAGE: &str =
    "Upstream usage quota still reports limit_reached";

pub(crate) enum QuotaVerificationDecision {
    Ready(Box<AcquiredAccount>),
    RetryWithAnotherAccount,
    MaxAttemptsReached,
}

pub(crate) struct QuotaVerificationContext<'a> {
    pub account_pool: &'a RuntimeAccountPoolService,
    pub codex: &'a CodexBackendClient,
    pub cloudflare: &'a CloudflareRecovery,
    pub installation_id: Option<&'a str>,
    pub request_id: &'a str,
    pub excluded_account_ids: &'a mut Vec<String>,
    pub verify_attempts: &'a mut usize,
}

pub(crate) async fn verify_acquired_quota_if_required(
    context: QuotaVerificationContext<'_>,
    acquired: AcquiredAccount,
) -> QuotaVerificationDecision {
    if !acquired.account.quota_verify_required {
        return QuotaVerificationDecision::Ready(Box::new(acquired));
    }

    let account_id = acquired.account.id.clone();
    let cookie_header = context
        .cloudflare
        .cookie_header_for_request(&account_id, "/codex/usage")
        .await;
    let usage = context
        .codex
        .fetch_usage(CodexRequestContext {
            access_token: &acquired.account.access_token,
            account_id: acquired.account.account_id.as_deref(),
            request_id: context.request_id,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: cookie_header.as_deref(),
            installation_id: context.installation_id,
            session_id: None,
        })
        .await;

    let raw = match usage {
        Ok(raw) => raw,
        Err(error) => {
            tracing::warn!(
                account_id = %account_id,
                error = %error,
                "failed to verify stale quota state before upstream request"
            );
            return QuotaVerificationDecision::Ready(Box::new(acquired));
        }
    };

    let quota = quota_from_usage(&raw);
    context
        .account_pool
        .apply_quota_snapshot(&account_id, &quota)
        .await;
    if quota_snapshot_limit_reached(&quota) {
        context.account_pool.release(&account_id).await;
        context.excluded_account_ids.push(account_id);
        *context.verify_attempts += 1;
        if *context.verify_attempts >= MAX_QUOTA_VERIFY_ATTEMPTS {
            return QuotaVerificationDecision::MaxAttemptsReached;
        }
        return QuotaVerificationDecision::RetryWithAnotherAccount;
    }

    QuotaVerificationDecision::Ready(Box::new(acquired_with_verified_quota(acquired, &quota)))
}

fn acquired_with_verified_quota(mut acquired: AcquiredAccount, quota: &Value) -> AcquiredAccount {
    let limit_reached = quota_snapshot_limit_reached(quota);
    acquired.account.quota_verify_required = false;
    acquired.account.quota_limit_reached = limit_reached;
    acquired.account.quota_cooldown_until = limit_reached
        .then_some(quota_snapshot_reset_at(quota))
        .flatten();
    if let Some(reset_at) = quota_snapshot_reset_at(quota) {
        acquired.account.window_reset_at = Some(reset_at);
        if let Some(limit_window_seconds) = quota_snapshot_limit_window_seconds(quota) {
            acquired.account.limit_window_seconds = Some(limit_window_seconds);
        }
    }
    acquired
}

pub(crate) async fn create_response_with_account(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexBackendResponse, CodexClientError> {
    let cookie_header = cloudflare
        .cookie_header_for_request(&account.id, "/codex/responses")
        .await;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        &account.id,
    );
    codex
        .create_response(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
                installation_id,
                session_id: identity.conversation_id.as_deref(),
            },
        )
        .await
}

async fn create_response_stream_with_account(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexBackendStreamingResponse, CodexClientError> {
    let cookie_header = cloudflare
        .cookie_header_for_request(&account.id, "/codex/responses")
        .await;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        &account.id,
    );
    codex
        .create_response_stream(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
                installation_id,
                session_id: identity.conversation_id.as_deref(),
            },
        )
        .await
}

async fn create_compact_response_with_account(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexCompactRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexCompactResponse, CodexClientError> {
    let cookie_header = cloudflare
        .cookie_header_for_request(&account.id, "/codex/responses/compact")
        .await;
    codex
        .create_compact_response(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: cookie_header.as_deref(),
                installation_id,
                session_id: None,
            },
        )
        .await
}

const MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT: usize = 2;

pub(crate) async fn create_response_with_account_retrying_5xx(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexBackendResponse, CodexClientError> {
    let mut retries = 0;
    loop {
        let result = create_response_with_account(
            codex,
            installation_id,
            cloudflare,
            request,
            request_id,
            account,
        )
        .await;
        match result {
            Err(error)
                if is_retryable_upstream_5xx_error(&error)
                    && retries < MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT =>
            {
                retries += 1;
            }
            result => return result,
        }
    }
}

pub(crate) async fn create_response_stream_with_account_retrying_5xx(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexBackendStreamingResponse, CodexClientError> {
    let mut retries = 0;
    loop {
        let result = create_response_stream_with_account(
            codex,
            installation_id,
            cloudflare,
            request,
            request_id,
            account,
        )
        .await;
        match result {
            Err(error)
                if is_retryable_upstream_5xx_error(&error)
                    && retries < MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT =>
            {
                retries += 1;
            }
            result => return result,
        }
    }
}

pub(crate) async fn create_compact_response_with_account_retrying_5xx(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexCompactRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexCompactResponse, CodexClientError> {
    let mut retries = 0;
    loop {
        let result = create_compact_response_with_account(
            codex,
            installation_id,
            cloudflare,
            request,
            request_id,
            account,
        )
        .await;
        match result {
            Err(error)
                if is_retryable_upstream_5xx_error(&error)
                    && retries < MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT =>
            {
                retries += 1;
            }
            result => return result,
        }
    }
}
