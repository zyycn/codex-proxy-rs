pub mod affinity;
pub mod fallback;
mod implicit_resume;
mod limits;
// 上游调度辅助命名为 routing，避免出现 dispatch::dispatch 的模块套娃。
mod reasoning_replay;
pub mod routing;
pub mod stream;
mod stream_audit;
pub mod usage;

use std::{
    future::Future,
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};

use axum::{
    body::Body,
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        StatusCode,
    },
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use futures::{stream as futures_stream, StreamExt};
use rand::RngExt;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::{
    codex::accounts::cookies::repository::CookieRepository,
    codex::accounts::{
        cloudflare_challenge::CfPathBlockTracker,
        model::Account,
        pool::{AccountAcquireRequest, AccountPool, AcquiredAccount},
        repository::AccountRepository,
    },
    codex::events::{
        event::{EventLevel, EventLog},
        service::LogService,
    },
    codex::gateway::conversation_identity::{build_conversation_identity, ensure_prompt_cache_key},
    codex::gateway::fingerprint::model::Fingerprint,
    codex::gateway::installation_id::get_installation_id,
    codex::gateway::protocol::codex_to_openai::openai_error,
    codex::gateway::transport::{
        http_client::{
            build_reqwest_client, CodexBackendClient, CodexBackendStream,
            CodexBackendWebSocketStream, CodexClientError, CodexCompactResponse,
            CodexRequestContext,
        },
        types::{CodexCompactRequest, CodexResponsesRequest},
        usage_events::TokenUsage,
        websocket::{
            transport_for_request, CodexTransport, CodexWebSocketError, CodexWebSocketPool,
        },
    },
    config::AppConfig,
};

use crate::codex::serving::http::errors::{
    codex_client_error_message, codex_client_error_response,
    codex_client_error_response_with_status_and_message,
};

pub(crate) use self::{
    fallback::{
        classify_upstream_account_retry, classify_upstream_request_recovery,
        websocket_history_retry_metadata, UpstreamAccountRetry, UpstreamRequestRecovery,
    },
    routing::{no_available_accounts_response, normalize_service_tier_for_upstream},
    stream::{completed_response_json, CollectedResponse},
};

use self::affinity::{
    compute_variant_hash, hash_instructions, prepare_variant_identity, SessionAffinityMap,
    SessionAffinityRepository,
};
use self::fallback::{
    apply_upstream_account_retry_with_deps, apply_upstream_retry_and_acquire_fallback_with_deps,
    build_account_exhaustion_detail,
};
pub(crate) use self::implicit_resume::ImplicitResumeSnapshot;
use self::implicit_resume::{continuation_input_start, implicit_resume_allowed};
use self::reasoning_replay::ReasoningReplayCache;
use self::stream_audit::{StreamAudit, WebSocketStreamAudit};
use self::{
    limits::apply_rate_limit_headers_with_deps,
    routing::request_domain,
    stream::{
        completed_response_metadata, ensure_stream_metadata, has_terminal_sse_event,
        premature_close_failed_event, TupleStreamReconverter,
    },
    usage::{record_empty_response_with_deps, record_request_attempt, record_usage_with_deps},
};

#[derive(Clone)]
struct CodexUpstreamDependencies {
    config: Arc<AppConfig>,
    account_pool: Arc<Mutex<AccountPool>>,
    account_repository: Option<AccountRepository>,
    cookie_repository: Option<CookieRepository>,
    logs: LogService,
    fingerprint: Fingerprint, // 用于实际请求的指纹
    session_affinity: Arc<SessionAffinityMap>,
    session_affinity_repository: Option<SessionAffinityRepository>,
    reasoning_replay: Arc<ReasoningReplayCache>,
    websocket_pool: Arc<CodexWebSocketPool>,
    cf_path_block_tracker: CfPathBlockTracker,
}

const MAX_UPSTREAM_5XX_RETRIES: u8 = 2;
const UPSTREAM_5XX_RETRY_BASE_DELAY_MS: u64 = 1_000;

#[derive(Clone)]
pub(crate) struct CodexUpstreamService {
    deps: CodexUpstreamDependencies,
}

pub(crate) struct CodexUpstreamRepositories {
    pub(crate) account: Option<AccountRepository>,
    pub(crate) cookie: Option<CookieRepository>,
    pub(crate) session_affinity: Option<SessionAffinityRepository>,
}

impl CodexUpstreamService {
    pub(crate) fn new(
        config: Arc<AppConfig>,
        account_pool: Arc<Mutex<AccountPool>>,
        repositories: CodexUpstreamRepositories,
        logs: LogService,
        fingerprint: Fingerprint,
        websocket_pool: Arc<CodexWebSocketPool>,
    ) -> Self {
        Self {
            deps: CodexUpstreamDependencies {
                config,
                account_pool,
                account_repository: repositories.account,
                cookie_repository: repositories.cookie,
                logs,
                fingerprint,
                session_affinity: Arc::new(SessionAffinityMap::with_default_ttl()),
                session_affinity_repository: repositories.session_affinity,
                reasoning_replay: Arc::new(ReasoningReplayCache::default()),
                websocket_pool,
                cf_path_block_tracker: CfPathBlockTracker::new(),
            },
        }
    }

    pub(crate) async fn acquire_account(&self, model: &str) -> Option<AcquiredAccount> {
        self.deps
            .account_pool
            .lock()
            .await
            .acquire_with(AccountAcquireRequest::new(model, Utc::now()))
    }

    pub(crate) async fn prepare_response_session(
        &self,
        request: &mut CodexResponsesRequest,
    ) -> Option<ImplicitResumeSnapshot> {
        prepare_variant_identity(request);
        if let Some(previous_response_id) = request.previous_response_id.as_deref() {
            if let Some(conversation_id) = self
                .deps
                .session_affinity
                .lookup_conversation_id(previous_response_id)
                .await
            {
                request.prompt_cache_key = Some(conversation_id);
            }
            if request
                .turn_state
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                request.turn_state = self
                    .deps
                    .session_affinity
                    .lookup_turn_state(previous_response_id)
                    .await;
            }
            ensure_prompt_cache_key(request);
            return None;
        }
        ensure_prompt_cache_key(request);
        apply_implicit_resume_with_deps(&self.deps, request).await
    }

    pub(crate) async fn acquire_account_for_request(
        &self,
        request: &CodexResponsesRequest,
    ) -> Option<AcquiredAccount> {
        let preferred_account_id = match request.previous_response_id.as_deref() {
            Some(previous_response_id) => {
                self.deps
                    .session_affinity
                    .lookup_account(previous_response_id)
                    .await
            }
            None => None,
        };
        let mut acquire_request = AccountAcquireRequest::new(&request.model, Utc::now());
        if let Some(preferred_account_id) = preferred_account_id {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        self.deps
            .account_pool
            .lock()
            .await
            .acquire_with(acquire_request)
    }

    pub(crate) async fn release_account(&self, account_id: &str) {
        self.deps.account_pool.lock().await.release(account_id);
    }

    pub(crate) async fn forget_response_affinity(&self, response_id: &str) {
        self.deps.session_affinity.forget(response_id).await;
    }

    pub(crate) async fn stagger_request(&self, previous_slot_at: Option<DateTime<Utc>>) {
        stagger_request_with_deps(&self.deps, previous_slot_at).await;
    }

    /// 获取当前使用的指纹（用于诊断）
    pub(crate) fn fingerprint(&self) -> &Fingerprint {
        &self.deps.fingerprint
    }

    pub(crate) async fn send_codex_request_with_upstream_retries(
        &self,
        request: &CodexResponsesRequest,
        account: &Account,
        request_id: &str,
    ) -> Result<crate::codex::gateway::transport::http_client::CodexBackendResponse, CodexClientError>
    {
        send_codex_request_with_upstream_retries_deps(&self.deps, request, account, request_id)
            .await
    }

    pub(crate) async fn send_compact_request_with_upstream_retries(
        &self,
        request: &CodexCompactRequest,
        account: &Account,
        request_id: &str,
    ) -> Result<CodexCompactResponse, CodexClientError> {
        send_compact_request_with_upstream_retries_deps(&self.deps, request, account, request_id)
            .await
    }

    pub(crate) async fn apply_retry_and_acquire_fallback(
        &self,
        account: &Account,
        retry: UpstreamAccountRetry,
        model: &str,
        excluded_account_ids: &mut Vec<String>,
    ) -> Option<AcquiredAccount> {
        apply_upstream_retry_and_acquire_fallback_with_deps(
            &self.deps,
            account,
            retry,
            model,
            excluded_account_ids,
        )
        .await
    }

    pub(crate) async fn fallback_exhausted_message(&self, message: &str) -> String {
        fallback_exhausted_message_with_deps(&self.deps, message).await
    }

    pub(crate) async fn apply_account_retry(&self, account: &Account, retry: UpstreamAccountRetry) {
        apply_upstream_account_retry_with_deps(&self.deps, account, retry).await;
    }

    pub(crate) async fn responses_stream(
        &self,
        request: CodexResponsesRequest,
        acquired: AcquiredAccount,
        log_context: CodexRequestLogContext,
        implicit_resume: Option<ImplicitResumeSnapshot>,
    ) -> Response {
        let deps = self.deps.clone();
        if matches!(
            transport_for_request(&request),
            CodexTransport::WebSocketPreferred | CodexTransport::WebSocketRequired
        ) {
            return responses_websocket_stream(
                deps,
                request,
                acquired,
                log_context,
                implicit_resume,
            )
            .await;
        }

        responses_http_sse_stream(deps, request, acquired, log_context, implicit_resume).await
    }

    pub(crate) async fn persist_cookies(
        &self,
        account_id: &str,
        set_cookie_headers: &[String],
    ) -> Result<(), ()> {
        persist_upstream_cookies_with_deps(&self.deps, account_id, set_cookie_headers).await
    }

    pub(crate) async fn record_usage(&self, account_id: &str, usage: TokenUsage) -> Result<(), ()> {
        record_usage_with_deps(&self.deps, account_id, usage).await
    }

    pub(crate) async fn record_empty_response(&self, account_id: &str) -> Result<(), ()> {
        record_empty_response_with_deps(&self.deps, account_id).await
    }

    pub(crate) async fn record_request_attempt(&self, account_id: &str) -> Result<(), ()> {
        record_request_attempt(&self.deps, account_id).await
    }

    pub(crate) async fn record_response_affinity(
        &self,
        request: &CodexResponsesRequest,
        account_id: &str,
        body: &str,
        turn_state: Option<&str>,
        usage: Option<TokenUsage>,
    ) {
        record_response_affinity_with_deps(
            &self.deps, request, account_id, body, turn_state, usage,
        )
        .await;
    }

    pub(crate) async fn evict_reasoning_replay(
        &self,
        request: &CodexResponsesRequest,
        account_id: &str,
    ) {
        evict_reasoning_replay_with_deps(&self.deps, request, account_id).await;
    }

    pub(crate) async fn log_response(
        &self,
        context: &CodexRequestLogContext,
        status: StatusCode,
        level: EventLevel,
        message: &str,
        metadata: Value,
    ) {
        log_codex_upstream_response_with_deps(
            &self.deps, context, status, level, message, metadata,
        )
        .await;
    }

    pub(crate) async fn reload_session_affinity_from_repository(
        &self,
    ) -> self::affinity::SessionAffinityRepositoryResult<usize> {
        let Some(repository) = self.deps.session_affinity_repository.as_ref() else {
            return Ok(0);
        };
        let now = Utc::now();
        repository.delete_expired(now).await?;
        let records = repository.list_active(now).await?;
        Ok(self.deps.session_affinity.restore(records).await)
    }
}

const IMPLICIT_RESUME_MAX_AGE: StdDuration = StdDuration::from_secs(55 * 60);

async fn fallback_exhausted_message_with_deps(
    deps: &CodexUpstreamDependencies,
    message: &str,
) -> String {
    let summary = deps.account_pool.lock().await.status_summary(Utc::now());
    build_account_exhaustion_detail(summary, message)
}

async fn record_failed_request_attempt_with_deps(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
) {
    if let Err(error) = record_request_attempt(deps, account_id).await {
        tracing::warn!(
            error = ?error,
            account_id = %account_id,
            "记录上游最终失败请求尝试失败"
        );
    }
}

async fn apply_implicit_resume_with_deps(
    deps: &CodexUpstreamDependencies,
    request: &mut CodexResponsesRequest,
) -> Option<ImplicitResumeSnapshot> {
    if request.previous_response_id.is_some() {
        return None;
    }
    let continuation_start = continuation_input_start(&request.input);
    if continuation_start == 0 || continuation_start >= request.input.len() {
        return None;
    }
    let conversation_id = request
        .prompt_cache_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;

    let snapshot = ImplicitResumeSnapshot::capture(request);
    let variant_hash = compute_variant_hash(request);
    let previous_response_id = deps
        .session_affinity
        .lookup_latest_response_by_conversation(
            conversation_id,
            Some(IMPLICIT_RESUME_MAX_AGE),
            Some(&variant_hash),
        )
        .await?;
    let current_instructions_hash = hash_instructions(Some(&request.instructions));
    if deps
        .session_affinity
        .lookup_instructions_hash(&previous_response_id)
        .await
        .as_deref()
        != Some(current_instructions_hash.as_str())
    {
        return None;
    }

    let stored_function_call_ids = deps
        .session_affinity
        .lookup_function_call_ids(&previous_response_id)
        .await;
    if !implicit_resume_allowed(
        &request.input[continuation_start..],
        &request.input,
        &stored_function_call_ids,
    ) {
        return None;
    }

    let account_id = deps
        .session_affinity
        .lookup_account(&previous_response_id)
        .await?;
    let replay_items = deps
        .reasoning_replay
        .lookup(
            &previous_response_id,
            &account_id,
            conversation_id,
            &variant_hash,
        )
        .await;
    let continuation = request.input[continuation_start..].to_vec();
    let mut input = replay_items;
    input.extend(continuation);

    request.previous_response_id = Some(previous_response_id.clone());
    request.use_websocket = true;
    request.force_http_sse = false;
    request.input = input;
    if let Some(turn_state) = deps
        .session_affinity
        .lookup_turn_state(&previous_response_id)
        .await
    {
        request.turn_state = Some(turn_state);
    }
    Some(snapshot)
}

async fn stagger_request_with_deps(
    deps: &CodexUpstreamDependencies,
    previous_slot_at: Option<DateTime<Utc>>,
) {
    let Some(previous_slot_at) = previous_slot_at else {
        return;
    };
    let interval_ms = deps.config.auth.request_interval_ms;
    if interval_ms == 0 {
        return;
    }
    let target_interval_ms = jitter_request_interval_ms(interval_ms);
    let elapsed_ms = Utc::now()
        .signed_duration_since(previous_slot_at)
        .num_milliseconds()
        .max(0) as u64;
    let Some(wait_ms) = target_interval_ms.checked_sub(elapsed_ms) else {
        return;
    };
    if wait_ms == 0 {
        return;
    }
    tracing::debug!(
        wait_ms,
        request_interval_ms = interval_ms,
        target_interval_ms,
        "按账户请求间隔等待后发送上游请求"
    );
    tokio::time::sleep(StdDuration::from_millis(wait_ms)).await;
}

fn jitter_request_interval_ms(interval_ms: u64) -> u64 {
    let mut rng = rand::rng();
    jitter_request_interval_ms_with_factor(interval_ms, rng.random_range(0.7..=1.3))
}

fn jitter_request_interval_ms_with_factor(interval_ms: u64, factor: f64) -> u64 {
    let factor = factor.clamp(0.7, 1.3);
    ((interval_ms as f64) * factor).round().min(u64::MAX as f64) as u64
}

async fn apply_upstream_request_recovery_with_deps(
    deps: &CodexUpstreamDependencies,
    request: &mut CodexResponsesRequest,
    recovery: UpstreamRequestRecovery,
    stream: bool,
    log_context: &CodexRequestLogContext,
    history_recovery_used: &mut bool,
    implicit_resume: &mut Option<ImplicitResumeSnapshot>,
) {
    *history_recovery_used = true;
    let stale_response_id = request.previous_response_id.clone();
    if let Some(response_id) = stale_response_id.as_deref() {
        deps.session_affinity.forget(response_id).await;
    }
    if let Some(snapshot) = implicit_resume.take() {
        snapshot.restore(request);
    }
    request.previous_response_id = None;
    request.turn_state = None;
    log_codex_upstream_response_with_deps(
        deps,
        log_context,
        StatusCode::BAD_REQUEST,
        EventLevel::Warn,
        "v1 responses 上游历史失效，去除 previous_response_id 后重试",
        recovery.metadata(stream, stale_response_id.as_deref()),
    )
    .await;
}

async fn responses_http_sse_stream(
    deps: CodexUpstreamDependencies,
    mut request: CodexResponsesRequest,
    mut acquired: AcquiredAccount,
    mut log_context: CodexRequestLogContext,
    mut implicit_resume: Option<ImplicitResumeSnapshot>,
) -> Response {
    let mut excluded_account_ids = Vec::new();
    let mut history_recovery_used = false;
    let mut model_unsupported_retry_used = false;
    let stream_response = loop {
        stagger_request_with_deps(&deps, acquired.previous_slot_at).await;
        let stream_response = send_codex_stream_request_with_upstream_retries(
            &deps,
            &request,
            &acquired.account,
            log_context.request_id.as_str(),
        )
        .await;
        match stream_response {
            Ok(response) => break response,
            Err(error) => {
                deps.account_pool.lock().await.release(&acquired.account.id);
                if let Some(recovery) =
                    classify_upstream_request_recovery(&error, history_recovery_used)
                {
                    apply_upstream_request_recovery_with_deps(
                        &deps,
                        &mut request,
                        recovery,
                        true,
                        &log_context,
                        &mut history_recovery_used,
                        &mut implicit_resume,
                    )
                    .await;
                    continue;
                }
                if let Some(retry) =
                    classify_upstream_account_retry(&error, model_unsupported_retry_used)
                {
                    if retry.is_model_unsupported() {
                        model_unsupported_retry_used = true;
                    }
                    let fallback = apply_upstream_retry_and_acquire_fallback_with_deps(
                        &deps,
                        &acquired.account,
                        retry,
                        &request.model,
                        &mut excluded_account_ids,
                    )
                    .await;
                    log_codex_upstream_response_with_deps(
                        &deps,
                        &log_context,
                        retry.status(),
                        EventLevel::Warn,
                        "v1 responses stream 上游请求将使用备用账户重试",
                        retry.metadata(true),
                    )
                    .await;
                    if let Some(fallback) = fallback {
                        log_context = log_context.with_account(&fallback.account.id);
                        acquired = fallback;
                        continue;
                    }
                    let retry_message =
                        retry.fallback_response_message(codex_client_error_message(&error));
                    let message = fallback_exhausted_message_with_deps(&deps, &retry_message).await;
                    let error_response = codex_client_error_response_with_status_and_message(
                        error,
                        retry.status(),
                        &message,
                    );
                    log_codex_upstream_response_with_deps(
                        &deps,
                        &log_context,
                        error_response.0,
                        EventLevel::Error,
                        "v1 responses stream fallback 已耗尽",
                        json!({"stream": true}),
                    )
                    .await;
                    return error_response.into_response();
                }
                record_failed_request_attempt_with_deps(&deps, &acquired.account.id).await;
                let error_response = codex_client_error_response(error);
                log_codex_upstream_response_with_deps(
                    &deps,
                    &log_context,
                    error_response.0,
                    EventLevel::Error,
                    "v1 responses stream 上游请求失败",
                    json!({"stream": true}),
                )
                .await;
                return error_response.into_response();
            }
        }
    };

    if persist_upstream_cookies_with_deps(
        &deps,
        &acquired.account.id,
        &stream_response.set_cookie_headers,
    )
    .await
    .is_err()
    {
        deps.account_pool.lock().await.release(&acquired.account.id);
        log_codex_upstream_response_with_deps(
            &deps,
            &log_context,
            StatusCode::INTERNAL_SERVER_ERROR,
            EventLevel::Error,
            "v1 responses stream 持久化 cookie 失败",
            json!({"stream": true, "cookieStoreError": true}),
        )
        .await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(openai_error(
                "Failed to persist upstream cookies",
                "cookie_store_error",
            )),
        )
            .into_response();
    }

    let rate_limit_headers = stream_response.rate_limit_headers.clone();
    let upstream = Box::pin(stream_response.response.bytes_stream());
    let tuple_schema = request.tuple_schema.clone();
    let audit = StreamAudit::new(
        deps,
        log_context,
        acquired.account.id,
        acquired.account.plan_type,
        request,
        rate_limit_headers,
    );

    use tokio::time::{interval_at, Duration, Instant as TokioInstant};
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
    const HEARTBEAT_CHUNK: &[u8] = b": ping\n\n";
    let heartbeat_timer = interval_at(TokioInstant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
    let tuple_reconverter = TupleStreamReconverter::new(tuple_schema);

    let body_stream = futures_stream::unfold(
        Some((
            upstream,
            Vec::new(),
            audit,
            heartbeat_timer,
            tuple_reconverter,
        )),
        |state| async move {
            let (
                mut upstream,
                mut collected,
                mut audit,
                mut heartbeat_timer,
                mut tuple_reconverter,
            ) = state?;

            tokio::select! {
                chunk_result = upstream.next() => {
                    match chunk_result {
                        Some(Ok(chunk)) => {
                            collected.extend_from_slice(&chunk);
                            let chunk = match std::str::from_utf8(&chunk) {
                                Ok(text) => {
                                    axum::body::Bytes::from(tuple_reconverter.transform_chunk(text))
                                }
                                Err(_) => chunk,
                            };
                            Some((
                                Ok::<axum::body::Bytes, reqwest::Error>(chunk),
                                Some((
                                    upstream,
                                    collected,
                                    audit,
                                    heartbeat_timer,
                                    tuple_reconverter,
                                )),
                            ))
                        }
                        Some(Err(error)) => {
                            if collected_has_terminal_sse_event(&collected) {
                                let tail = tuple_reconverter.finish();
                                audit.complete(&collected).await;
                                if tail.is_empty() {
                                    None
                                } else {
                                    Some((
                                        Ok::<axum::body::Bytes, reqwest::Error>(tail.into()),
                                        None,
                                    ))
                                }
                            } else {
                                let detail = error.to_string();
                                let mut output = tuple_reconverter.finish();
                                let failure = append_premature_close_failed_event(
                                    &mut collected,
                                    Some(detail.as_str()),
                                );
                                output.push_str(&failure);
                                audit.complete(&collected).await;
                                Some((
                                    Ok::<axum::body::Bytes, reqwest::Error>(output.into()),
                                    None,
                                ))
                            }
                        }
                        None => {
                            if collected_has_terminal_sse_event(&collected) {
                                let tail = tuple_reconverter.finish();
                                audit.complete(&collected).await;
                                if tail.is_empty() {
                                    None
                                } else {
                                    Some((
                                        Ok::<axum::body::Bytes, reqwest::Error>(tail.into()),
                                        None,
                                    ))
                                }
                            } else {
                                let mut output = tuple_reconverter.finish();
                                let failure =
                                    append_premature_close_failed_event(&mut collected, None);
                                output.push_str(&failure);
                                audit.complete(&collected).await;
                                Some((
                                    Ok::<axum::body::Bytes, reqwest::Error>(output.into()),
                                    None,
                                ))
                            }
                        }
                    }
                }
                _ = heartbeat_timer.tick() => {
                    Some((
                        Ok::<axum::body::Bytes, reqwest::Error>(HEARTBEAT_CHUNK.into()),
                        Some((
                            upstream,
                            collected,
                            audit,
                            heartbeat_timer,
                            tuple_reconverter,
                        )),
                    ))
                }
            }
        },
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(openai_error(
                    "Failed to build stream response",
                    "stream_response_error",
                )),
            )
                .into_response()
        })
}

fn websocket_stream_error_allows_http_sse_fallback(error: &CodexClientError) -> bool {
    match error {
        CodexClientError::WebSocket(
            CodexWebSocketError::Transport(_)
            | CodexWebSocketError::OpenTimeout { .. }
            | CodexWebSocketError::EmptyResponse,
        ) => true,
        CodexClientError::Upstream { status, .. } => matches!(
            *status,
            StatusCode::NOT_FOUND
                | StatusCode::METHOD_NOT_ALLOWED
                | StatusCode::UPGRADE_REQUIRED
                | StatusCode::NOT_IMPLEMENTED
        ),
        _ => false,
    }
}

fn collected_has_terminal_sse_event(collected: &[u8]) -> bool {
    let body = String::from_utf8_lossy(collected);
    has_terminal_sse_event(&body).unwrap_or(false)
}

fn append_premature_close_failed_event(collected: &mut Vec<u8>, detail: Option<&str>) -> String {
    let failure = premature_close_failed_event(detail);
    let prefix = sse_event_boundary_prefix(collected);
    collected.extend_from_slice(prefix.as_bytes());
    collected.extend_from_slice(failure.as_bytes());
    if prefix.is_empty() {
        failure
    } else {
        let mut output = String::with_capacity(prefix.len() + failure.len());
        output.push_str(prefix);
        output.push_str(&failure);
        output
    }
}

fn sse_event_boundary_prefix(collected: &[u8]) -> &'static str {
    if collected.is_empty() || collected_ends_with_sse_event_boundary(collected) {
        ""
    } else if collected.ends_with(b"\n") {
        "\n"
    } else {
        "\n\n"
    }
}

fn collected_ends_with_sse_event_boundary(collected: &[u8]) -> bool {
    collected.ends_with(b"\n\n")
        || collected.ends_with(b"\r\n\r\n")
        || collected.ends_with(b"\n\r\n")
}

async fn send_codex_request_with_upstream_retries_deps(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<crate::codex::gateway::transport::http_client::CodexBackendResponse, CodexClientError> {
    let result = retry_upstream_5xx(
        || send_codex_request(deps, request, account, request_id),
        request_id,
        &request.model,
    )
    .await;
    if let Ok(response) = &result {
        apply_rate_limit_headers_with_deps(
            deps,
            &account.id,
            account.plan_type.as_deref(),
            &response.rate_limit_headers,
        )
        .await;
    }
    result
}

async fn send_compact_request_with_upstream_retries_deps(
    deps: &CodexUpstreamDependencies,
    request: &CodexCompactRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexCompactResponse, CodexClientError> {
    let result = retry_upstream_5xx(
        || send_compact_request(deps, request, account, request_id),
        request_id,
        &request.model,
    )
    .await;
    if let Ok(response) = &result {
        apply_rate_limit_headers_with_deps(
            deps,
            &account.id,
            account.plan_type.as_deref(),
            &response.rate_limit_headers,
        )
        .await;
    }
    result
}

async fn send_codex_request(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<crate::codex::gateway::transport::http_client::CodexBackendResponse, CodexClientError> {
    let request_domain = request_domain(&deps.config.api.base_url);
    let cookie_header = match (deps.cookie_repository.as_ref(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };

    let account_scope = &account.id;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        account_scope,
    );

    let installation_id = get_installation_id(Some(&deps.config.database.url));

    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        deps.fingerprint.clone(),
    )
    .with_websocket_pool(deps.websocket_pool.clone(), account.id.clone());
    client
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
                installation_id: Some(&installation_id),
                session_id: identity.conversation_id.as_deref(),
            },
        )
        .await
}

async fn send_compact_request(
    deps: &CodexUpstreamDependencies,
    request: &CodexCompactRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexCompactResponse, CodexClientError> {
    let request_domain = request_domain(&deps.config.api.base_url);
    let cookie_header = match (deps.cookie_repository.as_ref(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };

    let installation_id = get_installation_id(Some(&deps.config.database.url));
    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        deps.fingerprint.clone(),
    );

    client
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
                installation_id: Some(&installation_id),
                session_id: None,
            },
        )
        .await
}

async fn send_codex_stream_request_with_upstream_retries(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendStream, CodexClientError> {
    retry_upstream_5xx(
        || send_codex_stream_request(deps, request, account, request_id),
        request_id,
        &request.model,
    )
    .await
}

async fn send_codex_stream_request(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendStream, CodexClientError> {
    let request_domain = request_domain(&deps.config.api.base_url);
    let cookie_header = match (deps.cookie_repository.as_ref(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };

    // Build conversation identity for session affinity
    // Use account.id as the scope since entry_id doesn't exist
    let account_scope = &account.id;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        account_scope,
    );

    // Get installation ID (cached after first call)
    let installation_id = get_installation_id(Some(&deps.config.database.url));

    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        deps.fingerprint.clone(),
    )
    .with_websocket_pool(deps.websocket_pool.clone(), account.id.clone());
    client
        .stream_response(
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
                installation_id: Some(&installation_id),
                session_id: identity.conversation_id.as_deref(),
            },
        )
        .await
}

async fn send_codex_websocket_stream_request_with_upstream_retries(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendWebSocketStream, CodexClientError> {
    retry_upstream_5xx(
        || send_codex_websocket_stream_request(deps, request, account, request_id),
        request_id,
        &request.model,
    )
    .await
}

async fn retry_upstream_5xx<T, F, Fut>(
    mut operation: F,
    request_id: &str,
    model: &str,
) -> Result<T, CodexClientError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, CodexClientError>>,
{
    let mut attempt = 0;
    loop {
        match operation().await {
            Err(error)
                if is_retryable_upstream_5xx(&error) && attempt < MAX_UPSTREAM_5XX_RETRIES =>
            {
                let delay = upstream_5xx_retry_delay(attempt);
                tracing::warn!(
                    error = %error,
                    request_id = %request_id,
                    model = %model,
                    retry_attempt = attempt + 1,
                    max_retries = MAX_UPSTREAM_5XX_RETRIES,
                    delay_ms = delay.as_millis(),
                    "Codex 上游 5xx，按原版策略同账户重试"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            result => return result,
        }
    }
}

fn is_retryable_upstream_5xx(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, .. } if status.is_server_error()
    )
}

fn upstream_5xx_retry_delay(attempt: u8) -> StdDuration {
    StdDuration::from_millis(UPSTREAM_5XX_RETRY_BASE_DELAY_MS * (1_u64 << u32::from(attempt)))
}

async fn send_codex_websocket_stream_request(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Result<CodexBackendWebSocketStream, CodexClientError> {
    let request_domain = request_domain(&deps.config.api.base_url);
    let cookie_header = match (deps.cookie_repository.as_ref(), request_domain.as_deref()) {
        (Some(repo), Some(domain)) => repo.cookie_header(&account.id, domain).await.ok().flatten(),
        _ => None,
    };

    let account_scope = &account.id;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        account_scope,
    );
    let installation_id = get_installation_id(Some(&deps.config.database.url));

    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        deps.fingerprint.clone(),
    )
    .with_websocket_pool(deps.websocket_pool.clone(), account.id.clone());
    client
        .websocket_stream_response(
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
                installation_id: Some(&installation_id),
                session_id: identity.conversation_id.as_deref(),
            },
        )
        .await
}

async fn record_response_affinity_with_deps(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account_id: &str,
    body: &str,
    turn_state: Option<&str>,
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
                "解析已完成响应 metadata 用于 session affinity 失败"
            );
            return;
        }
    };

    let variant_hash = compute_variant_hash(request);
    let response_id = metadata.response_id;
    let entry = deps
        .session_affinity
        .record(
            response_id.clone(),
            account_id.to_string(),
            conversation_id.to_string(),
            turn_state
                .filter(|value| !value.trim().is_empty())
                .map(ToString::to_string)
                .or_else(|| request.turn_state.clone()),
            Some(&request.instructions),
            usage.map(|usage| usage.input_tokens),
            Some(metadata.function_call_ids),
            Some(variant_hash.clone()),
        )
        .await;
    deps.reasoning_replay
        .record(
            &response_id,
            account_id,
            conversation_id,
            &variant_hash,
            &metadata.replay_items,
        )
        .await;
    if let Some(repository) = deps.session_affinity_repository.as_ref() {
        if let Err(error) = repository
            .upsert(&response_id, &entry, deps.session_affinity.ttl())
            .await
        {
            tracing::warn!(
                error = %error,
                response_id = %response_id,
                account_id = %account_id,
                "持久化 session affinity 失败"
            );
        }
    }
}

async fn evict_reasoning_replay_with_deps(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account_id: &str,
) {
    let Some(conversation_id) = request
        .prompt_cache_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let variant_hash = compute_variant_hash(request);
    let evicted = deps
        .reasoning_replay
        .evict_by_identity(account_id, conversation_id, &variant_hash)
        .await;
    if evicted > 0 {
        tracing::info!(
            account_id = %account_id,
            conversation_id = %conversation_id,
            variant_hash = %variant_hash,
            evicted,
            "已驱逐无效 encrypted content 对应的 reasoning replay"
        );
    }
}

async fn responses_websocket_stream(
    deps: CodexUpstreamDependencies,
    mut request: CodexResponsesRequest,
    mut acquired: AcquiredAccount,
    mut log_context: CodexRequestLogContext,
    mut implicit_resume: Option<ImplicitResumeSnapshot>,
) -> Response {
    let mut excluded_account_ids = Vec::new();
    let mut history_recovery_used = false;
    let mut model_unsupported_retry_used = false;
    let stream_response = loop {
        stagger_request_with_deps(&deps, acquired.previous_slot_at).await;
        let response = send_codex_websocket_stream_request_with_upstream_retries(
            &deps,
            &request,
            &acquired.account,
            log_context.request_id.as_str(),
        )
        .await;

        match response {
            Ok(response) => break response,
            Err(error) => {
                if transport_for_request(&request) == CodexTransport::WebSocketPreferred
                    && websocket_stream_error_allows_http_sse_fallback(&error)
                {
                    acquired.previous_slot_at = None;
                    return responses_http_sse_stream(
                        deps,
                        request,
                        acquired,
                        log_context,
                        implicit_resume,
                    )
                    .await;
                }
                deps.account_pool.lock().await.release(&acquired.account.id);
                if let Some(recovery) =
                    classify_upstream_request_recovery(&error, history_recovery_used)
                {
                    apply_upstream_request_recovery_with_deps(
                        &deps,
                        &mut request,
                        recovery,
                        true,
                        &log_context,
                        &mut history_recovery_used,
                        &mut implicit_resume,
                    )
                    .await;
                    continue;
                }
                if let Some(retry) =
                    classify_upstream_account_retry(&error, model_unsupported_retry_used)
                {
                    if retry.is_model_unsupported() {
                        model_unsupported_retry_used = true;
                    }
                    if request.previous_response_id.is_some()
                        && retry.preserve_history_account_affinity()
                    {
                        // previous_response_id 的历史由上游账号持有，换账号会静默丢失会话上下文。
                        apply_upstream_account_retry_with_deps(&deps, &acquired.account, retry)
                            .await;
                        log_codex_upstream_response_with_deps(
                            &deps,
                            &log_context,
                            retry.status(),
                            EventLevel::Warn,
                            "v1 responses WebSocket history 请求保持原账户",
                            websocket_history_retry_metadata(retry, true),
                        )
                        .await;
                    } else {
                        let fallback = apply_upstream_retry_and_acquire_fallback_with_deps(
                            &deps,
                            &acquired.account,
                            retry,
                            &request.model,
                            &mut excluded_account_ids,
                        )
                        .await;
                        log_codex_upstream_response_with_deps(
                            &deps,
                            &log_context,
                            retry.status(),
                            EventLevel::Warn,
                            "v1 responses WebSocket 上游请求将使用备用账户重试",
                            retry.metadata(true),
                        )
                        .await;
                        if let Some(fallback) = fallback {
                            log_context = log_context.with_account(&fallback.account.id);
                            acquired = fallback;
                            continue;
                        }
                        let retry_message =
                            retry.fallback_response_message(codex_client_error_message(&error));
                        let message =
                            fallback_exhausted_message_with_deps(&deps, &retry_message).await;
                        let error_response = codex_client_error_response_with_status_and_message(
                            error,
                            retry.status(),
                            &message,
                        );
                        log_codex_upstream_response_with_deps(
                            &deps,
                            &log_context,
                            error_response.0,
                            EventLevel::Error,
                            "v1 responses WebSocket stream fallback 已耗尽",
                            json!({"stream": true, "transport": "websocket"}),
                        )
                        .await;
                        return error_response.into_response();
                    }
                }
                record_failed_request_attempt_with_deps(&deps, &acquired.account.id).await;
                let error_response = codex_client_error_response(error);
                log_codex_upstream_response_with_deps(
                    &deps,
                    &log_context,
                    error_response.0,
                    EventLevel::Error,
                    "v1 responses WebSocket stream 上游请求失败",
                    json!({"stream": true, "transport": "websocket"}),
                )
                .await;
                return error_response.into_response();
            }
        }
    };

    if persist_upstream_cookies_with_deps(
        &deps,
        &acquired.account.id,
        &stream_response.set_cookie_headers,
    )
    .await
    .is_err()
    {
        deps.account_pool.lock().await.release(&acquired.account.id);
        log_codex_upstream_response_with_deps(
            &deps,
            &log_context,
            StatusCode::INTERNAL_SERVER_ERROR,
            EventLevel::Error,
            "v1 responses WebSocket stream 持久化 cookie 失败",
            json!({"stream": true, "transport": "websocket", "cookieStoreError": true}),
        )
        .await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(openai_error(
                "Failed to persist upstream cookies",
                "cookie_store_error",
            )),
        )
            .into_response();
    }

    let tuple_schema = request.tuple_schema.clone();
    let audit = WebSocketStreamAudit::new(
        deps,
        log_context,
        acquired.account.id,
        acquired.account.plan_type,
        request,
        stream_response.turn_state,
        stream_response.rate_limit_headers,
        stream_response.rate_limit_updates,
    );
    let upstream = stream_response.body_stream;

    use tokio::time::{interval_at, Duration, Instant as TokioInstant};
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
    const HEARTBEAT_CHUNK: &str = ": ping\n\n";
    let heartbeat_timer = interval_at(TokioInstant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
    let tuple_reconverter = TupleStreamReconverter::new(tuple_schema);

    let body_stream = futures_stream::unfold(
        Some((
            upstream,
            Vec::new(),
            audit,
            heartbeat_timer,
            tuple_reconverter,
        )),
        |state| async move {
            let (
                mut upstream,
                mut collected,
                mut audit,
                mut heartbeat_timer,
                mut tuple_reconverter,
            ) = state?;

            tokio::select! {
                chunk_result = upstream.next() => {
                    match chunk_result {
                        Some(Ok(chunk)) => {
                            collected.extend_from_slice(chunk.as_bytes());
                            let chunk = tuple_reconverter.transform_chunk(&chunk);
                            Some((
                                Ok::<String, std::io::Error>(chunk),
                                Some((
                                    upstream,
                                    collected,
                                    audit,
                                    heartbeat_timer,
                                    tuple_reconverter,
                                )),
                            ))
                        }
                        Some(Err(error)) => {
                            if collected_has_terminal_sse_event(&collected) {
                                let tail = tuple_reconverter.finish();
                                audit.complete(&collected).await;
                                if tail.is_empty() {
                                    None
                                } else {
                                    Some((Ok::<String, std::io::Error>(tail), None))
                                }
                            } else {
                                let detail = error.to_string();
                                let mut output = tuple_reconverter.finish();
                                let failure = append_premature_close_failed_event(
                                    &mut collected,
                                    Some(detail.as_str()),
                                );
                                output.push_str(&failure);
                                audit.complete(&collected).await;
                                Some((Ok::<String, std::io::Error>(output), None))
                            }
                        }
                        None => {
                            if collected_has_terminal_sse_event(&collected) {
                                let tail = tuple_reconverter.finish();
                                audit.complete(&collected).await;
                                if tail.is_empty() {
                                    None
                                } else {
                                    Some((Ok::<String, std::io::Error>(tail), None))
                                }
                            } else {
                                let mut output = tuple_reconverter.finish();
                                let failure =
                                    append_premature_close_failed_event(&mut collected, None);
                                output.push_str(&failure);
                                audit.complete(&collected).await;
                                Some((Ok::<String, std::io::Error>(output), None))
                            }
                        }
                    }
                }
                _ = heartbeat_timer.tick() => {
                    Some((
                        Ok::<String, std::io::Error>(HEARTBEAT_CHUNK.to_string()),
                        Some((
                            upstream,
                            collected,
                            audit,
                            heartbeat_timer,
                            tuple_reconverter,
                        )),
                    ))
                }
            }
        },
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(openai_error(
                    "Failed to build stream response",
                    "stream_response_error",
                )),
            )
                .into_response()
        })
}

async fn persist_upstream_cookies_with_deps(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
    set_cookie_headers: &[String],
) -> Result<(), ()> {
    let Some(cookie_repo) = deps.cookie_repository.as_ref() else {
        return Ok(());
    };
    for cookie in set_cookie_headers {
        cookie_repo
            .capture_set_cookie(account_id, cookie)
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct CodexRequestLogContext {
    request_id: String,
    account_id: String,
    model: String,
    stream: bool,
    started_at: Instant,
}

impl CodexRequestLogContext {
    pub(crate) fn new(
        request_id: &str,
        account_id: &str,
        model: &str,
        stream: bool,
        started_at: Instant,
    ) -> Self {
        Self {
            request_id: request_id.to_string(),
            account_id: account_id.to_string(),
            model: model.to_string(),
            stream,
            started_at,
        }
    }

    fn latency_ms(&self) -> i64 {
        self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
    }

    pub(crate) fn with_account(&self, account_id: &str) -> Self {
        Self {
            request_id: self.request_id.clone(),
            account_id: account_id.to_string(),
            model: self.model.clone(),
            stream: self.stream,
            started_at: self.started_at,
        }
    }
}

async fn log_codex_upstream_response_with_deps(
    deps: &CodexUpstreamDependencies,
    context: &CodexRequestLogContext,
    status: StatusCode,
    level: EventLevel,
    message: &str,
    mut metadata: Value,
) {
    ensure_stream_metadata(&mut metadata, context.stream);
    let mut event = EventLog::new("v1.response", level, message);
    event.request_id = Some(context.request_id.clone());
    event.account_id = Some(context.account_id.clone());
    event.route = Some("/v1/responses".to_string());
    event.model = Some(context.model.clone());
    event.status_code = Some(i64::from(status.as_u16()));
    event.latency_ms = Some(context.latency_ms());
    event.metadata = metadata;
    if let Err(error) = deps.logs.record(event).await {
        tracing::warn!(error = %error, "写入 v1 response 事件日志失败");
    }
}

#[cfg(test)]
mod tests {
    use super::{append_premature_close_failed_event, jitter_request_interval_ms_with_factor};

    #[test]
    fn jitter_request_interval_ms_with_factor_should_match_original_bounds() {
        assert_eq!(jitter_request_interval_ms_with_factor(300, 0.7), 210);
        assert_eq!(jitter_request_interval_ms_with_factor(300, 1.3), 390);
        assert_eq!(jitter_request_interval_ms_with_factor(300, 0.1), 210);
        assert_eq!(jitter_request_interval_ms_with_factor(300, 2.0), 390);
    }

    #[test]
    fn append_premature_close_failed_event_should_close_partial_sse_event_before_failure() {
        let mut collected =
            b"event: response.output_text.delta\ndata: {\"delta\":\"partial\"}\n".to_vec();

        append_premature_close_failed_event(&mut collected, None);

        let body = String::from_utf8(collected).unwrap();
        let failure = super::stream::responses_sse_failure(&body).unwrap();
        let metadata = failure.unwrap().metadata(true);
        assert_eq!(metadata["upstreamCode"], "stream_disconnected");
    }
}
