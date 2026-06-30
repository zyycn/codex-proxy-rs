//! Responses 创建编排与调度服务。
//!
//! 包含了将 OpenAI 请求调度到 Codex 上游账号的完整逻辑，包括：
//! - 响应创建（非流式 / 流式 / compact）
//! - 会话亲和性与隐式续接
//! - reasoning replay
//! - 账号回退与错误恢复
//! - 配额验证

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

use chrono::{DateTime, Duration, Utc};
use futures::{stream::Stream, StreamExt};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing;

use crate::{
    admin::monitoring::{
        usage_record::{ResponseUsageRecord, UsageRecord, UsageRecordLevel},
        usage_record_store::AdminUsageRecordService,
    },
    proxy::dispatch::{
        cloudflare::{
            cloudflare_challenge_error_message, cloudflare_path_block_error_message,
            is_cloudflare_challenge_upstream_error, is_cloudflare_path_block_upstream_error,
            CloudflareRecovery,
        },
        errors::{
            auth_failure_account_status, backend_transport_name, is_auth_upstream_error,
            is_history_recovery_signal, is_history_recovery_upstream_error,
            is_invalid_encrypted_content_signal, is_model_unsupported_signal,
            is_model_unsupported_upstream_error, is_quota_exhausted_upstream_error,
            is_rate_limit_upstream_error, rate_limit_cooldown_until, upstream_error_body,
            upstream_error_http_status, upstream_error_set_cookie_headers,
        },
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
    },
    upstream::accounts::{
        model::{Account, AccountStatus},
        pool::{AccountAcquireRequest, RuntimeAccountPoolService},
    },
    upstream::{
        models::ModelCatalog,
        protocol::{
            events::{extract_sse_usage, extract_usage, TokenUsage},
            responses::{
                apply_response_model_options, completed_response_metadata,
                reconvert_responses_sse_event_tuple_values, response_body_has_first_event,
                response_from_codex_sse, CodexCompactRequest, CodexResponsesRequest,
                CollectedResponse, ResponsesSseFailure,
            },
            sse::{
                encode_sse_event, parse_sse_events, sse_body_has_done, SseError, DONE_SSE_FRAME,
            },
        },
        transport::{
            backend_transport_for_response_request, is_banned_auth_signal,
            is_banned_upstream_error, CodexBackendClient, CodexBackendResponse,
            CodexBackendSseStream, CodexBackendTransport, CodexClientError,
            CodexRateLimitHeaderUpdates, CodexTurnStateUpdate, WebSocketPoolDecision,
        },
    },
};

use super::implicit_resume::{
    continuation_input_start, implicit_resume_allowed, ImplicitResumeSnapshot,
};

use crate::proxy::openai::responses::response_failed_sse_event_with_id;

#[derive(Clone, Copy)]
enum ExhaustedAccountClass {
    QuotaExhausted,
    RateLimited,
    Expired,
    Disabled,
    Banned,
    CloudflareChallenge,
    CloudflarePathBlocked,
    ModelUnsupported,
}

/// OpenAI Responses 调度服务。
#[derive(Clone)]
pub struct ResponseDispatchService {
    account_pool: Arc<RuntimeAccountPoolService>,
    models: Arc<crate::upstream::models::ModelService>,
    codex: Arc<CodexBackendClient>,
    session_affinity: Arc<RuntimeSessionAffinityService>,
    reasoning_replay: Arc<Mutex<ReasoningReplayCache>>,
    usage_records: Arc<AdminUsageRecordService>,
    installation_id: Option<String>,
    cloudflare: CloudflareRecovery,
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

struct MpscResponseBodyStream {
    receiver: mpsc::Receiver<Result<Bytes, ResponseDispatchStreamError>>,
    cancel: Option<oneshot::Sender<()>>,
}

impl Drop for MpscResponseBodyStream {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

impl Stream for MpscResponseBodyStream {
    type Item = Result<Bytes, ResponseDispatchStreamError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

use axum::body::Bytes;

impl ResponseDispatchService {
    pub(crate) fn new(
        account_pool: Arc<RuntimeAccountPoolService>,
        models: Arc<crate::upstream::models::ModelService>,
        codex: Arc<CodexBackendClient>,
        session_affinity: Arc<RuntimeSessionAffinityService>,
        usage_records: Arc<AdminUsageRecordService>,
        installation_id: Option<String>,
        cloudflare: CloudflareRecovery,
    ) -> Self {
        Self {
            account_pool,
            models,
            codex,
            session_affinity,
            reasoning_replay: Arc::new(Mutex::new(ReasoningReplayCache::new(Duration::seconds(
                DEFAULT_REASONING_REPLAY_TTL_SECS,
            )))),
            usage_records,
            installation_id,
            cloudflare,
        }
    }

    async fn prepare_response_session(
        &self,
        request: &mut CodexResponsesRequest,
    ) -> Option<ImplicitResumeSnapshot> {
        prepare_variant_identity(request);
        if let Some(previous_response_id) = request.previous_response_id.clone() {
            if request.prompt_cache_key.is_none() {
                request.prompt_cache_key = self
                    .session_affinity
                    .lookup_conversation_id(&previous_response_id, Utc::now())
                    .await;
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
        let continuation_start = continuation_input_start(&request.input);
        if continuation_start == 0 || continuation_start >= request.input.len() {
            return None;
        }
        let conversation_id = request
            .prompt_cache_key
            .as_deref()
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
        let current_instructions_hash = hash_instructions(Some(&request.instructions));
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
            &request.input[continuation_start..],
            &request.input,
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
        let continuation = request.input[continuation_start..].to_vec();
        let mut input = replay_items;
        input.extend(continuation);

        request.previous_response_id = Some(previous_response_id.clone());
        request.use_websocket = true;
        request.force_http_sse = false;
        request.input = input;
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
        let previous_response_id = request.previous_response_id.as_deref()?;
        self.session_affinity
            .lookup_account(previous_response_id, now)
            .await
    }

    async fn recover_request_history(
        &self,
        request: &mut CodexResponsesRequest,
        implicit_resume: &mut Option<ImplicitResumeSnapshot>,
    ) {
        if let Some(previous_response_id) = request.previous_response_id.as_deref() {
            self.session_affinity.forget(previous_response_id).await;
        }
        if let Some(snapshot) = implicit_resume.take() {
            snapshot.restore(request);
            request.previous_response_id = None;
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
        let has_history = request.previous_response_id.is_some()
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
            .or(request.previous_response_id.as_deref())
            .map(str::to_string);
        if let Some(snapshot) = implicit_resume.take() {
            snapshot.restore(request);
            request.previous_response_id = None;
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
        let parsed_model = catalog.parse_model_name(requested_model);
        let display_model = ModelCatalog::build_display_model_name(&parsed_model);
        apply_response_model_options(&mut request, &parsed_model);
        let tuple_schema = request.tuple_schema.clone();
        let image_generation_requested = request.expects_image_generation();
        let now = Utc::now();
        let explicit_previous_response_id = request.previous_response_id.clone();
        let mut implicit_resume = self.prepare_response_session(&mut request).await;
        let preferred_account_id = self.preferred_account_id_for_request(&request, now).await;
        let mut acquire_request = AccountAcquireRequest::new(&request.model, now);
        if let Some(preferred_account_id) = preferred_account_id.as_deref() {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        let mut excluded_account_ids = Vec::new();
        let mut rate_limited_count = 0usize;
        let mut last_rate_limit_error = None;
        let mut quota_exhausted_count = 0usize;
        let mut last_quota_error = None;
        let mut expired_count = 0usize;
        let mut last_auth_error = None;
        let mut disabled_count = 0usize;
        let mut last_disabled_auth_error = None;
        let mut banned_count = 0usize;
        let mut last_banned_auth_error = None;
        let mut last_banned_status_code: Option<u16> = None;
        let mut cloudflare_challenge_count = 0usize;
        let mut last_cloudflare_challenge_error = None;
        let mut cloudflare_path_block_count = 0usize;
        let mut last_cloudflare_path_block_error = None;
        let mut model_unsupported_count = 0usize;
        let mut last_model_unsupported_error = None;
        let mut model_unsupported_retry_used = false;
        let mut history_recovery_used = false;
        let mut last_exhausted_account_class = None;
        let mut empty_response_retries = 0u8;
        let mut quota_verify_attempts = 0usize;
        let mut last_attempted_account_id = None;
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
                let error = match last_exhausted_account_class {
                    Some(ExhaustedAccountClass::QuotaExhausted) => {
                        ResponseDispatchError::QuotaExhausted {
                            count: quota_exhausted_count,
                            upstream_error: last_quota_error.unwrap_or_default(),
                        }
                    }
                    Some(ExhaustedAccountClass::RateLimited) => {
                        ResponseDispatchError::RateLimited {
                            count: rate_limited_count,
                            upstream_error: last_rate_limit_error.unwrap_or_default(),
                        }
                    }
                    Some(ExhaustedAccountClass::Expired) => ResponseDispatchError::Expired {
                        count: expired_count,
                        upstream_error: last_auth_error.unwrap_or_default(),
                    },
                    Some(ExhaustedAccountClass::Disabled) => ResponseDispatchError::Disabled {
                        count: disabled_count,
                        upstream_error: last_disabled_auth_error.unwrap_or_default(),
                    },
                    Some(ExhaustedAccountClass::Banned) => ResponseDispatchError::Banned {
                        count: banned_count,
                        upstream_error: last_banned_auth_error.unwrap_or_default(),
                        status_code: last_banned_status_code.unwrap_or(403),
                    },
                    Some(ExhaustedAccountClass::CloudflareChallenge) => {
                        ResponseDispatchError::CloudflareChallenge {
                            count: cloudflare_challenge_count,
                            upstream_error: last_cloudflare_challenge_error.unwrap_or_default(),
                        }
                    }
                    Some(ExhaustedAccountClass::CloudflarePathBlocked) => {
                        ResponseDispatchError::CloudflarePathBlocked {
                            count: cloudflare_path_block_count,
                            upstream_error: last_cloudflare_path_block_error.unwrap_or_default(),
                        }
                    }
                    Some(ExhaustedAccountClass::ModelUnsupported) => {
                        ResponseDispatchError::ModelUnsupported {
                            count: model_unsupported_count,
                            upstream_error: last_model_unsupported_error.unwrap_or_default(),
                        }
                    }
                    None => ResponseDispatchError::NoActiveAccount,
                };
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        account_id: last_attempted_account_id.as_deref(),
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
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(QUOTA_VERIFY_LIMIT_REACHED_MESSAGE.to_string());
                    last_exhausted_account_class = Some(ExhaustedAccountClass::RateLimited);
                    continue;
                }
                QuotaVerificationDecision::MaxAttemptsReached => {
                    let error = ResponseDispatchError::RateLimited {
                        count: rate_limited_count + 1,
                        upstream_error: QUOTA_VERIFY_LIMIT_REACHED_MESSAGE.to_string(),
                    };
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
            last_attempted_account_id = Some(release_account_id.clone());
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
                                &request.model,
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
                            if model_unsupported_retry_used {
                                let error = ResponseDispatchError::ModelUnsupported {
                                    count: model_unsupported_count + 1,
                                    upstream_error,
                                };
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
                            model_unsupported_count += 1;
                            last_model_unsupported_error = Some(upstream_error);
                            last_exhausted_account_class =
                                Some(ExhaustedAccountClass::ModelUnsupported);
                            model_unsupported_retry_used = true;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        if is_quota_exhausted_sse_failure(failure) {
                            quota_exhausted_count += 1;
                            last_quota_error = Some(failure.message.clone());
                            last_exhausted_account_class =
                                Some(ExhaustedAccountClass::QuotaExhausted);
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        if is_auth_sse_failure(failure) {
                            let upstream_error = sse_failure_error_body(failure);
                            let account_status = auth_sse_failure_account_status(failure);
                            match account_status {
                                AccountStatus::Disabled => {
                                    disabled_count += 1;
                                    last_disabled_auth_error = Some(upstream_error);
                                    last_exhausted_account_class =
                                        Some(ExhaustedAccountClass::Disabled);
                                }
                                AccountStatus::Banned => {
                                    banned_count += 1;
                                    last_banned_status_code =
                                        Some(stream_failure_http_status(failure));
                                    last_banned_auth_error = Some(upstream_error);
                                    last_exhausted_account_class =
                                        Some(ExhaustedAccountClass::Banned);
                                }
                                _ => {
                                    expired_count += 1;
                                    last_auth_error = Some(upstream_error);
                                    last_exhausted_account_class =
                                        Some(ExhaustedAccountClass::Expired);
                                }
                            }
                            self.account_pool
                                .set_status(&release_account_id, account_status)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                    }
                    break (account, response, collected_response);
                }
                Err(error) if is_rate_limit_upstream_error(&error) => {
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::RateLimited);
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    quota_exhausted_count += 1;
                    last_quota_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::QuotaExhausted);
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
                    match account_status {
                        AccountStatus::Disabled => {
                            disabled_count += 1;
                            last_disabled_auth_error = Some(upstream_error);
                            last_exhausted_account_class = Some(ExhaustedAccountClass::Disabled);
                        }
                        AccountStatus::Banned => {
                            banned_count += 1;
                            last_banned_status_code = Some(upstream_error_http_status(&error));
                            last_banned_auth_error = Some(upstream_error);
                            last_exhausted_account_class = Some(ExhaustedAccountClass::Banned);
                        }
                        _ => {
                            expired_count += 1;
                            last_auth_error = Some(upstream_error);
                            last_exhausted_account_class = Some(ExhaustedAccountClass::Expired);
                        }
                    }
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    cloudflare_challenge_count += 1;
                    last_cloudflare_challenge_error =
                        Some(cloudflare_challenge_error_message().to_string());
                    last_exhausted_account_class = Some(ExhaustedAccountClass::CloudflareChallenge);
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    cloudflare_path_block_count += 1;
                    last_cloudflare_path_block_error =
                        Some(cloudflare_path_block_error_message().to_string());
                    last_exhausted_account_class =
                        Some(ExhaustedAccountClass::CloudflarePathBlocked);
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    if model_unsupported_retry_used {
                        let error = ResponseDispatchError::ModelUnsupported {
                            count: model_unsupported_count + 1,
                            upstream_error,
                        };
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
                    model_unsupported_count += 1;
                    last_model_unsupported_error = Some(upstream_error);
                    last_exhausted_account_class = Some(ExhaustedAccountClass::ModelUnsupported);
                    model_unsupported_retry_used = true;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    banned_count += 1;
                    last_banned_status_code = Some(upstream_error_http_status(&error));
                    last_banned_auth_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::Banned);
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
                            &request.model,
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
                    service_tier: request.service_tier.as_deref(),
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
        let parsed_model = catalog.parse_model_name(requested_model);
        let display_model = ModelCatalog::build_display_model_name(&parsed_model);
        apply_response_model_options(&mut request, &parsed_model);
        request.stream = true;
        let tuple_schema = request.tuple_schema.clone();
        let now = Utc::now();
        let explicit_previous_response_id = request.previous_response_id.clone();
        let mut implicit_resume = self.prepare_response_session(&mut request).await;
        let preferred_account_id = self.preferred_account_id_for_request(&request, now).await;
        let mut acquire_request = AccountAcquireRequest::new(&request.model, now);
        if let Some(preferred_account_id) = preferred_account_id.as_deref() {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        let mut excluded_account_ids = Vec::new();
        let mut rate_limited_count = 0usize;
        let mut last_rate_limit_error = None;
        let mut quota_exhausted_count = 0usize;
        let mut last_quota_error = None;
        let mut expired_count = 0usize;
        let mut last_auth_error = None;
        let mut disabled_count = 0usize;
        let mut last_disabled_auth_error = None;
        let mut banned_count = 0usize;
        let mut last_banned_auth_error = None;
        let mut last_banned_status_code: Option<u16> = None;
        let mut cloudflare_challenge_count = 0usize;
        let mut last_cloudflare_challenge_error = None;
        let mut cloudflare_path_block_count = 0usize;
        let mut last_cloudflare_path_block_error = None;
        let mut model_unsupported_count = 0usize;
        let mut last_model_unsupported_error = None;
        let mut model_unsupported_retry_used = false;
        let mut history_recovery_used = false;
        let mut last_exhausted_account_class = None;
        let mut quota_verify_attempts = 0usize;
        let mut last_attempted_account_id = None::<String>;
        macro_rules! return_stream_dispatch_error {
            ($error:expr) => {{
                let error = $error;
                self.record_response_dispatch_error(
                    request_id,
                    route,
                    requested_model,
                    started_at,
                    ResponseDispatchErrorDetails {
                        account_id: last_attempted_account_id.as_deref(),
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
                let error = match last_exhausted_account_class {
                    Some(ExhaustedAccountClass::QuotaExhausted) => {
                        ResponseDispatchError::QuotaExhausted {
                            count: quota_exhausted_count,
                            upstream_error: last_quota_error.unwrap_or_default(),
                        }
                    }
                    Some(ExhaustedAccountClass::RateLimited) => {
                        ResponseDispatchError::RateLimited {
                            count: rate_limited_count,
                            upstream_error: last_rate_limit_error.unwrap_or_default(),
                        }
                    }
                    Some(ExhaustedAccountClass::Expired) => ResponseDispatchError::Expired {
                        count: expired_count,
                        upstream_error: last_auth_error.unwrap_or_default(),
                    },
                    Some(ExhaustedAccountClass::Disabled) => ResponseDispatchError::Disabled {
                        count: disabled_count,
                        upstream_error: last_disabled_auth_error.unwrap_or_default(),
                    },
                    Some(ExhaustedAccountClass::Banned) => ResponseDispatchError::Banned {
                        count: banned_count,
                        upstream_error: last_banned_auth_error.unwrap_or_default(),
                        status_code: last_banned_status_code.unwrap_or(403),
                    },
                    Some(ExhaustedAccountClass::CloudflareChallenge) => {
                        ResponseDispatchError::CloudflareChallenge {
                            count: cloudflare_challenge_count,
                            upstream_error: last_cloudflare_challenge_error.unwrap_or_default(),
                        }
                    }
                    Some(ExhaustedAccountClass::CloudflarePathBlocked) => {
                        ResponseDispatchError::CloudflarePathBlocked {
                            count: cloudflare_path_block_count,
                            upstream_error: last_cloudflare_path_block_error.unwrap_or_default(),
                        }
                    }
                    Some(ExhaustedAccountClass::ModelUnsupported) => {
                        ResponseDispatchError::ModelUnsupported {
                            count: model_unsupported_count,
                            upstream_error: last_model_unsupported_error.unwrap_or_default(),
                        }
                    }
                    None => ResponseDispatchError::NoActiveAccount,
                };
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
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(QUOTA_VERIFY_LIMIT_REACHED_MESSAGE.to_string());
                    last_exhausted_account_class = Some(ExhaustedAccountClass::RateLimited);
                    continue;
                }
                QuotaVerificationDecision::MaxAttemptsReached => {
                    return_stream_dispatch_error!(
                        ResponseDispatchError::RateLimited {
                            count: rate_limited_count + 1,
                            upstream_error: QUOTA_VERIFY_LIMIT_REACHED_MESSAGE.to_string(),
                        },
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
            last_attempted_account_id = Some(release_account_id.clone());
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
                            if model_unsupported_retry_used {
                                self.account_pool.release(&release_account_id).await;
                                return_stream_dispatch_error!(
                                    ResponseDispatchError::ModelUnsupported {
                                        count: model_unsupported_count + 1,
                                        upstream_error,
                                    },
                                    account_id: Some(&release_account_id),
                                    transport: Some(backend_transport_name(transport))
                                );
                            }
                            model_unsupported_count += 1;
                            last_model_unsupported_error = Some(upstream_error);
                            model_unsupported_retry_used = true;
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        if is_quota_exhausted_sse_failure(&failure) {
                            quota_exhausted_count += 1;
                            last_quota_error = Some(failure.message.clone());
                            last_exhausted_account_class =
                                Some(ExhaustedAccountClass::QuotaExhausted);
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
                            match account_status {
                                AccountStatus::Disabled => {
                                    disabled_count += 1;
                                    last_disabled_auth_error = Some(upstream_error);
                                    last_exhausted_account_class =
                                        Some(ExhaustedAccountClass::Disabled);
                                }
                                AccountStatus::Banned => {
                                    banned_count += 1;
                                    last_banned_status_code =
                                        Some(stream_failure_http_status(&failure));
                                    last_banned_auth_error = Some(upstream_error);
                                    last_exhausted_account_class =
                                        Some(ExhaustedAccountClass::Banned);
                                }
                                _ => {
                                    expired_count += 1;
                                    last_auth_error = Some(upstream_error);
                                    last_exhausted_account_class =
                                        Some(ExhaustedAccountClass::Expired);
                                }
                            }
                            self.account_pool
                                .set_status(&release_account_id, account_status)
                                .await;
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
                        model: request.model.clone(),
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
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::RateLimited);
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    quota_exhausted_count += 1;
                    last_quota_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::QuotaExhausted);
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
                    match account_status {
                        AccountStatus::Disabled => {
                            disabled_count += 1;
                            last_disabled_auth_error = Some(upstream_error);
                            last_exhausted_account_class = Some(ExhaustedAccountClass::Disabled);
                        }
                        AccountStatus::Banned => {
                            banned_count += 1;
                            last_banned_status_code = Some(upstream_error_http_status(&error));
                            last_banned_auth_error = Some(upstream_error);
                            last_exhausted_account_class = Some(ExhaustedAccountClass::Banned);
                        }
                        _ => {
                            expired_count += 1;
                            last_auth_error = Some(upstream_error);
                            last_exhausted_account_class = Some(ExhaustedAccountClass::Expired);
                        }
                    }
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    cloudflare_challenge_count += 1;
                    last_cloudflare_challenge_error =
                        Some(cloudflare_challenge_error_message().to_string());
                    last_exhausted_account_class = Some(ExhaustedAccountClass::CloudflareChallenge);
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    cloudflare_path_block_count += 1;
                    last_cloudflare_path_block_error =
                        Some(cloudflare_path_block_error_message().to_string());
                    last_exhausted_account_class =
                        Some(ExhaustedAccountClass::CloudflarePathBlocked);
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    let upstream_error = upstream_error_body(&error);
                    if model_unsupported_retry_used {
                        return_stream_dispatch_error!(
                            ResponseDispatchError::ModelUnsupported {
                                count: model_unsupported_count + 1,
                                upstream_error,
                            },
                            account_id: Some(&release_account_id),
                            transport: Some(backend_transport_name(backend_transport_for_response_request(
                                &request
                            )))
                        );
                    }
                    model_unsupported_count += 1;
                    last_model_unsupported_error = Some(upstream_error);
                    last_exhausted_account_class = Some(ExhaustedAccountClass::ModelUnsupported);
                    model_unsupported_retry_used = true;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    banned_count += 1;
                    last_banned_status_code = Some(upstream_error_http_status(&error));
                    last_banned_auth_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::Banned);
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
        let parsed_model = catalog.parse_model_name(requested_model);
        let display_model = ModelCatalog::build_display_model_name(&parsed_model);
        request.model = parsed_model.model_id;
        let mut excluded_account_ids = Vec::new();
        let mut rate_limited_count = 0usize;
        let mut last_rate_limit_error = None;
        let mut quota_exhausted_count = 0usize;
        let mut last_quota_error = None;
        let mut expired_count = 0usize;
        let mut last_auth_error = None;
        let mut disabled_count = 0usize;
        let mut last_disabled_auth_error = None;
        let mut banned_count = 0usize;
        let mut last_banned_auth_error = None;
        let mut last_banned_status_code: Option<u16> = None;
        let mut cloudflare_challenge_count = 0usize;
        let mut last_cloudflare_challenge_error = None;
        let mut cloudflare_path_block_count = 0usize;
        let mut last_cloudflare_path_block_error = None;
        let mut model_unsupported_count = 0usize;
        let mut last_model_unsupported_error = None;
        let mut model_unsupported_retry_used = false;
        let mut quota_verify_attempts = 0usize;
        let mut last_attempted_account_id = None::<String>;

        loop {
            let acquire_request = AccountAcquireRequest::new(&request.model, Utc::now())
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            let acquired = match self.account_pool.acquire_with(&acquire_request).await {
                Some(acquired) => acquired,
                None if quota_exhausted_count > 0 => {
                    let error = ResponseDispatchError::QuotaExhausted {
                        count: quota_exhausted_count,
                        upstream_error: last_quota_error.unwrap_or_default(),
                    };
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
                None if rate_limited_count > 0 => {
                    let error = ResponseDispatchError::RateLimited {
                        count: rate_limited_count,
                        upstream_error: last_rate_limit_error.unwrap_or_default(),
                    };
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
                None if expired_count > 0 => {
                    let error = ResponseDispatchError::Expired {
                        count: expired_count,
                        upstream_error: last_auth_error.unwrap_or_default(),
                    };
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
                None if disabled_count > 0 => {
                    let error = ResponseDispatchError::Disabled {
                        count: disabled_count,
                        upstream_error: last_disabled_auth_error.unwrap_or_default(),
                    };
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
                None if banned_count > 0 => {
                    let error = ResponseDispatchError::Banned {
                        count: banned_count,
                        upstream_error: last_banned_auth_error.unwrap_or_default(),
                        status_code: last_banned_status_code.unwrap_or(403),
                    };
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
                None if cloudflare_challenge_count > 0 => {
                    let error = ResponseDispatchError::CloudflareChallenge {
                        count: cloudflare_challenge_count,
                        upstream_error: last_cloudflare_challenge_error.unwrap_or_default(),
                    };
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
                None if cloudflare_path_block_count > 0 => {
                    let error = ResponseDispatchError::CloudflarePathBlocked {
                        count: cloudflare_path_block_count,
                        upstream_error: last_cloudflare_path_block_error.unwrap_or_default(),
                    };
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
                None if model_unsupported_count > 0 => {
                    let error = ResponseDispatchError::ModelUnsupported {
                        count: model_unsupported_count,
                        upstream_error: last_model_unsupported_error.unwrap_or_default(),
                    };
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
                None => {
                    let error = ResponseDispatchError::NoActiveAccount;
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
                        &error,
                    )
                    .await;
                    return Err(error);
                }
            };
            last_attempted_account_id = Some(acquired.account.id.clone());
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
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(QUOTA_VERIFY_LIMIT_REACHED_MESSAGE.to_string());
                    continue;
                }
                QuotaVerificationDecision::MaxAttemptsReached => {
                    let error = ResponseDispatchError::RateLimited {
                        count: rate_limited_count + 1,
                        upstream_error: QUOTA_VERIFY_LIMIT_REACHED_MESSAGE.to_string(),
                    };
                    self.record_compact_dispatch_error(
                        request_id,
                        requested_model,
                        started_at,
                        last_attempted_account_id.as_deref(),
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
                            .record_token_usage(&account.id, &request.model, &usage)
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
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(upstream_error_body(&error));
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    quota_exhausted_count += 1;
                    last_quota_error = Some(upstream_error_body(&error));
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_auth_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    let account_status = auth_failure_account_status(&error);
                    match account_status {
                        AccountStatus::Disabled => {
                            disabled_count += 1;
                            last_disabled_auth_error = Some(upstream_error);
                        }
                        AccountStatus::Banned => {
                            banned_count += 1;
                            last_banned_status_code = Some(upstream_error_http_status(&error));
                            last_banned_auth_error = Some(upstream_error);
                        }
                        _ => {
                            expired_count += 1;
                            last_auth_error = Some(upstream_error);
                        }
                    }
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    cloudflare_challenge_count += 1;
                    last_cloudflare_challenge_error =
                        Some(cloudflare_challenge_error_message().to_string());
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    cloudflare_path_block_count += 1;
                    last_cloudflare_path_block_error =
                        Some(cloudflare_path_block_error_message().to_string());
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    if model_unsupported_retry_used {
                        let error = ResponseDispatchError::ModelUnsupported {
                            count: model_unsupported_count + 1,
                            upstream_error,
                        };
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
                    model_unsupported_count += 1;
                    last_model_unsupported_error = Some(upstream_error);
                    model_unsupported_retry_used = true;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    banned_count += 1;
                    last_banned_status_code = Some(upstream_error_http_status(&error));
                    last_banned_auth_error = Some(upstream_error_body(&error));
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

fn sse_failure_error_body(failure: &ResponsesSseFailure) -> String {
    match failure.upstream_code.as_deref() {
        Some(code) => serde_json::json!({
            "error": {
                "code": code,
                "message": failure.message.as_str(),
            }
        })
        .to_string(),
        None => failure.message.clone(),
    }
}

fn is_quota_exhausted_sse_failure(failure: &ResponsesSseFailure) -> bool {
    failure
        .upstream_code
        .as_deref()
        .is_some_and(|code| matches!(code, "quota_exceeded" | "insufficient_quota"))
        || failure.message.to_ascii_lowercase().contains("quota")
}

fn is_auth_sse_failure(failure: &ResponsesSseFailure) -> bool {
    failure.upstream_code.as_deref().is_some_and(|code| {
        let code = code.to_ascii_lowercase();
        matches!(
            code.as_str(),
            "token_invalid"
                | "token_expired"
                | "token_revoked"
                | "account_deactivated"
                | "unauthorized"
                | "invalid_api_key"
        )
    }) || {
        let message = failure.message.to_ascii_lowercase();
        message.contains("token revoked")
            || message.contains("token invalid")
            || message.contains("token expired")
    }
}

fn is_model_unsupported_sse_failure(failure: &ResponsesSseFailure) -> bool {
    failure
        .upstream_code
        .as_deref()
        .is_some_and(is_model_unsupported_signal)
        || is_model_unsupported_signal(&failure.message)
}

fn is_history_recovery_sse_failure(failure: &ResponsesSseFailure) -> bool {
    failure
        .upstream_code
        .as_deref()
        .is_some_and(is_history_recovery_signal)
        || is_history_recovery_signal(&failure.message)
}

fn sse_failure_invalid_reasoning_replay(failure: &ResponsesSseFailure) -> bool {
    failure
        .upstream_code
        .as_deref()
        .is_some_and(is_invalid_encrypted_content_signal)
        || is_invalid_encrypted_content_signal(&failure.message)
}

fn client_error_invalid_reasoning_replay(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { body, .. } if is_invalid_encrypted_content_signal(body)
    )
}

fn auth_sse_failure_account_status(failure: &ResponsesSseFailure) -> AccountStatus {
    if failure
        .upstream_code
        .as_deref()
        .is_some_and(is_banned_auth_signal)
        || is_banned_auth_signal(&failure.message)
    {
        AccountStatus::Banned
    } else {
        AccountStatus::Expired
    }
}

fn strip_request_history(request: &mut CodexResponsesRequest) {
    request.previous_response_id = None;
    request.turn_state = None;
}

const MAX_STREAM_PREFETCH_BYTES: usize = 64 * 1024;

async fn prefetch_first_sse_chunk(
    mut body: CodexBackendSseStream,
) -> Result<(Bytes, CodexBackendSseStream), ResponseDispatchError> {
    let mut prefetched = Vec::new();
    while !contains_sse_event_separator(&prefetched) {
        let Some(next) = body.next().await else {
            if prefetched.is_empty() {
                return Err(ResponseDispatchError::EmptyUpstreamResponse);
            }
            return Err(ResponseDispatchError::MissingCompleted);
        };
        let chunk = next.map_err(ResponseDispatchError::Upstream)?;
        prefetched.extend_from_slice(&chunk);
        if prefetched.len() > MAX_STREAM_PREFETCH_BYTES {
            return Err(ResponseDispatchError::InvalidSse(
                SseError::BufferExceeded {
                    max_bytes: MAX_STREAM_PREFETCH_BYTES,
                },
            ));
        }
    }

    Ok((Bytes::from(prefetched), body))
}

fn contains_sse_event_separator(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|window| window == b"\n\n")
        || bytes.windows(4).any(|window| window == b"\r\n\r\n")
}

fn first_sse_failure(prefetched: &[u8]) -> Result<Option<ResponsesSseFailure>, SseError> {
    let body = String::from_utf8_lossy(prefetched);
    match response_from_codex_sse(&body, None)? {
        CollectedResponse::Failed(failure) => Ok(Some(failure)),
        CollectedResponse::Completed(_)
        | CollectedResponse::MissingCompleted
        | CollectedResponse::Empty => Ok(None),
    }
}

struct LiveResponseStreamContext {
    account_pool: Arc<RuntimeAccountPoolService>,
    session_affinity: Arc<RuntimeSessionAffinityService>,
    reasoning_replay: Arc<Mutex<ReasoningReplayCache>>,
    usage_records: Arc<AdminUsageRecordService>,
    cloudflare: CloudflareRecovery,
    account_id: String,
    account_plan_type: Option<String>,
    request_id: String,
    route: String,
    model: String,
    display_model: String,
    requested_model: String,
    client_ip: Option<String>,
    request: CodexResponsesRequest,
    tuple_schema: Option<Value>,
    transport: CodexBackendTransport,
    rate_limit_headers: Vec<(String, String)>,
    rate_limit_header_updates: Option<CodexRateLimitHeaderUpdates>,
    turn_state_update: Option<CodexTurnStateUpdate>,
    websocket_pool_decision: Option<WebSocketPoolDecision>,
    turn_state: Option<String>,
    started_at: Instant,
}

fn spawn_live_response_stream(
    context: LiveResponseStreamContext,
    prefetched: Bytes,
    mut body: CodexBackendSseStream,
) -> ResponseDispatchStream {
    let (sender, receiver) = mpsc::channel(8);
    let (cancel_sender, mut cancel_receiver) = oneshot::channel();
    tokio::spawn(async move {
        let mut tuple_transformer = context
            .tuple_schema
            .clone()
            .map(TupleSseEventTransformer::new);
        let mut body_bytes = Vec::new();
        let mut first_token_ms = None;
        if !send_live_response_stream_chunk(
            &sender,
            &mut body_bytes,
            tuple_transformer.as_mut(),
            prefetched,
        )
        .await
        {
            context.account_pool.release(&context.account_id).await;
            return;
        }
        update_first_token_ms(&context, &body_bytes, &mut first_token_ms);

        loop {
            let next = tokio::select! {
                _ = &mut cancel_receiver => {
                    context.account_pool.release(&context.account_id).await;
                    return;
                }
                next = body.next() => next,
            };
            let Some(next) = next else {
                break;
            };
            match next {
                Ok(chunk) => {
                    if !send_live_response_stream_chunk(
                        &sender,
                        &mut body_bytes,
                        tuple_transformer.as_mut(),
                        chunk,
                    )
                    .await
                    {
                        context.account_pool.release(&context.account_id).await;
                        return;
                    }
                    update_first_token_ms(&context, &body_bytes, &mut first_token_ms);
                }
                Err(error) => {
                    if !flush_live_response_stream_transformer(
                        &sender,
                        &mut body_bytes,
                        tuple_transformer.as_mut(),
                    )
                    .await
                    {
                        context.account_pool.release(&context.account_id).await;
                        return;
                    }
                    let detail = error.to_string();
                    let Some(body_text) =
                        send_live_response_stream_tail(&sender, &mut body_bytes, Some(&detail))
                            .await
                    else {
                        context.account_pool.release(&context.account_id).await;
                        return;
                    };
                    finalize_live_response_stream(context, body_text, first_token_ms).await;
                    return;
                }
            }
        }

        if !flush_live_response_stream_transformer(
            &sender,
            &mut body_bytes,
            tuple_transformer.as_mut(),
        )
        .await
        {
            context.account_pool.release(&context.account_id).await;
            return;
        }
        let Some(body_text) = send_live_response_stream_tail(&sender, &mut body_bytes, None).await
        else {
            context.account_pool.release(&context.account_id).await;
            return;
        };

        finalize_live_response_stream(context, body_text, first_token_ms).await;
    });

    ResponseDispatchStream {
        body: Box::pin(MpscResponseBodyStream {
            receiver,
            cancel: Some(cancel_sender),
        }),
    }
}

fn update_first_token_ms(
    context: &LiveResponseStreamContext,
    body_bytes: &[u8],
    first_token_ms: &mut Option<i64>,
) {
    if first_token_ms.is_none() && response_body_has_first_event(body_bytes) {
        *first_token_ms = Some(elapsed_millis_i64(context.started_at).max(1));
    }
}

async fn send_live_response_stream_chunk(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    transformer: Option<&mut TupleSseEventTransformer>,
    chunk: Bytes,
) -> bool {
    let chunks = match transformer {
        Some(transformer) => transformer.push(&chunk),
        None => vec![chunk],
    };
    send_live_response_stream_chunks(sender, body_bytes, chunks).await
}

async fn flush_live_response_stream_transformer(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    transformer: Option<&mut TupleSseEventTransformer>,
) -> bool {
    let Some(transformer) = transformer else {
        return true;
    };
    send_live_response_stream_chunks(sender, body_bytes, transformer.finish()).await
}

async fn send_live_response_stream_chunks(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    chunks: Vec<Bytes>,
) -> bool {
    for chunk in chunks {
        body_bytes.extend_from_slice(&chunk);
        if sender.send(Ok(chunk)).await.is_err() {
            return false;
        }
    }
    true
}

struct TupleSseEventTransformer {
    tuple_schema: Value,
    pending: Vec<u8>,
}

impl TupleSseEventTransformer {
    fn new(tuple_schema: Value) -> Self {
        Self {
            tuple_schema,
            pending: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        self.pending.extend_from_slice(chunk);
        let mut chunks = Vec::new();
        while let Some(frame_end) = next_sse_frame_end(&self.pending) {
            let frame = self.pending.drain(..frame_end).collect::<Vec<_>>();
            chunks.push(self.transform_frame(&frame));
        }
        chunks
    }

    fn finish(&mut self) -> Vec<Bytes> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let frame = std::mem::take(&mut self.pending);
        vec![self.transform_frame(&frame)]
    }

    fn transform_frame(&self, frame: &[u8]) -> Bytes {
        let frame_text = String::from_utf8_lossy(frame);
        let Ok(events) = parse_sse_events(&frame_text) else {
            return Bytes::copy_from_slice(frame);
        };
        let [event] = events.as_slice() else {
            return Bytes::copy_from_slice(frame);
        };
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            return Bytes::copy_from_slice(frame);
        };
        let transformed = reconvert_responses_sse_event_tuple_values(
            event.event.as_deref(),
            data,
            &self.tuple_schema,
        );
        Bytes::from(encode_sse_event(
            event.event.as_deref().unwrap_or_default(),
            &transformed.to_string(),
        ))
    }
}

fn next_sse_frame_end(bytes: &[u8]) -> Option<usize> {
    let lf_lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| position + 2);
    let crlf_crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4);
    match (lf_lf, crlf_crlf) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(end), None) | (None, Some(end)) => Some(end),
        (None, None) => None,
    }
}

async fn send_live_response_stream_tail(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    failure_detail: Option<&str>,
) -> Option<String> {
    let mut body_text = String::from_utf8_lossy(body_bytes).to_string();
    if !sse_body_has_terminal_event(&body_text) {
        if let Some(separator) = missing_sse_event_separator(&body_text) {
            body_text.push_str(separator);
            body_bytes.extend_from_slice(separator.as_bytes());
            if sender
                .send(Ok(Bytes::copy_from_slice(separator.as_bytes())))
                .await
                .is_err()
            {
                return None;
            }
        }
        let failure =
            premature_close_failed_event(latest_response_id(&body_text).as_deref(), failure_detail);
        body_text.push_str(&failure);
        body_bytes.extend_from_slice(failure.as_bytes());
        if sender.send(Ok(Bytes::from(failure))).await.is_err() {
            return None;
        }
    }

    if !sse_body_has_done(&body_text) {
        body_text.push_str(DONE_SSE_FRAME);
        body_bytes.extend_from_slice(DONE_SSE_FRAME.as_bytes());
        if sender
            .send(Ok(Bytes::from_static(DONE_SSE_FRAME.as_bytes())))
            .await
            .is_err()
        {
            return None;
        }
    }

    Some(body_text)
}

fn sse_body_has_terminal_event(body: &str) -> bool {
    parse_sse_events(body).is_ok_and(|events| {
        events.iter().any(|event| {
            matches!(
                event.event.as_deref(),
                Some("response.completed" | "response.failed" | "error")
            )
        })
    })
}

fn missing_sse_event_separator(body: &str) -> Option<&'static str> {
    if body.is_empty()
        || body.ends_with("\n\n")
        || body.ends_with("\r\n\r\n")
        || body.ends_with("\r\r")
    {
        None
    } else if body.ends_with('\n') || body.ends_with('\r') {
        Some("\n")
    } else {
        Some("\n\n")
    }
}

fn latest_response_id(body: &str) -> Option<String> {
    parse_sse_events(body).ok().and_then(|events| {
        events.iter().rev().find_map(|event| {
            serde_json::from_str::<Value>(&event.data)
                .ok()
                .and_then(|data| {
                    data.pointer("/response/id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                        .map(ToString::to_string)
                })
        })
    })
}

const STREAM_DISCONNECTED_CODE: &str = "stream_disconnected";
const STREAM_DISCONNECTED_MESSAGE: &str = "Upstream stream closed before response.completed";

fn premature_close_failed_event(response_id: Option<&str>, detail: Option<&str>) -> String {
    let message = match detail.filter(|value| !value.trim().is_empty()) {
        Some(detail) => format!("{STREAM_DISCONNECTED_MESSAGE}: {detail}"),
        None => STREAM_DISCONNECTED_MESSAGE.to_string(),
    };
    response_failed_sse_event_with_id(
        response_id,
        "server_error",
        STREAM_DISCONNECTED_CODE,
        &message,
    )
}

async fn finalize_live_response_stream(
    context: LiveResponseStreamContext,
    body: String,
    first_token_ms: Option<i64>,
) {
    let rate_limit_headers = live_response_rate_limit_headers(&context).await;
    context
        .account_pool
        .sync_passive_rate_limit_headers_for_account(
            &context.account_id,
            context.account_plan_type.as_deref(),
            &rate_limit_headers,
        )
        .await;
    let turn_state = live_response_turn_state(&context).await;
    let usage = match extract_sse_usage(&body) {
        Ok(Some(usage)) => {
            context
                .account_pool
                .record_token_usage(&context.account_id, &context.model, &usage)
                .await;
            Some(usage)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(account_id = %context.account_id, error = %error, "failed to extract streaming token usage");
            None
        }
    };

    match response_from_codex_sse(&body, context.tuple_schema.as_ref()) {
        Ok(CollectedResponse::Completed(completed)) => {
            context
                .cloudflare
                .reset_account_recovery(&context.account_id)
                .await;
            let response_id = completed.get("id").and_then(Value::as_str);
            record_response_affinity(
                &context.session_affinity,
                &context.reasoning_replay,
                &context.request,
                &context.account_id,
                &body,
                turn_state,
                usage,
            )
            .await;
            record_live_response_stream_event(
                &context,
                200,
                UsageRecordLevel::Info,
                "v1 responses stream completed",
                serde_json::json!({
                    "stream": true,
                    "completed": true,
                    "responseId": response_id,
                    "firstTokenMs": first_token_ms,
                    "usage": usage,
                }),
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Ok(CollectedResponse::Failed(failure)) => {
            if sse_failure_invalid_reasoning_replay(&failure) {
                evict_reasoning_replay(
                    &context.reasoning_replay,
                    &context.request,
                    &context.account_id,
                )
                .await;
            }
            let response_id = latest_response_id(&body);
            let latency_ms = elapsed_millis_i64(context.started_at);
            let failure_source = stream_failure_source(&failure);
            let failure_detail = synthetic_stream_disconnected_detail(&failure);
            let websocket_pool_kind = context
                .websocket_pool_decision
                .map(|decision| decision.kind());
            let websocket_pool_reason = context
                .websocket_pool_decision
                .and_then(|decision| decision.reason());
            tracing::warn!(
                account_id = %context.account_id,
                request_id = %context.request_id,
                response_id = response_id.as_deref().unwrap_or(""),
                transport = %backend_transport_name(context.transport),
                websocket_pool_kind = ?websocket_pool_kind,
                websocket_pool_reason = ?websocket_pool_reason,
                first_token_ms = ?first_token_ms,
                latency_ms,
                event = %failure.event,
                code = ?failure.upstream_code.as_deref(),
                failure_source = %failure_source,
                failure_detail = ?failure_detail.as_deref(),
                "live upstream stream ended with response.failed"
            );
            let mut metadata = stream_failure_metadata(&failure, usage);
            insert_first_token_ms(&mut metadata, first_token_ms);
            record_live_response_stream_event(
                &context,
                status_code_for_stream_failure(&failure),
                UsageRecordLevel::Error,
                "v1 responses stream failed",
                metadata,
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Ok(CollectedResponse::MissingCompleted | CollectedResponse::Empty) => {
            tracing::warn!(
                account_id = %context.account_id,
                "live upstream stream ended without response.completed"
            );
            let mut metadata = serde_json::json!({
                "stream": true,
                "failed": true,
                "upstreamCode": "missing_completed",
                "usage": usage,
            });
            insert_first_token_ms(&mut metadata, first_token_ms);
            record_live_response_stream_event(
                &context,
                502,
                UsageRecordLevel::Error,
                "v1 responses stream ended without response.completed",
                metadata,
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Err(error) => {
            tracing::warn!(account_id = %context.account_id, error = %error, "failed to parse completed live stream");
            let mut metadata = serde_json::json!({
                "stream": true,
                "sseParseError": error.to_string(),
                "usage": usage,
            });
            insert_first_token_ms(&mut metadata, first_token_ms);
            record_live_response_stream_event(
                &context,
                502,
                UsageRecordLevel::Warn,
                "v1 responses stream SSE response invalid",
                metadata,
                &rate_limit_headers,
                &body,
            )
            .await;
        }
    }

    context.account_pool.release(&context.account_id).await;
}

// ====================================================================
// Event recording helpers
// ====================================================================

struct ResponseUpstreamErrorEventRecord<'a> {
    usage_records: &'a AdminUsageRecordService,
    request_id: &'a str,
    account_id: &'a str,
    account_email: Option<&'a str>,
    route: &'a str,
    model: &'a str,
    started_at: Instant,
    stream: bool,
    transport: CodexBackendTransport,
    error: &'a CodexClientError,
}

struct ResponseStreamFailureEventRecord<'a> {
    usage_records: &'a AdminUsageRecordService,
    request_id: &'a str,
    account_id: &'a str,
    route: &'a str,
    model: &'a str,
    requested_model: &'a str,
    started_at: Instant,
    transport: CodexBackendTransport,
    request: &'a CodexResponsesRequest,
    failure: &'a ResponsesSseFailure,
    rate_limit_headers: &'a [(String, String)],
    prefetched: &'a [u8],
}

struct ResponseDispatchErrorEventRecord<'a> {
    usage_records: &'a AdminUsageRecordService,
    request_id: &'a str,
    account_id: Option<&'a str>,
    route: &'a str,
    model: &'a str,
    started_at: Instant,
    stream: bool,
    compact: bool,
    transport: Option<&'a str>,
    error: &'a ResponseDispatchError,
}

struct ResponseDispatchErrorDetails<'a> {
    account_id: Option<&'a str>,
    stream: bool,
    compact: bool,
    transport: Option<&'a str>,
}

async fn record_response_dispatch_error_event(record: ResponseDispatchErrorEventRecord<'_>) {
    let mut metadata = dispatch_error_metadata(
        record.error,
        record.stream,
        record.compact,
        record.transport,
    );
    enrich_response_dispatch_error_metadata(&mut metadata, record.error);
    enrich_event_route_metadata(&mut metadata, record.route);
    let mut event = UsageRecord::new(
        response_event_kind(record.route),
        UsageRecordLevel::Error,
        "v1 responses dispatch failed",
    );
    event.request_id = Some(record.request_id.to_string());
    event.account_id = record.account_id.map(ToString::to_string);
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(i64::from(record.error.http_status_code()));
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    event.metadata = metadata;
    if let Err(error) = record.usage_records.record(event).await {
        tracing::warn!(
            account_id = record.account_id.unwrap_or(""),
            error = %error,
            "failed to record response dispatch error event"
        );
    }
}

async fn record_response_upstream_error_event(record: ResponseUpstreamErrorEventRecord<'_>) {
    let mut metadata = dispatch_error_metadata(
        record.error,
        record.stream,
        false,
        Some(backend_transport_name(record.transport)),
    );
    enrich_event_route_metadata(&mut metadata, record.route);
    let mut event = UsageRecord::new(
        "v1.response",
        UsageRecordLevel::Error,
        "v1 responses upstream request failed",
    );
    event.request_id = Some(record.request_id.to_string());
    event.account_id = Some(record.account_id.to_string());
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(i64::from(upstream_error_http_status(record.error)));
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    if let Some(object) = metadata.as_object_mut() {
        if let Some(account_email) = record
            .account_email
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            object.insert(
                "accountEmail".to_string(),
                Value::String(account_email.to_string()),
            );
        }
    }
    event.metadata = metadata;
    if let Err(error) = record.usage_records.record(event).await {
        tracing::warn!(account_id = %record.account_id, error = %error, "failed to record upstream error event");
    }
}

async fn record_prefetched_response_stream_failure_event(
    record: ResponseStreamFailureEventRecord<'_>,
) {
    let mut metadata = stream_failure_metadata(record.failure, None);
    if let Some(object) = metadata.as_object_mut() {
        if record.transport == CodexBackendTransport::WebSocket {
            object.insert(
                "transport".to_string(),
                Value::String("websocket".to_string()),
            );
        }
        object.insert("requestBody".to_string(), json!(record.request));
        object.insert(
            "responseBody".to_string(),
            Value::String(String::from_utf8_lossy(record.prefetched).to_string()),
        );
    }
    record_response_event(ResponseUsageRecord {
        usage_records: record.usage_records,
        request_id: record.request_id,
        account_id: record.account_id,
        route: record.route,
        model: record.model,
        requested_model: Some(record.requested_model),
        client_ip: record.request.client_ip.as_deref(),
        client_user_agent: record.request.client_user_agent.as_deref(),
        reasoning_effort: reasoning_effort_from_request(record.request),
        service_tier: record.request.service_tier.as_deref(),
        started_at: record.started_at,
        status_code: status_code_for_stream_failure(record.failure),
        level: UsageRecordLevel::Error,
        message: "v1 responses stream failed",
        metadata,
        rate_limit_headers: record.rate_limit_headers,
    })
    .await;
}

async fn record_response_event(record: ResponseUsageRecord<'_>) {
    let mut metadata = record.metadata;
    enrich_event_route_metadata(&mut metadata, record.route);
    let mut event = UsageRecord::new(
        response_event_kind(record.route),
        record.level,
        record.message,
    );
    event.request_id = Some(record.request_id.to_string());
    event.account_id = Some(record.account_id.to_string());
    event.route = Some(record.route.to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(record.status_code);
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    enrich_usage_record_identity(
        &mut metadata,
        record.requested_model,
        record.model,
        record.client_ip,
        record.client_user_agent,
        record.reasoning_effort,
        record.service_tier,
    );
    event.metadata = metadata;
    let rate_limit_headers = record.rate_limit_headers;
    if !rate_limit_headers.is_empty() {
        if let Some(object) = event.metadata.as_object_mut() {
            object.insert(
                "rateLimitHeaders".to_string(),
                serde_json::json!(rate_limit_headers),
            );
        }
    }
    if let Err(error) = record.usage_records.record(event).await {
        tracing::warn!(account_id = %record.account_id, error = %error, "failed to record response event");
    }
}

fn response_event_kind(route: &str) -> &'static str {
    if route == "/v1/chat/completions" {
        "v1.chat"
    } else {
        "v1.response"
    }
}

fn response_api_kind(route: &str) -> &'static str {
    if route == "/v1/chat/completions" {
        "chat"
    } else {
        "responses"
    }
}

fn enrich_event_route_metadata(metadata: &mut Value, route: &str) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object
        .entry("route".to_string())
        .or_insert_with(|| Value::String(route.to_string()));
    object
        .entry("apiKind".to_string())
        .or_insert_with(|| Value::String(response_api_kind(route).to_string()));
}

fn enrich_usage_record_identity(
    metadata: &mut Value,
    requested_model: Option<&str>,
    upstream_model: &str,
    client_ip: Option<&str>,
    client_user_agent: Option<&str>,
    reasoning_effort: Option<&str>,
    service_tier: Option<&str>,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };

    let upstream_model = upstream_model.trim();
    let requested_model = requested_model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(upstream_model);
    object.insert(
        "requestedModel".to_string(),
        Value::String(requested_model.to_string()),
    );
    object.insert(
        "upstreamModel".to_string(),
        Value::String(upstream_model.to_string()),
    );

    if let Some(client_ip) = client_ip.map(str::trim).filter(|value| !value.is_empty()) {
        object.insert("clientIp".to_string(), Value::String(client_ip.to_string()));
    }

    if let Some(user_agent) = client_user_agent
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        object.insert(
            "userAgent".to_string(),
            Value::String(user_agent.to_string()),
        );
    }

    if let Some(reasoning_effort) = reasoning_effort
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        object.insert(
            "reasoningEffort".to_string(),
            Value::String(reasoning_effort.to_string()),
        );
    }

    if let Some(service_tier) = service_tier
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        object.insert(
            "serviceTier".to_string(),
            Value::String(service_tier.to_string()),
        );
    }
}

fn reasoning_effort_from_request(request: &CodexResponsesRequest) -> Option<&str> {
    reasoning_effort_from_value(request.reasoning.as_ref())
}

fn reasoning_effort_from_compact_request(request: &CodexCompactRequest) -> Option<&str> {
    reasoning_effort_from_value(request.reasoning.as_ref())
}

fn reasoning_effort_from_value(reasoning: Option<&Value>) -> Option<&str> {
    reasoning?
        .get("effort")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn ensure_stream_metadata_flag(metadata: &mut Value) {
    let Some(object) = metadata.as_object_mut() else {
        *metadata = serde_json::json!({ "stream": true });
        return;
    };
    object
        .entry("stream".to_string())
        .or_insert(Value::Bool(true));
}

fn enrich_live_response_stream_metadata(
    context: &LiveResponseStreamContext,
    rate_limit_headers: &[(String, String)],
    metadata: &mut Value,
    body: &str,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object
        .entry("transport".to_string())
        .or_insert_with(|| Value::String(backend_transport_name(context.transport).to_string()));
    if !rate_limit_headers.is_empty() {
        object
            .entry("rateLimitHeaders".to_string())
            .or_insert_with(|| serde_json::json!(rate_limit_headers));
    }
    if let Some(decision) = context.websocket_pool_decision {
        object
            .entry("websocketPool".to_string())
            .or_insert_with(|| decision.metadata_value());
    }
    object
        .entry("requestBody".to_string())
        .or_insert_with(|| serde_json::json!(context.request));
    object
        .entry("responseBody".to_string())
        .or_insert_with(|| Value::String(body.to_string()));
}

async fn record_live_response_stream_event(
    context: &LiveResponseStreamContext,
    status_code: i64,
    level: UsageRecordLevel,
    message: &str,
    mut metadata: Value,
    rate_limit_headers: &[(String, String)],
    body: &str,
) {
    ensure_stream_metadata_flag(&mut metadata);
    enrich_event_route_metadata(&mut metadata, &context.route);
    enrich_live_response_stream_metadata(context, rate_limit_headers, &mut metadata, body);
    log_live_response_stream_finalized(context, status_code, level, message, &metadata, body);
    let mut event = UsageRecord::new(response_event_kind(&context.route), level, message);
    event.request_id = Some(context.request_id.clone());
    event.account_id = Some(context.account_id.clone());
    event.route = Some(context.route.clone());
    event.model = Some(context.display_model.clone());
    event.status_code = Some(status_code);
    event.latency_ms = Some(elapsed_millis_i64(context.started_at));
    enrich_usage_record_identity(
        &mut metadata,
        Some(&context.requested_model),
        &context.display_model,
        context.client_ip.as_deref(),
        context.request.client_user_agent.as_deref(),
        reasoning_effort_from_request(&context.request),
        context.request.service_tier.as_deref(),
    );
    event.metadata = metadata;
    if let Err(error) = context.usage_records.record(event).await {
        tracing::warn!(account_id = %context.account_id, error = %error, "failed to record live response stream event");
    }
}

fn log_live_response_stream_finalized(
    context: &LiveResponseStreamContext,
    status_code: i64,
    level: UsageRecordLevel,
    message: &str,
    metadata: &Value,
    body: &str,
) {
    let response_id = metadata_string_field(metadata, "responseId")
        .map(ToString::to_string)
        .or_else(|| latest_response_id(body));
    let first_token_ms = metadata.get("firstTokenMs").and_then(Value::as_i64);
    let websocket_pool_kind = context
        .websocket_pool_decision
        .map(WebSocketPoolDecision::kind);
    let websocket_pool_reason = context
        .websocket_pool_decision
        .and_then(WebSocketPoolDecision::reason);
    let completed = metadata.get("completed").and_then(Value::as_bool);
    let failed = metadata.get("failed").and_then(Value::as_bool);
    let upstream_code = metadata_string_field(metadata, "upstreamCode");
    let failure_class = metadata_string_field(metadata, "failureClass");
    let failure_source = metadata_string_field(metadata, "failureSource");
    let failure_detail = metadata_string_field(metadata, "failureDetail");

    macro_rules! emit_stream_finalized_log {
        ($level:expr) => {
            tracing::event!(
                $level,
                account_id = %context.account_id,
                request_id = %context.request_id,
                route = %context.route,
                model = %context.display_model,
                status_code,
                usage_level = ?level,
                event_message = %message,
                transport = %backend_transport_name(context.transport),
                websocket_pool_kind = ?websocket_pool_kind,
                websocket_pool_reason = ?websocket_pool_reason,
                response_id = response_id.as_deref().unwrap_or(""),
                first_token_ms = ?first_token_ms,
                latency_ms = elapsed_millis_i64(context.started_at),
                completed = ?completed,
                failed = ?failed,
                upstream_code = ?upstream_code,
                failure_class = ?failure_class,
                failure_source = ?failure_source,
                failure_detail = ?failure_detail,
                "live response stream finalized"
            );
        };
    }

    match level {
        UsageRecordLevel::Debug => {
            emit_stream_finalized_log!(tracing::Level::DEBUG);
        }
        UsageRecordLevel::Info => {
            emit_stream_finalized_log!(tracing::Level::INFO);
        }
        UsageRecordLevel::Warn => {
            emit_stream_finalized_log!(tracing::Level::WARN);
        }
        UsageRecordLevel::Error => {
            emit_stream_finalized_log!(tracing::Level::ERROR);
        }
    }
}

fn metadata_string_field<'a>(metadata: &'a Value, field: &str) -> Option<&'a str> {
    metadata.get(field).and_then(Value::as_str)
}

async fn live_response_rate_limit_headers(
    context: &LiveResponseStreamContext,
) -> Vec<(String, String)> {
    let mut headers = context.rate_limit_headers.clone();
    if let Some(updates) = &context.rate_limit_header_updates {
        headers.extend(updates.lock().await.iter().cloned());
    }
    headers
}

async fn live_response_turn_state(context: &LiveResponseStreamContext) -> Option<String> {
    if let Some(update) = &context.turn_state_update {
        return update.lock().await.clone();
    }
    context.turn_state.clone()
}

fn stream_failure_metadata(failure: &ResponsesSseFailure, usage: Option<TokenUsage>) -> Value {
    let mut metadata = serde_json::json!({
        "stream": true,
        "failed": true,
        "failureEvent": failure.event,
        "failureMessage": failure.message,
        "upstreamCode": failure.upstream_code,
        "usage": usage,
    });
    enrich_stream_failure_source_metadata(&mut metadata, failure);
    metadata
}

fn enrich_stream_failure_source_metadata(metadata: &mut Value, failure: &ResponsesSseFailure) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object.insert(
        "failureSource".to_string(),
        Value::String(stream_failure_source(failure).to_string()),
    );
    if let Some(detail) = synthetic_stream_disconnected_detail(failure) {
        object.insert("synthetic".to_string(), Value::Bool(true));
        if !detail.is_empty() {
            object.insert("failureDetail".to_string(), Value::String(detail));
        }
    }
}

fn stream_failure_source(failure: &ResponsesSseFailure) -> &'static str {
    if synthetic_stream_disconnected_detail(failure).is_some() {
        "proxy"
    } else {
        "upstream"
    }
}

fn synthetic_stream_disconnected_detail(failure: &ResponsesSseFailure) -> Option<String> {
    if failure.upstream_code.as_deref() != Some(STREAM_DISCONNECTED_CODE) {
        return None;
    }
    let detail = failure
        .message
        .strip_prefix(STREAM_DISCONNECTED_MESSAGE)?
        .strip_prefix(": ")
        .unwrap_or_default()
        .trim()
        .to_string();
    Some(detail)
}

fn insert_first_token_ms(metadata: &mut Value, first_token_ms: Option<i64>) {
    let Some(first_token_ms) = first_token_ms else {
        return;
    };
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "firstTokenMs".to_string(),
            Value::Number(first_token_ms.into()),
        );
    }
}

fn insert_websocket_pool_decision(metadata: &mut Value, decision: Option<WebSocketPoolDecision>) {
    let Some(decision) = decision else {
        return;
    };
    if let Some(object) = metadata.as_object_mut() {
        object.insert("websocketPool".to_string(), decision.metadata_value());
    }
}

fn status_code_for_stream_failure(failure: &ResponsesSseFailure) -> i64 {
    let code = failure
        .upstream_code
        .as_deref()
        .unwrap_or("error")
        .to_ascii_lowercase();
    if code.contains("model") && (code.contains("not_supported") || code.contains("not_available"))
    {
        return 400;
    }
    if code.contains("invalid_request") || code.contains("not_found") {
        return 400;
    }
    if code.contains("context_window")
        || code.contains("invalid_prompt")
        || code.contains("cyber_policy")
        || code.contains("bad_request")
    {
        return 400;
    }
    if code.contains("rate_limit") || code.contains("usage_limit") {
        return 429;
    }
    if code.contains("unauthorized")
        || code.contains("invalid_api_key")
        || code == "token_invalid"
        || code == "token_expired"
        || code == "account_deactivated"
    {
        return 401;
    }
    if code.contains("forbidden") || code.contains("banned") {
        return 403;
    }
    if code.contains("payment") || code.contains("quota") {
        return 402;
    }
    if code.contains("server_overloaded") {
        return 503;
    }
    502
}

fn stream_failure_http_status(failure: &ResponsesSseFailure) -> u16 {
    u16::try_from(status_code_for_stream_failure(failure)).unwrap_or(502)
}

// ====================================================================
// Affinity + replay recording
// ====================================================================

async fn record_response_affinity(
    session_affinity: &Arc<RuntimeSessionAffinityService>,
    reasoning_replay: &Arc<Mutex<ReasoningReplayCache>>,
    request: &CodexResponsesRequest,
    account_id: &str,
    body: &str,
    turn_state: Option<String>,
    usage: Option<TokenUsage>,
) {
    let Some(conversation_id) = request
        .prompt_cache_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let metadata = match completed_response_metadata(body) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to parse completed response metadata for session affinity"
            );
            return;
        }
    };

    let variant_hash = compute_variant_hash(request);
    let entry = crate::proxy::dispatch::session_affinity::SessionAffinityEntry {
        account_id: account_id.to_string(),
        conversation_id: conversation_id.to_string(),
        turn_state: turn_state
            .filter(|value| !value.trim().is_empty())
            .or_else(|| request.turn_state.clone()),
        instructions_hash: Some(hash_instructions(Some(&request.instructions))),
        input_tokens: usage.map(|usage| usage.input_tokens),
        function_call_ids: metadata.function_call_ids,
        variant_hash: Some(variant_hash.clone()),
        created_at: Utc::now(),
    };
    if let Err(error) = session_affinity
        .record(metadata.response_id.clone(), entry)
        .await
    {
        tracing::warn!(
            error = %error,
            response_id = %metadata.response_id,
            account_id = %account_id,
            "failed to record session affinity"
        );
    }

    reasoning_replay.lock().await.record(
        metadata.response_id,
        account_id,
        conversation_id,
        &variant_hash,
        &metadata.replay_items,
        Utc::now(),
    );
}

async fn evict_reasoning_replay(
    reasoning_replay: &Arc<Mutex<ReasoningReplayCache>>,
    request: &CodexResponsesRequest,
    account_id: &str,
) {
    let variant_hash = compute_variant_hash(request);
    let conversation_id = request
        .prompt_cache_key
        .as_deref()
        .unwrap_or("")
        .to_string();
    reasoning_replay.lock().await.evict_by_identity(
        account_id,
        &conversation_id,
        &variant_hash,
        Utc::now(),
    );
}

// ====================================================================
// ResponseDispatchError and ResponseDispatchStreamError
// ====================================================================

/// Responses 调度错误。
#[derive(Debug, Error)]
pub enum ResponseDispatchError {
    #[error("failed to list runtime accounts")]
    AccountStore,
    #[error("no active account is available")]
    NoActiveAccount,
    #[error("all accounts exhausted by quota")]
    QuotaExhausted {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by rate limit")]
    RateLimited {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by expired auth")]
    Expired {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by disabled auth")]
    Disabled {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by banned auth")]
    Banned {
        count: usize,
        upstream_error: String,
        status_code: u16,
    },
    #[error("all accounts exhausted by Cloudflare challenge")]
    CloudflareChallenge {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by Cloudflare path-block")]
    CloudflarePathBlocked {
        count: usize,
        upstream_error: String,
    },
    #[error("all accounts exhausted by unsupported model")]
    ModelUnsupported {
        count: usize,
        upstream_error: String,
    },
    #[error("upstream request failed: {0}")]
    Upstream(#[from] CodexClientError),
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
    #[error("upstream response did not include response.completed")]
    MissingCompleted,
    #[error("upstream response did not include visible output")]
    EmptyUpstreamResponse,
    #[error("upstream response failed: {0:?}")]
    Failed(ResponsesSseFailure),
}

impl ResponseDispatchError {
    pub fn http_status_code(&self) -> u16 {
        match self {
            Self::NoActiveAccount | Self::AccountStore => 503,
            Self::QuotaExhausted { .. } => 402,
            Self::RateLimited { .. } => 429,
            Self::Expired { .. } | Self::Disabled { .. } => 401,
            Self::Banned { status_code, .. } => *status_code,
            Self::CloudflareChallenge { .. }
            | Self::CloudflarePathBlocked { .. }
            | Self::InvalidSse(_)
            | Self::MissingCompleted
            | Self::EmptyUpstreamResponse => 502,
            Self::Failed(failure) => stream_failure_http_status(failure),
            Self::ModelUnsupported { .. } => 400,
            Self::Upstream(error) => upstream_error_http_status(error),
        }
    }
}

/// Responses live SSE body stream error.
#[derive(Debug, Error)]
pub enum ResponseDispatchStreamError {
    #[error("upstream stream failed: {0}")]
    Upstream(#[from] CodexClientError),
}

fn dispatch_error_metadata(
    error: impl std::fmt::Display,
    stream: bool,
    compact: bool,
    transport: Option<&str>,
) -> Value {
    let mut metadata = serde_json::json!({
        "stream": stream,
        "failed": true,
        "errorKind": "dispatch",
        "error": error.to_string(),
    });
    let Some(object) = metadata.as_object_mut() else {
        return metadata;
    };
    if compact {
        object.insert("compact".to_string(), Value::Bool(true));
    }
    if let Some(transport) = transport {
        object.insert(
            "transport".to_string(),
            Value::String(transport.to_string()),
        );
    }
    metadata
}

fn enrich_response_dispatch_error_metadata(metadata: &mut Value, error: &ResponseDispatchError) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    let (failure_class, exhausted_count, upstream_error, upstream_status) = match error {
        ResponseDispatchError::AccountStore => ("account_store", None, None, None),
        ResponseDispatchError::NoActiveAccount => ("no_available_accounts", None, None, None),
        ResponseDispatchError::QuotaExhausted {
            count,
            upstream_error,
        } => (
            "quota_exhausted",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::RateLimited {
            count,
            upstream_error,
        } => (
            "rate_limited",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::Expired {
            count,
            upstream_error,
        } => ("expired", Some(*count), Some(upstream_error.clone()), None),
        ResponseDispatchError::Disabled {
            count,
            upstream_error,
        } => ("disabled", Some(*count), Some(upstream_error.clone()), None),
        ResponseDispatchError::Banned {
            count,
            upstream_error,
            ..
        } => ("banned", Some(*count), Some(upstream_error.clone()), None),
        ResponseDispatchError::CloudflareChallenge {
            count,
            upstream_error,
        } => (
            "cloudflare_challenge",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::CloudflarePathBlocked {
            count,
            upstream_error,
        } => (
            "cloudflare_path_blocked",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::ModelUnsupported {
            count,
            upstream_error,
        } => (
            "model_unsupported",
            Some(*count),
            Some(upstream_error.clone()),
            None,
        ),
        ResponseDispatchError::Upstream(error) => {
            let upstream_status = match error {
                CodexClientError::Upstream { status, .. } => Some(status.as_u16()),
                _ => None,
            };
            (
                "upstream",
                None,
                Some(upstream_error_body(error)),
                upstream_status,
            )
        }
        ResponseDispatchError::InvalidSse(_) => ("invalid_sse", None, None, None),
        ResponseDispatchError::MissingCompleted => ("missing_completed", None, None, None),
        ResponseDispatchError::EmptyUpstreamResponse => {
            ("empty_upstream_response", None, None, None)
        }
        ResponseDispatchError::Failed(failure) => (
            "response_failed",
            None,
            Some(sse_failure_error_body(failure)),
            None,
        ),
    };

    object.insert(
        "failureClass".to_string(),
        Value::String(failure_class.to_string()),
    );
    if let Some(count) = exhausted_count {
        object.insert("exhaustedCount".to_string(), json!(count));
    }
    if let Some(error) = upstream_error {
        object.insert("upstreamError".to_string(), Value::String(error));
    }
    if let Some(status) = upstream_status {
        object.insert("upstreamStatus".to_string(), json!(status));
    }
}

fn elapsed_millis_i64(started_at: Instant) -> i64 {
    started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
}
