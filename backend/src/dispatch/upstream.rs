//! 调度层共享的账号级 Codex 上游调用。

use std::{future::Future, time::Instant};

use serde_json::Value;

use crate::{
    accounts::{
        account::Account,
        pool::{AcquiredAccount, RuntimeAccountPoolService},
        quota::{
            quota_from_usage, quota_snapshot_limit_reached, quota_snapshot_limit_window_seconds,
            quota_snapshot_reset_at,
        },
    },
    dispatch::{
        affinity::build_conversation_identity, cloudflare::CloudflareRecovery,
        errors::is_retryable_upstream_5xx_error,
    },
    upstream::openai::{
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
    RequiredAccountUnavailable,
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
    pub allow_retry_with_another_account: bool,
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
                request_id = %context.request_id,
                account_id = %account_id,
                quota_verify_required = true,
                quota_verify_result = "upstream_error",
                retry_with_another_account = false,
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
        context
            .account_pool
            .release_without_request_usage(&account_id)
            .await;
        context.excluded_account_ids.push(account_id.clone());
        *context.verify_attempts += 1;
        let max_attempts_reached = *context.verify_attempts >= MAX_QUOTA_VERIFY_ATTEMPTS;
        tracing::info!(
            request_id = %context.request_id,
            account_id = %account_id,
            quota_verify_required = true,
            quota_verify_result = "limit_reached",
            verify_attempts = *context.verify_attempts,
            retry_with_another_account = context.allow_retry_with_another_account && !max_attempts_reached,
            "quota verification reported exhausted account before upstream request"
        );
        if !context.allow_retry_with_another_account {
            return QuotaVerificationDecision::RequiredAccountUnavailable;
        }
        if max_attempts_reached {
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
    started_at: Instant,
) -> Result<CodexBackendResponse, CodexClientError> {
    let cookie_header = cloudflare
        .cookie_header_for_request(&account.id, "/codex/responses")
        .await;
    let identity = build_conversation_identity(
        request.prompt_cache_key(),
        request.codex_window_id.as_deref(),
        &account.id,
    );
    codex
        .create_response_with_pool_account_started_at(
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
            Some(&account.id),
            started_at,
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
        request.prompt_cache_key(),
        request.codex_window_id.as_deref(),
        &account.id,
    );
    codex
        .create_response_stream_with_pool_account(
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
            Some(&account.id),
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

async fn retry_upstream_5xx<T, F, Fut>(
    request_id: &str,
    account_id: &str,
    endpoint: Option<&str>,
    retry_message: &'static str,
    failure_message: &'static str,
    mut operation: F,
) -> Result<T, CodexClientError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, CodexClientError>>,
{
    let mut retries = 0;
    loop {
        let result = operation().await;
        match result {
            Err(error)
                if is_retryable_upstream_5xx_error(&error)
                    && retries < MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT =>
            {
                log_upstream_retry(
                    request_id,
                    account_id,
                    endpoint,
                    retries + 1,
                    &error,
                    retry_message,
                );
                retries += 1;
            }
            Ok(response) => return Ok(response),
            Err(error) => {
                log_upstream_failure(
                    request_id,
                    account_id,
                    endpoint,
                    retries,
                    &error,
                    failure_message,
                );
                return Err(error);
            }
        }
    }
}

fn log_upstream_retry(
    request_id: &str,
    account_id: &str,
    endpoint: Option<&str>,
    retry: usize,
    error: &CodexClientError,
    message: &'static str,
) {
    if let Some(endpoint) = endpoint {
        tracing::warn!(
            request_id,
            account_id = %account_id,
            endpoint,
            retry,
            error = %error,
            message
        );
    } else {
        tracing::warn!(
            request_id,
            account_id = %account_id,
            retry,
            error = %error,
            message
        );
    }
}

fn log_upstream_failure(
    request_id: &str,
    account_id: &str,
    endpoint: Option<&str>,
    retries: usize,
    error: &CodexClientError,
    message: &'static str,
) {
    if let Some(endpoint) = endpoint {
        tracing::warn!(
            request_id,
            account_id = %account_id,
            endpoint,
            retries,
            error = %error,
            message
        );
    } else {
        tracing::warn!(
            request_id,
            account_id = %account_id,
            retries,
            error = %error,
            message
        );
    }
}

pub(crate) async fn create_response_with_account_retrying_5xx(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
    started_at: Instant,
) -> Result<CodexBackendResponse, CodexClientError> {
    retry_upstream_5xx(
        request_id,
        &account.id,
        None,
        "upstream response request failed with retryable 5xx",
        "upstream response request failed",
        || {
            create_response_with_account(
                codex,
                installation_id,
                cloudflare,
                request,
                request_id,
                account,
                started_at,
            )
        },
    )
    .await
}

pub(crate) async fn create_response_stream_with_account_retrying_5xx(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexBackendStreamingResponse, CodexClientError> {
    retry_upstream_5xx(
        request_id,
        &account.id,
        None,
        "upstream response stream request failed with retryable 5xx",
        "upstream response stream request failed",
        || {
            create_response_stream_with_account(
                codex,
                installation_id,
                cloudflare,
                request,
                request_id,
                account,
            )
        },
    )
    .await
}

pub(crate) async fn create_compact_response_with_account_retrying_5xx(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexCompactRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexCompactResponse, CodexClientError> {
    retry_upstream_5xx(
        request_id,
        &account.id,
        Some("compact"),
        "upstream compact request failed with retryable 5xx",
        "upstream compact request failed",
        || {
            create_compact_response_with_account(
                codex,
                installation_id,
                cloudflare,
                request,
                request_id,
                account,
            )
        },
    )
    .await
}
