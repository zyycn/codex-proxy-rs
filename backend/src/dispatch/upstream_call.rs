//! 调度层共享的账号级 Codex 上游调用。

use std::{future::Future, time::Instant};

use serde_json::Value;

use crate::{
    dispatch::{
        affinity::AccountIdentityService, errors::is_retryable_upstream_5xx_error,
        recovery::cloudflare::CloudflareRecovery,
    },
    fleet::{
        account::Account,
        pool::{AccountLease, AccountPoolService},
        quota::{
            quota_from_usage, quota_snapshot_limit_reached, quota_snapshot_limit_window_seconds,
            quota_snapshot_reset_at,
        },
    },
    upstream::openai::{
        protocol::responses::CodexResponsesRequest,
        transport::{
            CodexBackendClient, CodexBackendResponse, CodexBackendStreamingResponse,
            CodexClientError, CodexRequestContext,
        },
    },
};

pub(crate) const QUOTA_VERIFY_LIMIT_REACHED_MESSAGE: &str =
    "Upstream usage quota still reports limit_reached";

pub(crate) enum QuotaVerificationDecision {
    Ready(Box<AccountLease>),
    RetryWithAnotherAccount,
}

pub(crate) struct QuotaVerificationContext<'a> {
    pub account_pool: &'a AccountPoolService,
    pub codex: &'a CodexBackendClient,
    pub cloudflare: &'a CloudflareRecovery,
    pub account_identity: &'a AccountIdentityService,
    pub request_id: &'a str,
}

#[derive(Clone, Copy)]
pub(crate) struct AccountUpstreamContext<'a> {
    pub codex: &'a CodexBackendClient,
    pub account_identity: &'a AccountIdentityService,
    pub cloudflare: &'a CloudflareRecovery,
    pub request_id: &'a str,
    pub account: &'a Account,
}

pub(crate) async fn verify_acquired_quota_if_required(
    context: QuotaVerificationContext<'_>,
    acquired: AccountLease,
) -> QuotaVerificationDecision {
    if !acquired.account.quota_verify_required {
        return QuotaVerificationDecision::Ready(Box::new(acquired));
    }

    let account_id = acquired.account.id.clone();
    let identity = context
        .account_identity
        .scope_auxiliary(&account_id, context.request_id);
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
            installation_id: Some(&identity.installation_id),
            session_id: None,
            thread_id: None,
            prompt_cache_key: None,
            client_request_id: Some(&identity.client_request_id),
            turn_id: None,
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
        acquired.release_without_usage().await;
        tracing::info!(
            request_id = %context.request_id,
            account_id = %account_id,
            quota_verify_required = true,
            quota_verify_result = "limit_reached",
            retry_with_another_account = true,
            "quota verification reported exhausted account before upstream request"
        );
        return QuotaVerificationDecision::RetryWithAnotherAccount;
    }

    QuotaVerificationDecision::Ready(Box::new(acquired_with_verified_quota(acquired, &quota)))
}

fn acquired_with_verified_quota(mut acquired: AccountLease, quota: &Value) -> AccountLease {
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
    context: AccountUpstreamContext<'_>,
    request: &CodexResponsesRequest,
    started_at: Instant,
) -> Result<CodexBackendResponse, CodexClientError> {
    let cookie_header = context
        .cloudflare
        .cookie_header_for_request(&context.account.id, "/codex/responses")
        .await;
    let identity = context
        .account_identity
        .scope(request, &context.account.id, context.request_id);
    context
        .codex
        .create_response_with_pool_account_started_at(
            request,
            CodexRequestContext {
                access_token: &context.account.access_token,
                account_id: context.account.account_id.as_deref(),
                request_id: context.request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: identity.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
                installation_id: Some(&identity.installation_id),
                session_id: identity.session_id.as_deref(),
                thread_id: identity.thread_id.as_deref(),
                prompt_cache_key: identity.prompt_cache_key.as_deref(),
                client_request_id: Some(&identity.client_request_id),
                turn_id: identity.turn_id.as_deref(),
            },
            Some(&context.account.id),
            started_at,
        )
        .await
}

async fn create_response_stream_with_account(
    context: AccountUpstreamContext<'_>,
    request: &CodexResponsesRequest,
) -> Result<CodexBackendStreamingResponse, CodexClientError> {
    let cookie_header = context
        .cloudflare
        .cookie_header_for_request(&context.account.id, "/codex/responses")
        .await;
    let identity = context
        .account_identity
        .scope(request, &context.account.id, context.request_id);
    context
        .codex
        .create_response_stream_with_pool_account(
            request,
            CodexRequestContext {
                access_token: &context.account.access_token,
                account_id: context.account.account_id.as_deref(),
                request_id: context.request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: identity.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
                installation_id: Some(&identity.installation_id),
                session_id: identity.session_id.as_deref(),
                thread_id: identity.thread_id.as_deref(),
                prompt_cache_key: identity.prompt_cache_key.as_deref(),
                client_request_id: Some(&identity.client_request_id),
                turn_id: identity.turn_id.as_deref(),
            },
            Some(&context.account.id),
        )
        .await
}

const MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT: usize = 2;

async fn retry_upstream_5xx<T, F, Fut>(
    request_id: &str,
    account_id: &str,
    endpoint: Option<&str>,
    retry_message: &'static str,
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
            Err(error) => return Err(error),
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
        tracing::debug!(
            request_id,
            account_id = %account_id,
            endpoint,
            retry,
            error = %error,
            message
        );
    } else {
        tracing::debug!(
            request_id,
            account_id = %account_id,
            retry,
            error = %error,
            message
        );
    }
}

pub(crate) async fn create_response_with_account_retrying_5xx(
    context: AccountUpstreamContext<'_>,
    request: &CodexResponsesRequest,
    started_at: Instant,
) -> Result<CodexBackendResponse, CodexClientError> {
    retry_upstream_5xx(
        context.request_id,
        &context.account.id,
        None,
        "upstream response request failed with retryable 5xx",
        || create_response_with_account(context, request, started_at),
    )
    .await
}

pub(crate) async fn create_response_stream_with_account_retrying_5xx(
    context: AccountUpstreamContext<'_>,
    request: &CodexResponsesRequest,
) -> Result<CodexBackendStreamingResponse, CodexClientError> {
    retry_upstream_5xx(
        context.request_id,
        &context.account.id,
        None,
        "upstream response stream request failed with retryable 5xx",
        || create_response_stream_with_account(context, request),
    )
    .await
}
