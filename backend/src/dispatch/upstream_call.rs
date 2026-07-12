//! 调度层共享的账号级 Codex 上游调用。

use std::{future::Future, time::Instant};

use chrono::Utc;
use serde_json::{json, Value};

use crate::{
    dispatch::{
        affinity::{AccountIdentityService, AccountScopedIdentity},
        attempts::AccountAttemptLedger,
        errors::{
            is_model_unsupported_upstream_error, is_quota_exhausted_upstream_error,
            is_rate_limit_upstream_error, is_retryable_account_transport_error,
            is_retryable_upstream_5xx_error, rate_limit_cooldown_until, upstream_error_body,
            upstream_error_http_status, upstream_error_set_cookie_headers, ResponseDispatchError,
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
        service::{ResponseDispatchResponse, ResponseDispatchService},
        stream::trace::ResponseDispatchTrace,
    },
    fleet::{
        account::{Account, AccountStatus},
        pool::{AccountAcquireRequest, AccountLease, AccountPoolService},
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

async fn create_compact_response_with_account(
    context: AccountUpstreamContext<'_>,
    request: &CodexCompactRequest,
) -> Result<CodexCompactResponse, CodexClientError> {
    let cookie_header = context
        .cloudflare
        .cookie_header_for_request(&context.account.id, "/codex/responses/compact")
        .await;
    let identity =
        context
            .account_identity
            .scope_compact(request, &context.account.id, context.request_id);
    let upstream_request = compact_upstream_request(request, &identity);
    context
        .codex
        .create_compact_response(
            &upstream_request,
            CodexRequestContext {
                access_token: &context.account.access_token,
                account_id: context.account.account_id.as_deref(),
                request_id: context.request_id,
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
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
        )
        .await
}

fn compact_upstream_request(
    request: &CodexCompactRequest,
    identity: &AccountScopedIdentity,
) -> CodexCompactRequest {
    let mut upstream = request.clone();
    for (key, value) in [
        ("prompt_cache_key", identity.prompt_cache_key.as_deref()),
        ("session_id", identity.session_id.as_deref()),
        ("thread_id", identity.thread_id.as_deref()),
        ("turn_id", identity.turn_id.as_deref()),
        (
            "x-client-request-id",
            Some(identity.client_request_id.as_str()),
        ),
        ("x-codex-window-id", identity.window_id.as_deref()),
        (
            "x-codex-parent-thread-id",
            identity.parent_thread_id.as_deref(),
        ),
        (
            "x-codex-installation-id",
            Some(identity.installation_id.as_str()),
        ),
    ] {
        replace_existing_identity_field(&mut upstream.body, key, value);
    }

    let metadata = match upstream.body.get("client_metadata") {
        Some(Value::Object(metadata)) => Some(metadata.clone()),
        None => Some(serde_json::Map::new()),
        Some(_) => None,
    };
    if let Some(mut metadata) = metadata {
        for (key, value) in [
            (
                "x-codex-installation-id",
                Some(identity.installation_id.as_str()),
            ),
            ("session_id", identity.session_id.as_deref()),
            ("thread_id", identity.thread_id.as_deref()),
            (
                "x-client-request-id",
                Some(identity.client_request_id.as_str()),
            ),
            ("turn_id", identity.turn_id.as_deref()),
            ("x-codex-window-id", identity.window_id.as_deref()),
            (
                "x-codex-parent-thread-id",
                identity.parent_thread_id.as_deref(),
            ),
        ] {
            match value.filter(|value| !value.trim().is_empty()) {
                Some(value) => {
                    metadata.insert(key.to_string(), Value::String(value.to_string()));
                }
                None => {
                    metadata.remove(key);
                }
            }
        }
        if !metadata.is_empty() {
            upstream
                .body
                .insert("client_metadata".to_string(), Value::Object(metadata));
        }
    }
    upstream
}

fn replace_existing_identity_field(
    body: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    if !body.get(key).is_some_and(Value::is_string) {
        return;
    }
    match value.filter(|value| !value.trim().is_empty()) {
        Some(value) => {
            body.insert(key.to_string(), Value::String(value.to_string()));
        }
        None => {
            body.remove(key);
        }
    }
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

pub(crate) async fn create_compact_response_with_account_retrying_5xx(
    context: AccountUpstreamContext<'_>,
    request: &CodexCompactRequest,
) -> Result<CodexCompactResponse, CodexClientError> {
    retry_upstream_5xx(
        context.request_id,
        &context.account.id,
        Some("compact"),
        "upstream compact request failed with retryable 5xx",
        || create_compact_response_with_account(context, request),
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
    ) -> Result<ResponseDispatchResponse, ResponseDispatchError> {
        let started_at = Instant::now();
        let client_api_key_id = request.client_api_key_id.clone();
        let catalog = self.models.catalog().await;
        let display_model = catalog.resolve_model_id(requested_model);
        request.set_model(display_model.clone());
        let mut exhausted_accounts = AccountExhaustionTracker::default();
        let mut trace = ResponseDispatchTrace::default();
        let acquire_request = AccountAcquireRequest::new(request.model(), Utc::now());
        let mut candidates =
            AccountAttemptLedger::freeze(&self.account_pool, &acquire_request).await;

        loop {
            let acquired = match candidates.acquire_next(&self.account_pool).await {
                Some(acquired) => acquired,
                None => {
                    tracing::info!(
                        candidate_count = candidates.candidate_count(),
                        attempted = candidates.attempted_count(),
                        state_excluded = candidates.state_excluded_count(),
                        "Responses compact account candidate ledger exhausted"
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
                    account_identity: &self.account_identity,
                    request_id,
                },
                acquired,
            )
            .await
            {
                QuotaVerificationDecision::Ready(acquired) => *acquired,
                QuotaVerificationDecision::RetryWithAnotherAccount => {
                    exhausted_accounts.record_rate_limited(
                        Some(&acquired_account_id),
                        QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
                    );
                    continue;
                }
            };
            let account = acquired.account.clone();
            let release_account_id = account.id.clone();
            let attempt = trace.start_attempt(&release_account_id);
            let response_result = create_compact_response_with_account_retrying_5xx(
                AccountUpstreamContext {
                    codex: &self.codex,
                    account_identity: &self.account_identity,
                    cloudflare: &self.cloudflare,
                    request_id,
                    account: &account,
                },
                &request,
            )
            .await;
            acquired.complete().await;
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
                            .record_token_usage(&account.id, &usage)
                            .await;
                    }
                    let mut metadata = json!({
                        "stream": false,
                        "compact": true,
                        "usage": usage,
                        "effectiveModel": response.response_metadata.effective_model.as_deref(),
                        "modelsEtag": response.response_metadata.models_etag.as_deref(),
                        "reasoningIncluded": response.response_metadata.reasoning_included,
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
                        model: response
                            .response_metadata
                            .effective_model
                            .as_deref()
                            .unwrap_or(&display_model),
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
                    self.models
                        .observe_models_etag(response.response_metadata.models_etag.as_deref());
                    return Ok(ResponseDispatchResponse {
                        body: response.body,
                        response_headers: response.response_metadata.client_headers,
                    });
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
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    exhausted_accounts.record_quota_exhausted(
                        Some(&release_account_id),
                        upstream_error_body(&error),
                    );
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
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
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    exhausted_accounts.record_cloudflare_challenge(
                        Some(&release_account_id),
                        cloudflare_challenge_error_message(),
                    );
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    exhausted_accounts.record_cloudflare_path_blocked(
                        Some(&release_account_id),
                        cloudflare_path_block_error_message(),
                    );
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    exhausted_accounts
                        .record_model_unsupported(Some(&release_account_id), upstream_error);
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
                }
                Err(error) => {
                    if is_retryable_account_transport_error(&error) {
                        exhausted_accounts.record_upstream_unavailable(
                            Some(&release_account_id),
                            upstream_error_body(&error),
                        );
                        continue;
                    }
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
