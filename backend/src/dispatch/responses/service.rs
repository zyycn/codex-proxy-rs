//! Responses 创建编排与调度服务。
//!
//! 包含了将 OpenAI 请求调度到 Codex 上游账号的完整逻辑，包括：
//! - 响应创建（非流式 / 流式 / compact）
//! - 会话亲和性与隐式续接
//! - reasoning replay
//! - 账号回退与错误恢复
//! - 配额验证

use std::{pin::Pin, sync::Arc, time::Instant};

use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use futures::stream::Stream;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::{
    accounts::{
        account::{Account, AccountStatus},
        pool::{AccountAcquireRequest, RuntimeAccountPoolService},
    },
    dispatch::{
        affinity::resolve::{evict_reasoning_replay, record_response_affinity},
        affinity::{
            compute_variant_hash, ensure_prompt_cache_key, hash_instructions,
            prepare_variant_identity, RuntimeSessionAffinityService,
        },
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
        upstream::{
            create_compact_response_with_account_retrying_5xx,
            create_response_stream_with_account_retrying_5xx,
            create_response_with_account_retrying_5xx, verify_acquired_quota_if_required,
            QuotaVerificationContext, QuotaVerificationDecision,
            QUOTA_VERIFY_LIMIT_REACHED_MESSAGE,
        },
    },
    models::service::ModelService,
    telemetry::{
        ops::query::OpsQueryService,
        recorder::{
            reasoning_effort_from_compact_request, reasoning_effort_from_request,
            record_response_event,
        },
        usage::query::UsageQueryService,
        usage::types::ResponseUsageRecord,
    },
    upstream::openai::{
        protocol::{
            events::{extract_usage, TokenUsage},
            responses::{
                response_from_codex_sse, CodexCompactRequest, CodexResponsesRequest,
                CollectedResponse,
            },
        },
        transport::{
            backend_transport_for_response_request, is_banned_upstream_error, CodexBackendClient,
            CodexBackendResponse,
        },
    },
};

use crate::dispatch::implicit_resume::{
    continuation_input_start, implicit_resume_allowed, ImplicitResumeSnapshot,
};

use super::{
    errors::{ResponseDispatchError, ResponseDispatchStreamError},
    event_recording::{
        insert_response_status_metadata, insert_response_trace_metadata,
        insert_response_upstream_diagnostics, insert_websocket_pool_decision,
        record_prefetched_response_stream_failure_event, record_response_dispatch_error_event,
        record_response_upstream_error_event, ResponseDispatchErrorDetails,
        ResponseDispatchErrorEventRecord, ResponseStreamFailureEventRecord,
        ResponseUpstreamErrorEventRecord,
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
    trace::{ResponseDispatchAttempt, ResponseDispatchTrace},
};

/// OpenAI Responses 调度服务。
#[derive(Clone)]
pub struct ResponseDispatchService {
    account_pool: Arc<RuntimeAccountPoolService>,
    models: Arc<ModelService>,
    codex: Arc<CodexBackendClient>,
    session_affinity: Arc<RuntimeSessionAffinityService>,
    reasoning_replay: Arc<Mutex<ReasoningReplayCache>>,
    usage_records: Arc<UsageQueryService>,
    ops_errors: Arc<OpsQueryService>,
    installation_id: Option<String>,
    cloudflare: CloudflareRecovery,
}

pub(crate) struct ResponseDispatchServiceParts {
    pub account_pool: Arc<RuntimeAccountPoolService>,
    pub models: Arc<ModelService>,
    pub codex: Arc<CodexBackendClient>,
    pub session_affinity: Arc<RuntimeSessionAffinityService>,
    pub usage_records: Arc<UsageQueryService>,
    pub ops_errors: Arc<OpsQueryService>,
    pub installation_id: Option<String>,
    pub cloudflare: CloudflareRecovery,
}

/// 默认 reasoning replay TTL 秒数。
const DEFAULT_REASONING_REPLAY_TTL_SECS: i64 = 55 * 60;
const IMPLICIT_RESUME_MAX_AGE_SECS: i64 = DEFAULT_REASONING_REPLAY_TTL_SECS;

#[derive(Debug, Clone, Default)]
struct AccountAffinityDecision {
    preferred_account_id: Option<String>,
    request_history_owner: RequestHistoryOwner,
}

#[derive(Debug, Clone, Default)]
enum RequestHistoryOwner {
    #[default]
    Absent,
    Known(String),
    Unknown,
}

impl AccountAffinityDecision {
    fn preferred_account_id(&self) -> Option<&str> {
        self.preferred_account_id.as_deref()
    }

    fn should_strip_request_history_for(&self, account_id: &str) -> bool {
        match &self.request_history_owner {
            RequestHistoryOwner::Absent => false,
            RequestHistoryOwner::Known(history_account_id) => history_account_id != account_id,
            RequestHistoryOwner::Unknown => true,
        }
    }
}

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
            ops_errors: parts.ops_errors,
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

    async fn account_affinity_for_request(
        &self,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> AccountAffinityDecision {
        let has_previous_response_id = request.previous_response_id().is_some();
        if let Some(account_id) = self.previous_response_account_id(request, now).await {
            return AccountAffinityDecision {
                preferred_account_id: Some(account_id.clone()),
                request_history_owner: RequestHistoryOwner::Known(account_id),
            };
        }

        let conversation_account_id = self.conversation_account_id(request, now).await;
        AccountAffinityDecision {
            preferred_account_id: conversation_account_id,
            request_history_owner: if has_previous_response_id {
                RequestHistoryOwner::Unknown
            } else {
                RequestHistoryOwner::Absent
            },
        }
    }

    async fn previous_response_account_id(
        &self,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let previous_response_id = request.previous_response_id()?;
        self.session_affinity
            .lookup_account(previous_response_id, now)
            .await
    }

    async fn conversation_account_id(
        &self,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let conversation_id = request
            .prompt_cache_key()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let variant_hash = compute_variant_hash(request);
        self.session_affinity
            .lookup_latest_account_by_conversation(conversation_id, None, Some(&variant_hash), now)
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
        restore_request_without_history(request, implicit_resume);
    }

    fn strip_history_if_account_changed(
        request: &mut CodexResponsesRequest,
        implicit_resume: &mut Option<ImplicitResumeSnapshot>,
        account_affinity: &AccountAffinityDecision,
        acquired_account_id: &str,
    ) {
        if request.previous_response_id().is_none()
            || !account_affinity.should_strip_request_history_for(acquired_account_id)
        {
            return;
        }

        restore_request_without_history(request, implicit_resume);
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
        restore_request_without_history(request, implicit_resume);
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
}

mod compact;
mod complete;
mod stream;

fn strip_request_history(request: &mut CodexResponsesRequest) {
    request.set_previous_response_id(None);
    request.turn_state = None;
}

fn restore_request_without_history(
    request: &mut CodexResponsesRequest,
    implicit_resume: &mut Option<ImplicitResumeSnapshot>,
) {
    if let Some(snapshot) = implicit_resume.take() {
        snapshot.restore(request);
    }
    strip_request_history(request);
}
