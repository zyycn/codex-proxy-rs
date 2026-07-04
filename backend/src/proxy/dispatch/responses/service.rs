//! Responses 创建编排与调度服务。
//!
//! 包含了将 OpenAI 请求调度到 Codex 上游账号的完整逻辑，包括：
//! - 响应创建（非流式 / 流式 / compact）
//! - 会话亲和性与隐式续接
//! - reasoning replay
//! - 账号回退与错误恢复
//! - 配额验证

use std::{pin::Pin, sync::Arc, time::Instant};

use axum::body::Bytes;
use chrono::{DateTime, Duration, Utc};
use futures::stream::Stream;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::{
    admin::monitoring::{
        usage_record_model::{ResponseUsageRecord, UsageRecordLevel},
        usage_record_service::AdminUsageRecordService,
    },
    proxy::dispatch::{
        auth_recovery::trigger_refresh_after_auth_failure,
        cloudflare::{
            cloudflare_challenge_error_message, cloudflare_path_block_error_message,
            is_cloudflare_challenge_upstream_error, is_cloudflare_path_block_upstream_error,
            CloudflareRecovery,
        },
        errors::{
            auth_failure_account_status, backend_transport_name, is_auth_upstream_error,
            is_history_recovery_upstream_error, is_model_unsupported_upstream_error,
            is_quota_exhausted_upstream_error, is_rate_limit_upstream_error,
            rate_limit_cooldown_until, upstream_error_body, upstream_error_http_status,
            upstream_error_set_cookie_headers,
        },
        exhaustion::AccountExhaustionTracker,
        reasoning_replay::ReasoningReplayCache,
        session_affinity::{
            compute_variant_hash, ensure_prompt_cache_key, hash_instructions,
            prepare_variant_identity, RuntimeSessionAffinityService,
        },
        upstream::{
            create_compact_response_with_account_retrying_5xx,
            create_response_stream_with_account_retrying_5xx,
            create_response_with_account_retrying_5xx, verify_acquired_quota_if_required,
            QuotaVerificationContext, QuotaVerificationDecision,
            QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
        },
        usage_events::{
            reasoning_effort_from_compact_request, reasoning_effort_from_request,
            record_response_event,
        },
    },
    upstream::accounts::{
        model::{Account, AccountStatus},
        pool::{AccountAcquireRequest, RuntimeAccountPoolService},
        token_refresh::RuntimeTokenRefreshService,
    },
    upstream::{
        models::service::ModelService,
        protocol::{
            events::{extract_usage, TokenUsage},
            responses::{
                response_from_codex_sse, CodexCompactRequest, CodexResponsesRequest,
                CollectedResponse,
            },
        },
        token_client::OpenAiTokenClient,
        transport::{
            backend_transport_for_response_request, is_banned_upstream_error, CodexBackendClient,
            CodexBackendResponse,
        },
    },
};

use crate::proxy::dispatch::implicit_resume::{
    continuation_input_start, implicit_resume_allowed, ImplicitResumeSnapshot,
};

use super::{
    affinity::{evict_reasoning_replay, record_response_affinity},
    errors::{ResponseDispatchError, ResponseDispatchStreamError},
    event_recording::{
        insert_websocket_pool_decision, record_prefetched_response_stream_failure_event,
        record_response_dispatch_error_event, record_response_upstream_error_event,
        ResponseDispatchErrorDetails, ResponseDispatchErrorEventRecord,
        ResponseStreamFailureEventRecord, ResponseUpstreamErrorEventRecord,
    },
    live_stream::spawn_live_response_stream,
    prefetch::prefetch_first_sse_chunk,
    sse_failure::{
        auth_sse_failure_account_status, client_error_invalid_reasoning_replay, first_sse_failure,
        is_auth_sse_failure, is_history_recovery_sse_failure, is_model_unsupported_sse_failure,
        is_quota_exhausted_sse_failure, sse_failure_error_body,
        sse_failure_invalid_reasoning_replay, stream_failure_http_status,
    },
    stream_lifecycle::LiveResponseStreamContext,
};

/// OpenAI Responses 调度服务。
#[derive(Clone)]
pub struct ResponseDispatchService {
    account_pool: Arc<RuntimeAccountPoolService>,
    models: Arc<ModelService>,
    codex: Arc<CodexBackendClient>,
    session_affinity: Arc<RuntimeSessionAffinityService>,
    reasoning_replay: Arc<Mutex<ReasoningReplayCache>>,
    usage_records: Arc<AdminUsageRecordService>,
    token_refresh: Arc<RuntimeTokenRefreshService<OpenAiTokenClient>>,
    installation_id: Option<String>,
    cloudflare: CloudflareRecovery,
}

pub(crate) struct ResponseDispatchServiceParts {
    pub account_pool: Arc<RuntimeAccountPoolService>,
    pub models: Arc<ModelService>,
    pub codex: Arc<CodexBackendClient>,
    pub session_affinity: Arc<RuntimeSessionAffinityService>,
    pub usage_records: Arc<AdminUsageRecordService>,
    pub token_refresh: Arc<RuntimeTokenRefreshService<OpenAiTokenClient>>,
    pub installation_id: Option<String>,
    pub cloudflare: CloudflareRecovery,
}

/// 默认 reasoning replay TTL 秒数。
const DEFAULT_REASONING_REPLAY_TTL_SECS: i64 = 55 * 60;
const IMPLICIT_RESUME_MAX_AGE_SECS: i64 = DEFAULT_REASONING_REPLAY_TTL_SECS;

/// Responses live SSE 响应体流。
pub type ResponseBodyStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, ResponseDispatchStreamError>> + Send + 'static>>;

/// Responses live SSE 调度结果。
pub struct ResponseDispatchStream {
    pub body: ResponseBodyStream,
}

impl ResponseDispatchService {
    pub(crate) fn new(parts: ResponseDispatchServiceParts) -> Self {
        Self {
            account_pool: parts.account_pool,
            models: parts.models,
            codex: parts.codex,
            session_affinity: parts.session_affinity,
            reasoning_replay: Arc::new(Mutex::new(ReasoningReplayCache::new(Duration::seconds(
                DEFAULT_REASONING_REPLAY_TTL_SECS,
            )))),
            usage_records: parts.usage_records,
            token_refresh: parts.token_refresh,
            installation_id: parts.installation_id,
            cloudflare: parts.cloudflare,
        }
    }

    async fn prepare_response_session(
        &self,
        request: &mut CodexResponsesRequest,
    ) -> Option<ImplicitResumeSnapshot> {
        prepare_variant_identity(request);
        if let Some(previous_response_id) = request.previous_response_id().map(ToString::to_string)
        {
            if request.prompt_cache_key().is_none() {
                let conversation_id = self
                    .session_affinity
                    .lookup_conversation_id(&previous_response_id, Utc::now())
                    .await;
                request.set_prompt_cache_key(conversation_id);
            }
            if request.turn_state.as_deref().is_none_or(str::is_empty) {
                request.turn_state = self
                    .session_affinity
                    .lookup_turn_state(&previous_response_id, Utc::now())
                    .await;
            }
            ensure_prompt_cache_key(request);
            return None;
        }

        ensure_prompt_cache_key(request);
        self.apply_implicit_resume(request).await
    }

    async fn apply_implicit_resume(
        &self,
        request: &mut CodexResponsesRequest,
    ) -> Option<ImplicitResumeSnapshot> {
        let continuation_start = continuation_input_start(request.input());
        if continuation_start == 0 || continuation_start >= request.input().len() {
            return None;
        }
        let conversation_id = request
            .prompt_cache_key()
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();
        let snapshot = ImplicitResumeSnapshot::capture(request);
        let variant_hash = compute_variant_hash(request);
        let now = Utc::now();
        let previous_response_id = self
            .session_affinity
            .lookup_latest_response_by_conversation(
                &conversation_id,
                Some(Duration::seconds(IMPLICIT_RESUME_MAX_AGE_SECS)),
                Some(&variant_hash),
                now,
            )
            .await?;
        let current_instructions_hash = hash_instructions(Some(request.instructions()));
        if self
            .session_affinity
            .lookup_instructions_hash(&previous_response_id, now)
            .await
            .as_deref()
            != Some(current_instructions_hash.as_str())
        {
            return None;
        }
        let stored_function_call_ids = self
            .session_affinity
            .lookup_function_call_ids(&previous_response_id, now)
            .await;
        if !implicit_resume_allowed(
            &request.input()[continuation_start..],
            request.input(),
            &stored_function_call_ids,
        ) {
            return None;
        }
        let account_id = self
            .session_affinity
            .lookup_account(&previous_response_id, now)
            .await?;
        let replay_items = self.reasoning_replay.lock().await.lookup(
            &previous_response_id,
            &account_id,
            &conversation_id,
            &variant_hash,
            now,
        );
        let continuation = request.input()[continuation_start..].to_vec();
        let mut input = replay_items;
        input.extend(continuation);

        request.set_previous_response_id(Some(previous_response_id.clone()));
        request.use_websocket = true;
        request.force_http_sse = false;
        request.set_input(input);
        if let Some(turn_state) = self
            .session_affinity
            .lookup_turn_state(&previous_response_id, now)
            .await
        {
            request.turn_state = Some(turn_state);
        }

        Some(snapshot)
    }

    async fn preferred_account_id_for_request(
        &self,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let previous_response_id = request.previous_response_id()?;
        self.session_affinity
            .lookup_account(previous_response_id, now)
            .await
    }

    async fn recover_request_history(
        &self,
        request: &mut CodexResponsesRequest,
        implicit_resume: &mut Option<ImplicitResumeSnapshot>,
    ) {
        if let Some(previous_response_id) = request.previous_response_id() {
            self.session_affinity.forget(previous_response_id).await;
        }
        if let Some(snapshot) = implicit_resume.take() {
            snapshot.restore(request);
            request.set_previous_response_id(None);
            request.turn_state = None;
        } else {
            strip_request_history(request);
        }
    }

    async fn apply_cascading_ban_defense(
        &self,
        request: &mut CodexResponsesRequest,
        implicit_resume: &mut Option<ImplicitResumeSnapshot>,
        preferred_account_id: Option<&str>,
        acquired_account_id: &str,
        explicit_previous_response_id: Option<&str>,
    ) -> bool {
        let Some(preferred_account_id) =
            preferred_account_id.filter(|account_id| *account_id != acquired_account_id)
        else {
            return false;
        };
        let has_history = request.previous_response_id().is_some()
            || request
                .turn_state
                .as_deref()
                .is_some_and(|value| !value.is_empty());
        if !has_history {
            return false;
        }
        let Some(preferred_account) = self
            .account_pool
            .account_snapshot(preferred_account_id)
            .await
        else {
            return false;
        };
        if !matches!(
            preferred_account.status,
            AccountStatus::Banned | AccountStatus::Disabled
        ) {
            return false;
        }

        let response_id_to_forget = explicit_previous_response_id
            .or(request.previous_response_id())
            .map(str::to_string);
        if let Some(snapshot) = implicit_resume.take() {
            snapshot.restore(request);
            request.set_previous_response_id(None);
            request.turn_state = None;
        } else {
            strip_request_history(request);
        }
        if let Some(response_id) = response_id_to_forget {
            self.session_affinity.forget(&response_id).await;
        }
        true
    }

    async fn evict_reasoning_replay(&self, request: &CodexResponsesRequest, account_id: &str) {
        evict_reasoning_replay(&self.reasoning_replay, request, account_id).await;
    }

    async fn record_response_affinity(
        &self,
        request: &CodexResponsesRequest,
        account_id: &str,
        body: &str,
        turn_state: Option<String>,
        usage: Option<TokenUsage>,
    ) {
        record_response_affinity(
            &self.session_affinity,
            &self.reasoning_replay,
            request,
            account_id,
            body,
            turn_state,
            usage,
        )
        .await;
    }

    /// 调度非流式 Responses 请求到 Codex Responses 上游。
    pub async fn complete(
        &self,
        request_id: &str,
        route: &str,
        mut request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<Value, ResponseDispatchError> {
        let started_at = Instant::now();
        let catalog = self.models.catalog().await;
        let display_model = catalog.resolve_model_id(requested_model);
        request.set_model(display_model.clone());
        let tuple_schema = request.tuple_schema.clone();
        let image_generation_requested = request.expects_image_generation();
        let now = Utc::now();
        let explicit_previous_response_id = request.previous_response_id().map(ToString::to_string);
        let mut implicit_resume = self.prepare_response_session(&mut request).await;
        let preferred_account_id = self.preferred_account_id_for_request(&request, now).await;
        let mut acquire_request = AccountAcquireRequest::new(request.model(), now);
        if let Some(preferred_account_id) = preferred_account_id.as_deref() {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        let mut excluded_account_ids = Vec::new();
        let mut exhausted_accounts = AccountExhaustionTracker::default();
        let mut history_recovery_used = false;
        let mut empty_response_retries = 0u8;
        let mut quota_verify_attempts = 0usize;
        const MAX_EMPTY_RESPONSE_RETRIES: u8 = 2;
        let (account, response, collected_response): (
            Account,
            CodexBackendResponse,
            CollectedResponse,
        ) = loop {
            let mut attempt_acquire_request = acquire_request
                .clone()
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            attempt_acquire_request.now = Utc::now();
            let Some(acquired) = self
                .account_pool
                .acquire_with(&attempt_acquire_request)
                .await
            else {
                let error = exhausted_accounts
                    .last_exhausted()
                    .map(ResponseDispatchError::from_exhausted_account)
                    .unwrap_or(ResponseDispatchError::NoActiveAccount);
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        account_id: exhausted_accounts.last_account_id(),
                        stream: false,
                        compact: false,
                        transport: Some(backend_transport_name(
                            backend_transport_for_response_request(&request),
                        )),
                    },
                    &error,
                )
                .await;
                return Err(error);
            };
            let acquired_account_id = acquired.account.id.clone();

            // 配额验证
            let acquired = match verify_acquired_quota_if_required(
                QuotaVerificationContext {
                    account_pool: self.account_pool.as_ref(),
                    codex: self.codex.as_ref(),
                    cloudflare: &self.cloudflare,
                    installation_id: self.installation_id.as_deref(),
                    request_id,
                    excluded_account_ids: &mut excluded_account_ids,
                    verify_attempts: &mut quota_verify_attempts,
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
                QuotaVerificationDecision::MaxAttemptsReached => {
                    exhausted_accounts.record_rate_limited(
                        Some(&acquired_account_id),
                        QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
                    );
                    let error = exhausted_accounts
                        .last_exhausted()
                        .map(ResponseDispatchError::from_exhausted_account)
                        .unwrap_or(ResponseDispatchError::NoActiveAccount);
                    self.record_response_dispatch_error(
                        request_id,
                        route,
                        requested_model,
                        started_at,
                        ResponseDispatchErrorDetails {
                            account_id: Some(&acquired_account_id),
                            stream: false,
                            compact: false,
                            transport: Some(backend_transport_name(
                                backend_transport_for_response_request(&request),
                            )),
                        },
                        &error,
                    )
                    .await;
                    return Err(error);
                }
            };

            self.apply_cascading_ban_defense(
                &mut request,
                &mut implicit_resume,
                preferred_account_id.as_deref(),
                &acquired.account.id,
                explicit_previous_response_id.as_deref(),
            )
            .await;
            self.account_pool.wait_for_request_interval(&acquired).await;
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let response_result = create_response_with_account_retrying_5xx(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
                started_at,
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
                    self.account_pool
                        .sync_passive_rate_limit_headers(&account, &response.rate_limit_headers)
                        .await;
                    let collected_response =
                        match response_from_codex_sse(&response.body, tuple_schema.as_ref()) {
                            Ok(collected_response) => collected_response,
                            Err(error) => {
                                let error = ResponseDispatchError::InvalidSse(error);
                                self.record_response_dispatch_error(
                                    request_id,
                                    route,
                                    requested_model,
                                    started_at,
                                    ResponseDispatchErrorDetails {
                                        account_id: Some(&release_account_id),
                                        stream: false,
                                        compact: false,
                                        transport: Some(backend_transport_name(response.transport)),
                                    },
                                    &error,
                                )
                                .await;
                                return Err(error);
                            }
                        };
                    if matches!(collected_response, CollectedResponse::Empty) {
                        self.account_pool
                            .record_empty_response_attempt(
                                &release_account_id,
                                request.model(),
                                image_generation_requested,
                            )
                            .await;
                        empty_response_retries += 1;
                        if empty_response_retries <= MAX_EMPTY_RESPONSE_RETRIES {
                            continue;
                        }
                    }
                    if let CollectedResponse::Failed(failure) = &collected_response {
                        if is_history_recovery_sse_failure(failure) && !history_recovery_used {
                            if sse_failure_invalid_reasoning_replay(failure) {
                                self.evict_reasoning_replay(&request, &release_account_id)
                                    .await;
                            }
                            self.recover_request_history(&mut request, &mut implicit_resume)
                                .await;
                            history_recovery_used = true;
                            continue;
                        }
                        if is_model_unsupported_sse_failure(failure) {
                            let upstream_error = sse_failure_error_body(failure);
                            if let Some(exhausted) = exhausted_accounts
                                .model_unsupported_retry_exhausted(upstream_error.clone())
                            {
                                let error =
                                    ResponseDispatchError::from_exhausted_account(exhausted);
                                self.record_response_dispatch_error(
                                    request_id,
                                    route,
                                    requested_model,
                                    started_at,
                                    ResponseDispatchErrorDetails {
                                        account_id: Some(&release_account_id),
                                        stream: false,
                                        compact: false,
                                        transport: Some(backend_transport_name(response.transport)),
                                    },
                                    &error,
                                )
                                .await;
                                return Err(error);
                            }
                            exhausted_accounts.record_model_unsupported(
                                Some(&release_account_id),
                                upstream_error,
                            );
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        if is_quota_exhausted_sse_failure(failure) {
                            exhausted_accounts.record_quota_exhausted(
                                Some(&release_account_id),
                                failure.message.clone(),
                            );
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        if is_auth_sse_failure(failure) {
                            let upstream_error = sse_failure_error_body(failure);
                            let account_status = auth_sse_failure_account_status(failure);
                            exhausted_accounts.record_auth_failure(
                                Some(&release_account_id),
                                account_status,
                                upstream_error,
                                Some(stream_failure_http_status(failure)),
                            );
                            self.account_pool
                                .set_status(&release_account_id, account_status)
                                .await;
                            trigger_refresh_after_auth_failure(
                                &self.token_refresh,
                                &release_account_id,
                                account_status,
                            );
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                    }
                    break (account, response, collected_response);
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
                Err(error)
                    if is_history_recovery_upstream_error(&error) && !history_recovery_used =>
                {
                    if client_error_invalid_reasoning_replay(&error) {
                        self.evict_reasoning_replay(&request, &release_account_id)
                            .await;
                    }
                    self.recover_request_history(&mut request, &mut implicit_resume)
                        .await;
                    history_recovery_used = true;
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
                    trigger_refresh_after_auth_failure(
                        &self.token_refresh,
                        &release_account_id,
                        account_status,
                    );
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
                        self.record_response_dispatch_error(
                            request_id,
                            route,
                            requested_model,
                            started_at,
                            ResponseDispatchErrorDetails {
                                account_id: Some(&release_account_id),
                                stream: false,
                                compact: false,
                                transport: Some(backend_transport_name(
                                    backend_transport_for_response_request(&request),
                                )),
                            },
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
                    record_response_upstream_error_event(ResponseUpstreamErrorEventRecord {
                        usage_records: &self.usage_records,
                        request_id,
                        account_id: &release_account_id,
                        account_email: account.email.as_deref(),
                        route,
                        model: requested_model,
                        started_at,
                        stream: false,
                        transport: backend_transport_for_response_request(&request),
                        error: &error,
                    })
                    .await;
                    return Err(ResponseDispatchError::Upstream(error));
                }
            }
        };

        match collected_response {
            CollectedResponse::Completed(body) => {
                let response_id = body.get("id").and_then(Value::as_str);
                self.cloudflare.reset_account_recovery(&account.id).await;
                if let Some(usage) = response.usage {
                    self.account_pool
                        .record_response_usage(
                            &account.id,
                            request.model(),
                            usage,
                            image_generation_requested,
                        )
                        .await;
                }
                self.record_response_affinity(
                    &request,
                    &account.id,
                    &response.body,
                    response.turn_state.clone(),
                    response.usage,
                )
                .await;
                let mut metadata = json!({
                    "responseId": response_id,
                    "stream": false,
                    "transport": backend_transport_name(response.transport),
                    "firstTokenMs": response.first_token_ms,
                    "usage": response.usage,
                });
                insert_websocket_pool_decision(&mut metadata, response.websocket_pool_decision);
                record_response_event(ResponseUsageRecord {
                    usage_records: &self.usage_records,
                    request_id,
                    account_id: &account.id,
                    route,
                    model: &display_model,
                    requested_model: Some(requested_model),
                    client_ip: request.client_ip.as_deref(),
                    client_user_agent: request.client_user_agent.as_deref(),
                    reasoning_effort: reasoning_effort_from_request(&request),
                    service_tier: request.service_tier(),
                    started_at,
                    status_code: 200,
                    level: UsageRecordLevel::Info,
                    message: "v1 responses completed",
                    metadata,
                    rate_limit_headers: &response.rate_limit_headers,
                })
                .await;
                Ok(body)
            }
            CollectedResponse::Failed(failure) => {
                let error = ResponseDispatchError::Failed(failure);
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        account_id: Some(&account.id),
                        stream: false,
                        compact: false,
                        transport: Some(backend_transport_name(response.transport)),
                    },
                    &error,
                )
                .await;
                Err(error)
            }
            CollectedResponse::MissingCompleted => {
                let error = ResponseDispatchError::MissingCompleted;
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        account_id: Some(&account.id),
                        stream: false,
                        compact: false,
                        transport: Some(backend_transport_name(response.transport)),
                    },
                    &error,
                )
                .await;
                Err(error)
            }
            CollectedResponse::Empty => {
                let error = ResponseDispatchError::EmptyUpstreamResponse;
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        account_id: Some(&account.id),
                        stream: false,
                        compact: false,
                        transport: Some(backend_transport_name(response.transport)),
                    },
                    &error,
                )
                .await;
                Err(error)
            }
        }
    }

    async fn record_response_dispatch_error(
        &self,
        request_id: &str,
        route: &str,
        requested_model: &str,
        started_at: Instant,
        details: ResponseDispatchErrorDetails<'_>,
        error: &ResponseDispatchError,
    ) {
        record_response_dispatch_error_event(ResponseDispatchErrorEventRecord {
            usage_records: &self.usage_records,
            request_id,
            account_id: details.account_id,
            route,
            model: requested_model,
            started_at,
            stream: details.stream,
            compact: details.compact,
            transport: details.transport,
            error,
        })
        .await;
    }

    /// 调度流式 Responses 请求到 Codex Responses 上游。
    pub async fn stream(
        &self,
        request_id: &str,
        route: &str,
        mut request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<ResponseDispatchStream, ResponseDispatchError> {
        let started_at = Instant::now();
        let catalog = self.models.catalog().await;
        let display_model = catalog.resolve_model_id(requested_model);
        request.set_model(display_model.clone());
        request.set_stream(true);
        let tuple_schema = request.tuple_schema.clone();
        let now = Utc::now();
        let explicit_previous_response_id = request.previous_response_id().map(ToString::to_string);
        let mut implicit_resume = self.prepare_response_session(&mut request).await;
        let preferred_account_id = self.preferred_account_id_for_request(&request, now).await;
        let mut acquire_request = AccountAcquireRequest::new(request.model(), now);
        if let Some(preferred_account_id) = preferred_account_id.as_deref() {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        let mut excluded_account_ids = Vec::new();
        let mut exhausted_accounts = AccountExhaustionTracker::default();
        let mut history_recovery_used = false;
        let mut quota_verify_attempts = 0usize;
        macro_rules! return_stream_dispatch_error {
            ($error:expr) => {{
                let error = $error;
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        account_id: exhausted_accounts.last_account_id(),
                        stream: true,
                        compact: false,
                        transport: Some(backend_transport_name(
                            backend_transport_for_response_request(&request),
                        )),
                    },
                    &error,
                )
                .await;
                return Err(error);
            }};
            ($error:expr, account_id: $account_id:expr, transport: $transport:expr) => {{
                let error = $error;
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        account_id: $account_id,
                        stream: true,
                        compact: false,
                        transport: $transport,
                    },
                    &error,
                )
                .await;
                return Err(error);
            }};
        }
        loop {
            let mut attempt_acquire_request = acquire_request
                .clone()
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            attempt_acquire_request.now = Utc::now();
            let Some(acquired) = self
                .account_pool
                .acquire_with(&attempt_acquire_request)
                .await
            else {
                let error = exhausted_accounts
                    .last_exhausted()
                    .map(ResponseDispatchError::from_exhausted_account)
                    .unwrap_or(ResponseDispatchError::NoActiveAccount);
                return_stream_dispatch_error!(error);
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
                QuotaVerificationDecision::MaxAttemptsReached => {
                    exhausted_accounts.record_rate_limited(
                        Some(&acquired_account_id),
                        QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
                    );
                    let error = exhausted_accounts
                        .last_exhausted()
                        .map(ResponseDispatchError::from_exhausted_account)
                        .unwrap_or(ResponseDispatchError::NoActiveAccount);
                    return_stream_dispatch_error!(
                        error,
                        account_id: Some(&acquired_account_id),
                        transport: Some(backend_transport_name(backend_transport_for_response_request(
                            &request
                        )))
                    );
                }
            };

            self.apply_cascading_ban_defense(
                &mut request,
                &mut implicit_resume,
                preferred_account_id.as_deref(),
                &acquired.account.id,
                explicit_previous_response_id.as_deref(),
            )
            .await;

            self.account_pool.wait_for_request_interval(&acquired).await;
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let response_result = create_response_stream_with_account_retrying_5xx(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
            )
            .await;
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
                    let transport = response.transport;
                    let set_cookie_headers = response.set_cookie_headers;
                    let rate_limit_headers = response.rate_limit_headers;
                    let rate_limit_header_updates = response.rate_limit_header_updates;
                    let turn_state_update = response.turn_state_update;
                    let websocket_pool_decision = response.websocket_pool_decision;
                    let turn_state = response.turn_state;
                    self.cloudflare
                        .capture_set_cookie_headers(&release_account_id, &set_cookie_headers)
                        .await;
                    self.account_pool
                        .sync_passive_rate_limit_headers(&account, &rate_limit_headers)
                        .await;
                    let (prefetched, body) = match prefetch_first_sse_chunk(response.body).await {
                        Ok(prefetched) => prefetched,
                        Err(ResponseDispatchError::Upstream(error))
                            if is_history_recovery_upstream_error(&error)
                                && !history_recovery_used =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            if client_error_invalid_reasoning_replay(&error) {
                                self.evict_reasoning_replay(&request, &release_account_id)
                                    .await;
                            }
                            self.recover_request_history(&mut request, &mut implicit_resume)
                                .await;
                            history_recovery_used = true;
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_rate_limit_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            exhausted_accounts.record_rate_limited(
                                Some(&release_account_id),
                                upstream_error_body(&error),
                            );
                            let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                            self.account_pool
                                .mark_quota_limited_until(&release_account_id, cooldown_until)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_quota_exhausted_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            exhausted_accounts.record_quota_exhausted(
                                Some(&release_account_id),
                                upstream_error_body(&error),
                            );
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_auth_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
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
                            trigger_refresh_after_auth_failure(
                                &self.token_refresh,
                                &release_account_id,
                                account_status,
                            );
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_cloudflare_challenge_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            exhausted_accounts.record_cloudflare_challenge(
                                Some(&release_account_id),
                                cloudflare_challenge_error_message(),
                            );
                            self.cloudflare
                                .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_cloudflare_path_block_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            exhausted_accounts.record_cloudflare_path_blocked(
                                Some(&release_account_id),
                                cloudflare_path_block_error_message(),
                            );
                            self.cloudflare
                                .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_model_unsupported_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            let upstream_error = upstream_error_body(&error);
                            if let Some(exhausted) = exhausted_accounts
                                .model_unsupported_retry_exhausted(upstream_error.clone())
                            {
                                return_stream_dispatch_error!(
                                    ResponseDispatchError::from_exhausted_account(exhausted),
                                    account_id: Some(&release_account_id),
                                    transport: Some(backend_transport_name(transport))
                                );
                            }
                            exhausted_accounts.record_model_unsupported(
                                Some(&release_account_id),
                                upstream_error,
                            );
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        Err(ResponseDispatchError::Upstream(error))
                            if is_banned_upstream_error(&error) =>
                        {
                            self.account_pool.release(&release_account_id).await;
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
                            continue;
                        }
                        Err(error) => {
                            self.account_pool.release(&release_account_id).await;
                            if let ResponseDispatchError::Upstream(upstream_error) = &error {
                                record_response_upstream_error_event(
                                    ResponseUpstreamErrorEventRecord {
                                        usage_records: &self.usage_records,
                                        request_id,
                                        account_id: &release_account_id,
                                        account_email: account.email.as_deref(),
                                        route,
                                        model: requested_model,
                                        started_at,
                                        stream: true,
                                        transport,
                                        error: upstream_error,
                                    },
                                )
                                .await;
                                return Err(error);
                            }
                            return_stream_dispatch_error!(
                                error,
                                account_id: Some(&release_account_id),
                                transport: Some(backend_transport_name(transport))
                            );
                        }
                    };
                    let first_failure = match first_sse_failure(&prefetched) {
                        Ok(failure) => failure,
                        Err(error) => {
                            self.account_pool.release(&release_account_id).await;
                            return_stream_dispatch_error!(
                                ResponseDispatchError::InvalidSse(error),
                                account_id: Some(&release_account_id),
                                transport: Some(backend_transport_name(transport))
                            );
                        }
                    };
                    if let Some(failure) = first_failure {
                        if is_history_recovery_sse_failure(&failure) && !history_recovery_used {
                            self.account_pool.release(&release_account_id).await;
                            if sse_failure_invalid_reasoning_replay(&failure) {
                                self.evict_reasoning_replay(&request, &release_account_id)
                                    .await;
                            }
                            self.recover_request_history(&mut request, &mut implicit_resume)
                                .await;
                            history_recovery_used = true;
                            continue;
                        }
                        if is_model_unsupported_sse_failure(&failure) {
                            let upstream_error = sse_failure_error_body(&failure);
                            if let Some(exhausted) = exhausted_accounts
                                .model_unsupported_retry_exhausted(upstream_error.clone())
                            {
                                self.account_pool.release(&release_account_id).await;
                                return_stream_dispatch_error!(
                                    ResponseDispatchError::from_exhausted_account(exhausted),
                                    account_id: Some(&release_account_id),
                                    transport: Some(backend_transport_name(transport))
                                );
                            }
                            exhausted_accounts.record_model_unsupported(
                                Some(&release_account_id),
                                upstream_error,
                            );
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        if is_quota_exhausted_sse_failure(&failure) {
                            exhausted_accounts.record_quota_exhausted(
                                Some(&release_account_id),
                                failure.message.clone(),
                            );
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        if is_auth_sse_failure(&failure) {
                            let upstream_error = sse_failure_error_body(&failure);
                            let account_status = auth_sse_failure_account_status(&failure);
                            exhausted_accounts.record_auth_failure(
                                Some(&release_account_id),
                                account_status,
                                upstream_error,
                                Some(stream_failure_http_status(&failure)),
                            );
                            self.account_pool
                                .set_status(&release_account_id, account_status)
                                .await;
                            trigger_refresh_after_auth_failure(
                                &self.token_refresh,
                                &release_account_id,
                                account_status,
                            );
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        self.account_pool.release(&release_account_id).await;
                        record_prefetched_response_stream_failure_event(
                            ResponseStreamFailureEventRecord {
                                usage_records: &self.usage_records,
                                request_id,
                                account_id: &release_account_id,
                                route,
                                model: &display_model,
                                requested_model,
                                started_at,
                                transport,
                                request: &request,
                                failure: &failure,
                                rate_limit_headers: &rate_limit_headers,
                                prefetched: &prefetched,
                            },
                        )
                        .await;
                        return Err(ResponseDispatchError::Failed(failure.clone()));
                    }

                    let context = LiveResponseStreamContext {
                        account_pool: Arc::clone(&self.account_pool),
                        session_affinity: Arc::clone(&self.session_affinity),
                        reasoning_replay: Arc::clone(&self.reasoning_replay),
                        usage_records: Arc::clone(&self.usage_records),
                        cloudflare: self.cloudflare.clone(),
                        account_id: account.id,
                        account_plan_type: account.plan_type,
                        request_id: request_id.to_string(),
                        route: route.to_string(),
                        model: request.model().to_string(),
                        display_model: display_model.clone(),
                        requested_model: requested_model.to_string(),
                        client_ip: request.client_ip.clone(),
                        request,
                        tuple_schema,
                        transport,
                        rate_limit_headers,
                        rate_limit_header_updates,
                        turn_state_update,
                        websocket_pool_decision,
                        turn_state,
                        started_at,
                    };
                    return Ok(spawn_live_response_stream(context, prefetched, body));
                }
                Err(error) if is_rate_limit_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
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
                    self.account_pool.release(&release_account_id).await;
                    exhausted_accounts.record_quota_exhausted(
                        Some(&release_account_id),
                        upstream_error_body(&error),
                    );
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error)
                    if is_history_recovery_upstream_error(&error) && !history_recovery_used =>
                {
                    self.account_pool.release(&release_account_id).await;
                    if client_error_invalid_reasoning_replay(&error) {
                        self.evict_reasoning_replay(&request, &release_account_id)
                            .await;
                    }
                    self.recover_request_history(&mut request, &mut implicit_resume)
                        .await;
                    history_recovery_used = true;
                }
                Err(error) if is_auth_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
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
                    trigger_refresh_after_auth_failure(
                        &self.token_refresh,
                        &release_account_id,
                        account_status,
                    );
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
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
                    self.account_pool.release(&release_account_id).await;
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
                    self.account_pool.release(&release_account_id).await;
                    let upstream_error = upstream_error_body(&error);
                    if let Some(exhausted) =
                        exhausted_accounts.model_unsupported_retry_exhausted(upstream_error.clone())
                    {
                        return_stream_dispatch_error!(
                            ResponseDispatchError::from_exhausted_account(exhausted),
                            account_id: Some(&release_account_id),
                            transport: Some(backend_transport_name(backend_transport_for_response_request(
                                &request
                            )))
                        );
                    }
                    exhausted_accounts
                        .record_model_unsupported(Some(&release_account_id), upstream_error);
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
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
                    self.account_pool.release(&release_account_id).await;
                    record_response_upstream_error_event(ResponseUpstreamErrorEventRecord {
                        usage_records: &self.usage_records,
                        request_id,
                        account_id: &release_account_id,
                        account_email: account.email.as_deref(),
                        route,
                        model: requested_model,
                        started_at,
                        stream: true,
                        transport: backend_transport_for_response_request(&request),
                        error: &error,
                    })
                    .await;
                    return Err(ResponseDispatchError::Upstream(error));
                }
            }
        }
    }

    /// 调度 Responses compact 请求到 Codex compact 上游。
    pub async fn compact(
        &self,
        request_id: &str,
        mut request: CodexCompactRequest,
        requested_model: &str,
    ) -> Result<Value, ResponseDispatchError> {
        let started_at = Instant::now();
        let catalog = self.models.catalog().await;
        let display_model = catalog.resolve_model_id(requested_model);
        request.set_model(display_model.clone());
        let mut excluded_account_ids = Vec::new();
        let mut exhausted_accounts = AccountExhaustionTracker::default();
        let mut quota_verify_attempts = 0usize;

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
                QuotaVerificationDecision::MaxAttemptsReached => {
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
                    record_response_event(ResponseUsageRecord {
                        usage_records: &self.usage_records,
                        request_id,
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
                        level: UsageRecordLevel::Info,
                        message: "v1 responses compact completed",
                        metadata: json!({
                            "stream": false,
                            "compact": true,
                            "usage": usage,
                        }),
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
                    trigger_refresh_after_auth_failure(
                        &self.token_refresh,
                        &release_account_id,
                        account_status,
                    );
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
        requested_model: &str,
        started_at: Instant,
        account_id: Option<&str>,
        error: &ResponseDispatchError,
    ) {
        record_response_dispatch_error_event(ResponseDispatchErrorEventRecord {
            usage_records: &self.usage_records,
            request_id,
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

fn strip_request_history(request: &mut CodexResponsesRequest) {
    request.set_previous_response_id(None);
    request.turn_state = None;
}
