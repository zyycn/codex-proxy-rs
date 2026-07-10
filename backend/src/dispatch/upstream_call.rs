//! 调度层共享的账号级 Codex 上游调用。

use std::{future::Future, time::Instant};

use chrono::Utc;
use serde_json::{json, Value};

use crate::{
    dispatch::{
        affinity::build_conversation_identity,
        errors::{
            is_model_unsupported_upstream_error, is_quota_exhausted_upstream_error,
            is_rate_limit_upstream_error, is_retryable_upstream_5xx_error,
            rate_limit_cooldown_until, upstream_error_body, upstream_error_http_status,
            upstream_error_set_cookie_headers, ResponseDispatchError,
        },
        recording::{
            insert_response_status_metadata, insert_response_trace_metadata,
            insert_response_upstream_diagnostics, record_response_dispatch_error_event,
            ResponseDispatchErrorEventRecord,
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
        service::ResponseDispatchService,
        stream::trace::ResponseDispatchTrace,
    },
    fleet::{
        account::{Account, AccountStatus},
        pool::{AccountAcquireRequest, AccountPoolService, AcquiredAccount},
        quota::{
            quota_from_usage, quota_snapshot_limit_reached, quota_snapshot_limit_window_seconds,
            quota_snapshot_reset_at,
        },
    },
    telemetry::{
        recorder::{reasoning_effort_from_compact_request, record_response_event},
        usage::types::ResponseUsageRecord,
    },
    upstream::openai::{
        protocol::{
            events::extract_usage,
            responses::{CodexCompactRequest, CodexResponsesRequest},
        },
        transport::{
            is_banned_upstream_error, CodexBackendClient, CodexBackendResponse,
            CodexBackendStreamingResponse, CodexClientError, CodexCompactResponse,
            CodexRequestContext,
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
    pub account_pool: &'a AccountPoolService,
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

impl ResponseDispatchService {
    /// 调度 Responses compact 请求到 Codex compact 上游。
    pub async fn compact(
        &self,
        request_id: &str,
        mut request: CodexCompactRequest,
        requested_model: &str,
    ) -> Result<Value, ResponseDispatchError> {
        let started_at = Instant::now();
        let client_api_key_id = request.client_api_key_id.clone();
        let catalog = self.models.catalog().await;
        let display_model = catalog.resolve_model_id(requested_model);
        request.set_model(display_model.clone());
        let mut excluded_account_ids = Vec::new();
        let mut exhausted_accounts = AccountExhaustionTracker::default();
        let mut quota_verify_attempts = 0usize;
        let mut trace = ResponseDispatchTrace::default();

        loop {
            let acquire_request = AccountAcquireRequest::new(request.model(), Utc::now())
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            let acquired = match self.account_pool.acquire_with(&acquire_request).await {
                Some(acquired) => acquired,
                None => {
                    let error = exhausted_accounts
                        .last_exhausted()
                        .map(ResponseDispatchError::from_exhausted_account)
                        .unwrap_or(ResponseDispatchError::NoActiveAccount);
                    self.record_compact_dispatch_error(
                        request_id,
                        client_api_key_id.as_deref(),
                        requested_model,
                        started_at,
                        exhausted_accounts.last_account_id(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
            };
            let acquired_account_id = acquired.account.id.clone();
            let acquired = match verify_acquired_quota_if_required(
                QuotaVerificationContext {
                    account_pool: self.account_pool.as_ref(),
                    codex: self.codex.as_ref(),
                    cloudflare: &self.cloudflare,
                    installation_id: self.installation_id.as_deref(),
                    request_id,
                    excluded_account_ids: &mut excluded_account_ids,
                    verify_attempts: &mut quota_verify_attempts,
                    allow_retry_with_another_account: true,
                },
                acquired,
            )
            .await
            {
                QuotaVerificationDecision::Ready(acquired) => *acquired,
                QuotaVerificationDecision::RetryWithAnotherAccount => {
                    exhausted_accounts
                        .record_rate_limited(None, QUOTA_VERIFY_LIMIT_REACHED_MESSAGE);
                    continue;
                }
                QuotaVerificationDecision::MaxAttemptsReached
                | QuotaVerificationDecision::RequiredAccountUnavailable => {
                    exhausted_accounts.record_rate_limited(
                        Some(&acquired_account_id),
                        QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
                    );
                    let error = exhausted_accounts
                        .last_exhausted()
                        .map(ResponseDispatchError::from_exhausted_account)
                        .unwrap_or(ResponseDispatchError::NoActiveAccount);
                    self.record_compact_dispatch_error(
                        request_id,
                        client_api_key_id.as_deref(),
                        requested_model,
                        started_at,
                        Some(&acquired_account_id),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
            };
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let attempt = trace.start_attempt(&release_account_id);
            let response_result = create_compact_response_with_account_retrying_5xx(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
            )
            .await;
            self.account_pool.release(&release_account_id).await;
            if let Err(error) = &response_result {
                self.cloudflare
                    .capture_set_cookie_headers(
                        &release_account_id,
                        upstream_error_set_cookie_headers(error),
                    )
                    .await;
            }

            match response_result {
                Ok(response) => {
                    self.cloudflare
                        .capture_set_cookie_headers(
                            &release_account_id,
                            &response.set_cookie_headers,
                        )
                        .await;
                    self.cloudflare.reset_account_recovery(&account.id).await;
                    self.account_pool
                        .sync_passive_rate_limit_headers(&account, &response.rate_limit_headers)
                        .await;
                    let usage = extract_usage(&response.body);
                    if let Some(usage) = usage {
                        self.account_pool
                            .record_token_usage(&account.id, request.model(), &usage)
                            .await;
                    }
                    let mut metadata = json!({
                        "stream": false,
                        "compact": true,
                        "usage": usage,
                    });
                    insert_response_status_metadata(
                        &mut metadata,
                        200,
                        200,
                        response.diagnostics.status_code.map(i64::from),
                    );
                    insert_response_upstream_diagnostics(&mut metadata, &response.diagnostics);
                    insert_response_trace_metadata(&mut metadata, &trace, Some(&attempt));
                    record_response_event(ResponseUsageRecord {
                        recorder: &self.recorder,
                        request_id,
                        client_api_key_id: request.client_api_key_id.as_deref(),
                        account_id: &account.id,
                        route: "/v1/responses/compact",
                        model: &display_model,
                        requested_model: Some(requested_model),
                        client_ip: request.client_ip.as_deref(),
                        client_user_agent: request.client_user_agent.as_deref(),
                        reasoning_effort: reasoning_effort_from_compact_request(&request),
                        service_tier: None,
                        started_at,
                        status_code: 200,
                        message: "v1 responses compact completed",
                        metadata,
                        rate_limit_headers: &response.rate_limit_headers,
                    })
                    .await;
                    return Ok(response.body);
                }
                Err(error) if is_rate_limit_upstream_error(&error) => {
                    exhausted_accounts.record_rate_limited(
                        Some(&release_account_id),
                        upstream_error_body(&error),
                    );
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    exhausted_accounts.record_quota_exhausted(
                        Some(&release_account_id),
                        upstream_error_body(&error),
                    );
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_auth_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    let account_status = auth_failure_account_status(&error);
                    exhausted_accounts.record_auth_failure(
                        Some(&release_account_id),
                        account_status,
                        upstream_error,
                        Some(upstream_error_http_status(&error)),
                    );
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    exhausted_accounts.record_cloudflare_challenge(
                        Some(&release_account_id),
                        cloudflare_challenge_error_message(),
                    );
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    exhausted_accounts.record_cloudflare_path_blocked(
                        Some(&release_account_id),
                        cloudflare_path_block_error_message(),
                    );
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    if let Some(exhausted) =
                        exhausted_accounts.model_unsupported_retry_exhausted(upstream_error.clone())
                    {
                        let error = ResponseDispatchError::from_exhausted_account(exhausted);
                        self.record_compact_dispatch_error(
                            request_id,
                            client_api_key_id.as_deref(),
                            requested_model,
                            started_at,
                            Some(&release_account_id),
                            &error,
                        )
                        .await;
                        return Err(error);
                    }
                    exhausted_accounts
                        .record_model_unsupported(Some(&release_account_id), upstream_error);
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    exhausted_accounts.record_auth_failure(
                        Some(&release_account_id),
                        AccountStatus::Banned,
                        upstream_error_body(&error),
                        Some(upstream_error_http_status(&error)),
                    );
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::Banned)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) => {
                    let error = ResponseDispatchError::Upstream(error);
                    self.record_compact_dispatch_error(
                        request_id,
                        client_api_key_id.as_deref(),
                        requested_model,
                        started_at,
                        Some(&release_account_id),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
            }
        }
    }

    async fn record_compact_dispatch_error(
        &self,
        request_id: &str,
        client_api_key_id: Option<&str>,
        requested_model: &str,
        started_at: Instant,
        account_id: Option<&str>,
        error: &ResponseDispatchError,
    ) {
        record_response_dispatch_error_event(ResponseDispatchErrorEventRecord {
            recorder: &self.recorder,
            request_id,
            client_api_key_id,
            account_id,
            route: "/v1/responses/compact",
            model: requested_model,
            started_at,
            stream: false,
            compact: true,
            transport: Some("http"),
            error,
        })
        .await;
    }
}
