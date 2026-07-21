//! 唯一的账号重试、Provider instance 切换、发送与下游 commit barrier owner。

use std::collections::{BTreeSet, VecDeque};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::{FutureExt, StreamExt, pin_mut, select_biased};
use futures_timer::Delay;
use uuid::Uuid;

use crate::accounting::{CostEstimate, CostSource, Usage};
use crate::engine::continuation::ContinuationBinding;
use crate::engine::provider::{Provider, ProviderCallMetadata, ProviderRequest, ProviderStream};
use crate::engine::{
    AccountAttemptContext, AttemptContext, AttemptRecord, AttemptTrigger, CancellationToken,
    CommitRequirement, ContinuationAttempt, CoordinatedEvent, EngineError, ExecutionOutcome,
    ExecutionStore, GatewayEngine, IntermediateFailure, ModelRequestFinalization, ModelRequestId,
    ModelRequestTimings, NewModelRequest, ProviderAccountStateOwner, ProviderAttemptOutcome,
    RequestAttemptContext, UpstreamSendState,
};
use crate::error::{GatewayError, GatewayErrorKind, ProviderError, ProviderErrorKind};
use crate::event::{
    GatewayEvent, ProviderEvent, ProviderResponseHeader, ProviderResponseObservation,
};
use crate::operation::{Operation, RetrySafety};
use crate::routing::RoutingPlan;

/// Request 级协调器；不会创建或写入 `request_attempts`。
pub struct AttemptCoordinator<S: ?Sized> {
    engine: Arc<GatewayEngine<S>>,
}

impl<S: ?Sized> AttemptCoordinator<S>
where
    S: ExecutionStore,
{
    #[must_use]
    pub fn new(engine: GatewayEngine<S>) -> Self {
        Self {
            engine: Arc::new(engine),
        }
    }

    /// 先创建唯一请求行，再返回由 Core 完整拥有 retry/commit 的流式会话。
    ///
    /// # Errors
    ///
    /// 初始持久化、取消或已过 deadline 时返回稳定错误。
    pub async fn start(
        &self,
        request: NewModelRequest,
        operation: Operation,
        plan: RoutingPlan,
        required_account: Option<crate::engine::credential::ProviderAccountId>,
        continuation: Option<ContinuationBinding>,
        cancellation: CancellationToken,
    ) -> Result<ResponseExecutionSession<S>, EngineError> {
        let request_id = request.id.clone();
        let client_api_key_ref = request.client_api_key_ref.clone();
        let request_started_at = request.started_at;
        let deadline = request.deadline_at;
        self.engine.store().create_model_request(request).await?;
        let gateway_response_id = format!("resp_{}", Uuid::now_v7().simple());
        let account_state_owner = continuation
            .as_ref()
            .and_then(ContinuationBinding::pinned)
            .map(ProviderAccountStateOwner::from_continuation);
        let continuation_attempt =
            initial_continuation_attempt(&operation, &plan, continuation.as_ref());
        let commit_policy = if continuation_attempt == ContinuationAttempt::None {
            StreamCommitPolicy::FirstCanonicalEvent
        } else {
            StreamCommitPolicy::UntilOutputOrTerminal
        };

        let mut session = ResponseExecutionSession {
            engine: Arc::clone(&self.engine),
            request_id,
            client_api_key_ref,
            request_started_at,
            deadline,
            operation,
            plan,
            required_account,
            continuation,
            continuation_attempt,
            commit_policy,
            account_state_owner,
            cancellation,
            candidate_index: 0,
            last_attempt_candidate_index: None,
            attempts: 0,
            excluded_accounts: BTreeSet::new(),
            credential_recovery_attempted_accounts: BTreeSet::new(),
            current: None,
            downstream_committed_at: None,
            client_status_code: None,
            delivery_pending: false,
            upstream_complete: false,
            finalized: false,
            usage: Usage::new(),
            cost: CostEstimate::unavailable(),
            timings: ModelRequestTimings::default(),
            gateway_response_id,
            client_response_id: None,
            upstream_response_id: None,
            provider_attempt_outcomes: Vec::new(),
            before_commit: VecDeque::new(),
        };

        if session.cancellation.is_cancelled() {
            session.finish_interruption(EngineError::Cancelled).await?;
            return Err(EngineError::Cancelled);
        }
        if SystemTime::now() >= deadline {
            session.finish_interruption(EngineError::Deadline).await?;
            return Err(EngineError::Deadline);
        }
        Ok(session)
    }
}

struct CurrentAttempt {
    stream: ProviderStream,
    metadata: ProviderCallMetadata,
    trigger: AttemptTrigger,
    index: NonZeroU32,
    started_at: SystemTime,
    send_observed: bool,
    response_observation: Option<ProviderResponseObservation>,
}

struct FailureFinalization {
    outcome: ExecutionOutcome,
    send_state: UpstreamSendState,
    error: GatewayError,
    upstream_status_code: Option<u16>,
    provider_error_code: Option<String>,
    retry_after_ms: Option<u64>,
    provider_response_id: Option<String>,
}

#[derive(Clone, Copy)]
enum StreamCommitPolicy {
    FirstCanonicalEvent,
    UntilOutputOrTerminal,
}

/// API 可逐事件消费的 Core 执行会话。
///
/// API 只能提交下游 delivery 边界；账号重试、Provider instance 切换、断流终结与
/// `model_requests` 写回均留在本类型内。
pub struct ResponseExecutionSession<S: ?Sized> {
    engine: Arc<GatewayEngine<S>>,
    request_id: ModelRequestId,
    client_api_key_ref: crate::policy::ClientApiKeyId,
    request_started_at: SystemTime,
    deadline: SystemTime,
    operation: Operation,
    plan: RoutingPlan,
    required_account: Option<crate::engine::credential::ProviderAccountId>,
    continuation: Option<ContinuationBinding>,
    continuation_attempt: ContinuationAttempt,
    commit_policy: StreamCommitPolicy,
    account_state_owner: Option<ProviderAccountStateOwner>,
    cancellation: CancellationToken,
    candidate_index: usize,
    last_attempt_candidate_index: Option<usize>,
    attempts: u32,
    excluded_accounts: BTreeSet<crate::engine::credential::ProviderAccountId>,
    credential_recovery_attempted_accounts: BTreeSet<crate::engine::credential::ProviderAccountId>,
    current: Option<CurrentAttempt>,
    downstream_committed_at: Option<SystemTime>,
    client_status_code: Option<u16>,
    delivery_pending: bool,
    upstream_complete: bool,
    finalized: bool,
    usage: Usage,
    cost: CostEstimate,
    timings: ModelRequestTimings,
    gateway_response_id: String,
    client_response_id: Option<String>,
    upstream_response_id: Option<String>,
    provider_attempt_outcomes: Vec<ProviderAttemptOutcome>,
    before_commit: VecDeque<ProviderEvent>,
}

impl<S: ?Sized> ResponseExecutionSession<S>
where
    S: ExecutionStore,
{
    /// 读取下一条 canonical event；首条未提交事件会携带 commit 要求。
    ///
    /// # Errors
    ///
    /// 未提交上一条首事件、Provider/Store 失败、取消或超时时返回错误。
    pub async fn next_event(&mut self) -> Result<Option<CoordinatedEvent>, EngineError> {
        if self.delivery_pending {
            return Err(EngineError::DownstreamCommitRequired);
        }
        if self.finalized {
            return Ok(None);
        }
        if self.downstream_committed_at.is_some()
            && let Some(event) = self.before_commit.pop_front()
        {
            return Ok(Some(CoordinatedEvent::single(
                event,
                CommitRequirement::AlreadyCommitted,
            )));
        }

        loop {
            match self.pull().await? {
                PullOutcome::Event(event) => {
                    if self.downstream_committed_at.is_some() {
                        return Ok(Some(CoordinatedEvent::single(
                            event,
                            CommitRequirement::AlreadyCommitted,
                        )));
                    }
                    let commit_significant = match self.commit_policy {
                        StreamCommitPolicy::FirstCanonicalEvent => event.has_canonical_facts(),
                        StreamCommitPolicy::UntilOutputOrTerminal => event.is_commit_significant(),
                    };
                    self.before_commit.push_back(event);
                    if !commit_significant {
                        continue;
                    }
                    self.delivery_pending = true;
                    let events = self.before_commit.drain(..).collect();
                    return CoordinatedEvent::try_batch(
                        events,
                        CommitRequirement::CommitBeforeDelivery,
                    )
                    .map(Some);
                }
                PullOutcome::AttemptDiscarded => {
                    self.before_commit.clear();
                }
                PullOutcome::End => {
                    if self.downstream_committed_at.is_some() {
                        self.finish_success().await?;
                    }
                    return Ok(None);
                }
            }
        }
    }

    /// 非流式协议可在任何下游提交前收集一个完整、可丢弃重试的结果。
    ///
    /// # Errors
    ///
    /// 会话已提交、已有待提交结果或执行失败时返回错误。
    pub async fn collect_uncommitted(&mut self) -> Result<Vec<ProviderEvent>, EngineError> {
        if self.downstream_committed_at.is_some() || self.delivery_pending {
            return Err(EngineError::InvalidDeliveryState);
        }
        if self.finalized {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        loop {
            match self.pull().await? {
                PullOutcome::Event(event) => events.push(event),
                PullOutcome::AttemptDiscarded => events.clear(),
                PullOutcome::End => {
                    if events.is_empty() {
                        return Err(EngineError::InvalidDeliveryState);
                    }
                    self.delivery_pending = true;
                    return Ok(events);
                }
            }
        }
    }

    /// 在协议 adapter 真正写出首字节前持久化下游不可撤回边界。
    ///
    /// # Errors
    ///
    /// 没有待提交结果、重复提交或 Store 失败时返回错误。
    pub async fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> Result<(), EngineError> {
        if !self.delivery_pending || self.downstream_committed_at.is_some() || self.finalized {
            return Err(EngineError::InvalidDeliveryState);
        }
        let committed_at = SystemTime::now();
        self.engine
            .store()
            .mark_downstream_committed(&self.request_id, committed_at, client_status_code)
            .await?;
        self.downstream_committed_at = Some(committed_at);
        self.client_status_code = client_status_code;
        self.delivery_pending = false;
        if self.upstream_complete {
            self.finish_success().await?;
        }
        Ok(())
    }

    /// 在 HTTP adapter 已确定首字节前错误响应后补写最终状态。
    ///
    /// # Errors
    ///
    /// 状态已经写入或 Store 无法写回时返回错误。
    pub async fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> Result<(), EngineError> {
        if self.client_status_code.is_some() {
            return Err(EngineError::InvalidDeliveryState);
        }
        self.engine
            .store()
            .record_client_status(&self.request_id, client_status_code)
            .await?;
        self.client_status_code = Some(client_status_code);
        Ok(())
    }

    #[must_use]
    pub const fn is_finalized(&self) -> bool {
        self.finalized
    }

    /// 返回截至当前已完成的实际上游调用结果。
    ///
    /// 调用方可保存已消费下标；该切片在会话生命周期内只追加、不改写。
    #[must_use]
    pub fn provider_attempt_outcomes(&self) -> &[ProviderAttemptOutcome] {
        &self.provider_attempt_outcomes
    }

    /// 返回最终选中 attempt 已公开给协议层的安全响应头。
    #[must_use]
    pub fn response_headers(&self) -> &[ProviderResponseHeader] {
        self.current
            .as_ref()
            .and_then(|current| current.response_observation.as_ref())
            .map(ProviderResponseObservation::client_headers)
            .unwrap_or_default()
    }

    /// 请求取消；实际终态在下一次会话 poll 时由 Core 持久化。
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// 丢弃尚未提交的 delivery，并立即把请求收敛为取消终态。
    ///
    /// # Errors
    ///
    /// Store 无法写回发送状态或最终请求行时返回错误。
    pub async fn cancel_and_finalize(&mut self) -> Result<(), EngineError> {
        self.cancellation.cancel();
        if self.finalized {
            return Ok(());
        }
        self.delivery_pending = false;
        self.finish_interruption(EngineError::Cancelled).await
    }

    async fn pull(&mut self) -> Result<PullOutcome, EngineError> {
        loop {
            if self.current.is_none() {
                self.prepare_attempt().await?;
            }

            let boundary = {
                let current = self.current.as_mut().ok_or(EngineError::EmptyRoutingPlan)?;
                poll_stream_item(
                    &mut current.stream,
                    self.cancellation.clone(),
                    self.deadline,
                )
                .await
            };

            match boundary {
                PollBoundary::Cancelled => {
                    self.finish_interruption(EngineError::Cancelled).await?;
                    return Err(EngineError::Cancelled);
                }
                PollBoundary::Deadline => {
                    self.record_current_provider_failure(ProviderErrorKind::Timeout);
                    self.finish_interruption(EngineError::Deadline).await?;
                    return Err(EngineError::Deadline);
                }
                PollBoundary::Item(Some(Ok(mut event))) => {
                    if let Some(observation) = event.take_observation()
                        && let Err(error) = self.observe_response(observation)
                    {
                        if self.handle_stream_error(error).await? {
                            return Ok(PullOutcome::AttemptDiscarded);
                        }
                        continue;
                    }
                    let mut identity_error = None;
                    for fact in event.canonical_facts_mut() {
                        if let Err(error) = self.freeze_response_identity(fact) {
                            identity_error = Some(error);
                            break;
                        }
                    }
                    if let Some(error) = identity_error {
                        if self.handle_stream_error(error).await? {
                            return Ok(PullOutcome::AttemptDiscarded);
                        }
                        continue;
                    }
                    for fact in event.canonical_facts() {
                        self.observe_event(fact).await?;
                    }
                    if event.wire_event().is_some() && !event.has_canonical_facts() {
                        self.observe_wire_event().await?;
                    }
                    if !event.has_client_event() {
                        continue;
                    }
                    return Ok(PullOutcome::Event(event));
                }
                PollBoundary::Item(Some(Err(error))) => {
                    if self.handle_stream_error(error).await? {
                        return Ok(PullOutcome::AttemptDiscarded);
                    }
                }
                PollBoundary::Item(None) => {
                    self.record_current_provider_success();
                    self.upstream_complete = true;
                    return Ok(PullOutcome::End);
                }
            }
        }
    }

    async fn prepare_attempt(&mut self) -> Result<(), EngineError> {
        loop {
            if self.attempts >= self.plan.max_attempts().get() {
                return Err(EngineError::EmptyRoutingPlan);
            }
            let Some(candidate) = self.plan.candidates().get(self.candidate_index).cloned() else {
                let error = GatewayError::new(
                    GatewayErrorKind::NoAvailableProvider,
                    "no upstream account or Provider instance is available",
                );
                self.finish_failure(FailureFinalization {
                    outcome: ExecutionOutcome::Failed,
                    send_state: self.current_send_state(),
                    error,
                    upstream_status_code: None,
                    provider_error_code: None,
                    retry_after_ms: None,
                    provider_response_id: None,
                })
                .await?;
                return Err(EngineError::EmptyRoutingPlan);
            };
            let next_attempt = self
                .attempts
                .checked_add(1)
                .and_then(NonZeroU32::new)
                .ok_or(EngineError::EmptyRoutingPlan)?;
            let context = AttemptContext::new(
                RequestAttemptContext::new(
                    self.request_id.clone(),
                    self.client_api_key_ref.clone(),
                ),
                next_attempt,
                self.deadline,
                self.plan.account_selection_policy(),
                AccountAttemptContext::new(
                    self.excluded_accounts.clone(),
                    self.required_account.clone(),
                    self.account_state_owner.clone(),
                )
                .with_credential_recovery_attempted(
                    self.required_account.as_ref().is_some_and(|account| {
                        self.credential_recovery_attempted_accounts
                            .contains(account)
                    }),
                ),
                self.continuation.clone(),
                self.cancellation.clone(),
            )
            .with_continuation_attempt(self.continuation_attempt);
            let trigger = if self.attempts == 0 {
                AttemptTrigger::Initial
            } else if self.last_attempt_candidate_index == Some(self.candidate_index) {
                AttemptTrigger::AccountRetry
            } else {
                AttemptTrigger::InstanceFallback
            };
            let provider = self
                .engine
                .providers()
                .get(candidate.provider())
                .cloned()
                .ok_or_else(|| EngineError::ProviderNotRegistered {
                    provider: candidate.provider().as_str().to_owned(),
                })?;
            let provider_request = ProviderRequest::new(self.operation.clone(), candidate.clone());
            let stream = match poll_provider(
                provider,
                provider_request,
                context,
                self.cancellation.clone(),
                self.deadline,
            )
            .await
            {
                ProviderBoundary::Cancelled => {
                    self.finish_interruption(EngineError::Cancelled).await?;
                    return Err(EngineError::Cancelled);
                }
                ProviderBoundary::Deadline => {
                    self.record_provider_failure(
                        candidate.instance().clone(),
                        ProviderErrorKind::Timeout,
                    );
                    self.finish_interruption(EngineError::Deadline).await?;
                    return Err(EngineError::Deadline);
                }
                ProviderBoundary::Result(result) => match *result {
                    Ok(stream) => stream,
                    Err(error) => {
                        if error.kind() != ProviderErrorKind::Unavailable
                            || error.send_state() != UpstreamSendState::NotSent
                        {
                            self.record_provider_failure(
                                candidate.instance().clone(),
                                error.kind(),
                            );
                        }
                        let provider_proved_replay_safe = provider_proved_replay_safe(&error);
                        if self.can_fallback_instance(provider_proved_replay_safe) {
                            self.candidate_index += 1;
                            continue;
                        }
                        let engine_error = provider_engine_error(&error);
                        self.finish_provider_error(&error).await?;
                        return Err(engine_error);
                    }
                },
            };
            if !stream.metadata().confirms(&candidate) {
                self.record_provider_failure(
                    candidate.instance().clone(),
                    ProviderErrorKind::Protocol,
                );
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "provider metadata did not match the frozen candidate",
                );
                self.finish_failure(FailureFinalization {
                    outcome: ExecutionOutcome::Failed,
                    send_state: self.current_send_state(),
                    error,
                    upstream_status_code: None,
                    provider_error_code: None,
                    retry_after_ms: None,
                    provider_response_id: None,
                })
                .await?;
                return Err(EngineError::ProviderMetadataMismatch);
            }

            let metadata = stream.metadata().clone();
            if self
                .required_account
                .as_ref()
                .is_some_and(|required| metadata.provider_account_id() != Some(required))
            {
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "provider did not use the required account",
                );
                self.finish_failure(FailureFinalization {
                    outcome: ExecutionOutcome::Failed,
                    send_state: self.current_send_state(),
                    error,
                    upstream_status_code: None,
                    provider_error_code: None,
                    retry_after_ms: None,
                    provider_response_id: None,
                })
                .await?;
                return Err(EngineError::RequiredAccountMismatch);
            }
            if let Some(pin) = self
                .continuation
                .as_ref()
                .and_then(ContinuationBinding::pinned)
                && self.continuation_attempt == ContinuationAttempt::Native
                && !metadata.provider_account_id().is_some_and(|account| {
                    pin.matches(metadata.provider(), metadata.instance(), account)
                })
            {
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "native continuation binding did not match selected account",
                );
                self.finish_failure(FailureFinalization {
                    outcome: ExecutionOutcome::Failed,
                    send_state: self.current_send_state(),
                    error,
                    upstream_status_code: None,
                    provider_error_code: None,
                    retry_after_ms: None,
                    provider_response_id: None,
                })
                .await?;
                return Err(EngineError::ContinuationPinMismatch);
            }
            if self.account_state_owner.is_none()
                && let Some(account) = metadata.provider_account_id()
            {
                self.account_state_owner = Some(ProviderAccountStateOwner::new(
                    metadata.provider().clone(),
                    metadata.instance().clone(),
                    account.clone(),
                ));
            }
            self.engine
                .store()
                .record_attempt(AttemptRecord {
                    request_id: self.request_id.clone(),
                    attempt_count: next_attempt,
                    trigger,
                    provider_instance_id: metadata.instance().clone(),
                    provider_kind: metadata.provider().clone(),
                    provider_account_id: metadata.provider_account_id().cloned(),
                    provider_account_ref: metadata.provider_account_id().cloned(),
                    upstream_model_id: metadata.upstream_model().clone(),
                    upstream_transport: metadata.transport().as_str().to_owned(),
                    http_version: None,
                })
                .await?;
            self.attempts = next_attempt.get();
            self.last_attempt_candidate_index = Some(self.candidate_index);
            self.current = Some(CurrentAttempt {
                stream,
                metadata,
                trigger,
                index: next_attempt,
                started_at: SystemTime::now(),
                send_observed: false,
                response_observation: None,
            });
            return Ok(());
        }
    }

    async fn observe_event(&mut self, event: &GatewayEvent) -> Result<(), EngineError> {
        let observed_at = SystemTime::now();
        let elapsed = elapsed_ms(self.request_started_at, observed_at);
        observe_event_timing(&mut self.timings, event, elapsed);
        if let GatewayEvent::Usage(observed) = event {
            self.usage.merge(observed);
        }
        if let GatewayEvent::CalculatedCost(observed) = event
            && self.cost.source() != CostSource::ProviderReported
        {
            self.cost = observed.into_estimate();
        }
        if let GatewayEvent::ProviderCost(observed) = event {
            self.cost = observed.into_estimate();
        }
        let current = self.current.as_mut().ok_or(EngineError::EmptyRoutingPlan)?;
        if !current.send_observed {
            self.engine
                .store()
                .mark_send_state(&self.request_id, UpstreamSendState::Sent)
                .await?;
            current.send_observed = true;
        }
        Ok(())
    }

    async fn observe_wire_event(&mut self) -> Result<(), EngineError> {
        let current = self.current.as_mut().ok_or(EngineError::EmptyRoutingPlan)?;
        if !current.send_observed {
            self.engine
                .store()
                .mark_send_state(&self.request_id, UpstreamSendState::Sent)
                .await?;
            current.send_observed = true;
        }
        Ok(())
    }

    fn observe_response(
        &mut self,
        observation: ProviderResponseObservation,
    ) -> Result<(), ProviderError> {
        let send_state = self.current_send_state();
        let current = self
            .current
            .as_mut()
            .ok_or_else(|| ProviderError::new(ProviderErrorKind::Protocol, send_state))?;
        if current
            .response_observation
            .as_ref()
            .is_some_and(|existing| existing.transport() != observation.transport())
        {
            return Err(ProviderError::new(ProviderErrorKind::Protocol, send_state));
        }
        let observed = observation.timings();
        if let Some(value) = observed.transport_decision_wait_ms {
            self.timings.transport_decision_wait_ms = Some(value);
        }
        if let Some(value) = observed.connect_ms {
            self.timings.connect_ms = Some(value);
        }
        if let Some(value) = observed.headers_ms {
            self.timings.headers_ms = Some(value);
        }
        if let Some(value) = observed.first_event_ms {
            self.timings.first_event_ms = Some(value);
        }
        current.response_observation = Some(observation);
        Ok(())
    }

    fn freeze_response_identity(&mut self, event: &mut GatewayEvent) -> Result<(), ProviderError> {
        let upstream_response_id = event
            .freeze_gateway_response_id(&self.gateway_response_id)
            .map_err(|_| {
                ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
            })?;
        let Some(upstream_response_id) = upstream_response_id else {
            return Ok(());
        };
        if self
            .upstream_response_id
            .as_deref()
            .is_some_and(|expected| expected != upstream_response_id)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::Protocol,
                UpstreamSendState::Sent,
            ));
        }
        self.client_response_id = Some(self.gateway_response_id.clone());
        self.upstream_response_id = Some(upstream_response_id);
        Ok(())
    }

    /// 返回 `true` 表示调用方必须丢弃本 attempt 已收集的未提交事件。
    async fn handle_stream_error(&mut self, error: ProviderError) -> Result<bool, EngineError> {
        let current = self.current.take().ok_or(EngineError::EmptyRoutingPlan)?;
        self.record_provider_failure(current.metadata.instance().clone(), error.kind());
        let send_state = if current.send_observed {
            UpstreamSendState::Sent
        } else {
            error.send_state()
        };
        self.engine
            .store()
            .mark_send_state(&self.request_id, send_state)
            .await?;
        let provider_proved_replay_safe = provider_proved_replay_safe(&error);
        let continuation_retry = self.prepare_continuation_retry(
            &current,
            &error,
            send_state,
            provider_proved_replay_safe,
        );
        let ordinary_retry = self.required_account.is_none()
            && self.continuation_attempt == ContinuationAttempt::None
            && self.downstream_committed_at.is_none()
            && !self.delivery_pending
            && send_state != UpstreamSendState::Ambiguous
            && (self.operation.retry_safety() == RetrySafety::Idempotent
                || provider_proved_replay_safe)
            && self.attempts < self.plan.max_attempts().get();
        let same_account_retry = error.retries_same_account()
            && provider_proved_replay_safe
            && self.downstream_committed_at.is_none()
            && !self.delivery_pending
            && send_state != UpstreamSendState::Ambiguous
            && self.attempts < self.plan.max_attempts().get()
            && current
                .metadata
                .provider_account_id()
                .is_some_and(|account| {
                    !self
                        .credential_recovery_attempted_accounts
                        .contains(account)
                });
        let retryable = continuation_retry || same_account_retry || ordinary_retry;

        if retryable {
            if same_account_retry {
                if let Some(account) = current.metadata.provider_account_id() {
                    self.credential_recovery_attempted_accounts
                        .insert(account.clone());
                    self.required_account = Some(account.clone());
                }
            } else if !continuation_retry
                && let Some(account) = current.metadata.provider_account_id()
            {
                self.excluded_accounts.insert(account.clone());
            }
            if error.kind() == ProviderErrorKind::Unsupported
                && self.can_fallback_instance(provider_proved_replay_safe)
            {
                self.candidate_index += 1;
            }
            self.engine
                .store()
                .record_intermediate_failure(IntermediateFailure {
                    request_id: self.request_id.clone(),
                    attempt_index: current.index,
                    trigger: current.trigger,
                    instance_id: current.metadata.instance().clone(),
                    provider_kind: current.metadata.provider().clone(),
                    account_id: current.metadata.provider_account_id().cloned(),
                    upstream_model_id: current.metadata.upstream_model().clone(),
                    upstream_status_code: current
                        .response_observation
                        .as_ref()
                        .and_then(ProviderResponseObservation::status_code),
                    upstream_request_id: current
                        .response_observation
                        .as_ref()
                        .and_then(ProviderResponseObservation::request_id)
                        .map(|value| value.as_str().to_owned()),
                    latency: current.started_at.elapsed().unwrap_or_default(),
                    error,
                })
                .await?;
            self.reset_uncommitted_observations();
            return Ok(true);
        }

        let engine_error = provider_engine_error(&error);
        self.current = Some(current);
        self.finish_provider_error_with_send_state(&error, send_state)
            .await?;
        Err(engine_error)
    }

    fn prepare_continuation_retry(
        &mut self,
        current: &CurrentAttempt,
        error: &ProviderError,
        send_state: UpstreamSendState,
        provider_proved_replay_safe: bool,
    ) -> bool {
        if self.required_account.is_some()
            || self.continuation_attempt == ContinuationAttempt::None
            || self.downstream_committed_at.is_some()
            || self.delivery_pending
            || send_state == UpstreamSendState::Ambiguous
            || !provider_proved_replay_safe
            || self.attempts >= self.plan.max_attempts().get()
            || self
                .operation
                .provider_session_state(current.metadata.provider().as_str())
                .is_none()
        {
            return false;
        }

        match self.continuation_attempt {
            ContinuationAttempt::Native if error.continuation_failure().is_some() => {
                self.continuation_attempt = ContinuationAttempt::ReplayOwner;
            }
            ContinuationAttempt::Native | ContinuationAttempt::ReplayOwner => {
                self.continuation_attempt = ContinuationAttempt::ReplayAny;
                if let Some(account) = current.metadata.provider_account_id() {
                    self.excluded_accounts.insert(account.clone());
                }
            }
            ContinuationAttempt::ReplayAny => {
                if let Some(account) = current.metadata.provider_account_id() {
                    self.excluded_accounts.insert(account.clone());
                }
            }
            ContinuationAttempt::None => return false,
        }
        true
    }

    fn reset_uncommitted_observations(&mut self) {
        self.usage = Usage::new();
        self.cost = CostEstimate::unavailable();
        self.client_response_id = None;
        self.upstream_response_id = None;
        self.timings.transport_decision_wait_ms = None;
        self.timings.connect_ms = None;
        self.timings.headers_ms = None;
        self.timings.first_event_ms = None;
        self.timings.first_reasoning_ms = None;
        self.timings.first_text_ms = None;
        self.timings.first_token_ms = None;
        self.upstream_complete = false;
        self.before_commit.clear();
    }

    fn can_fallback_instance(&self, provider_proved_replay_safe: bool) -> bool {
        if self.required_account.is_some()
            || self.candidate_index + 1 >= self.plan.candidates().len()
        {
            return false;
        }
        let next = &self.plan.candidates()[self.candidate_index + 1];
        match self.continuation_attempt {
            ContinuationAttempt::None => {
                self.operation.retry_safety() == RetrySafety::Idempotent
                    || provider_proved_replay_safe
            }
            ContinuationAttempt::ReplayAny => {
                provider_proved_replay_safe
                    && self
                        .operation
                        .provider_session_state(next.provider().as_str())
                        .is_some()
            }
            ContinuationAttempt::Native | ContinuationAttempt::ReplayOwner => false,
        }
    }

    async fn finish_success(&mut self) -> Result<(), EngineError> {
        if self.finalized {
            return Ok(());
        }
        let completed_at = SystemTime::now();
        self.timings.latency_ms = Some(elapsed_ms(self.request_started_at, completed_at));
        let upstream_request_id = self.current.as_ref().and_then(|current| {
            current
                .response_observation
                .as_ref()
                .and_then(|observation| {
                    observation
                        .request_id()
                        .map(|value| value.as_str().to_owned())
                })
                .or_else(|| {
                    current
                        .metadata
                        .upstream_request_id()
                        .map(|value| value.as_str().to_owned())
                })
        });
        let upstream_status_code = self
            .current
            .as_ref()
            .and_then(|current| current.response_observation.as_ref())
            .and_then(ProviderResponseObservation::status_code)
            .or(Some(200));
        let (upstream_transport, http_version) = self.current_transport_observation();
        self.engine
            .store()
            .finalize_model_request(ModelRequestFinalization {
                request_id: self.request_id.clone(),
                outcome: ExecutionOutcome::Succeeded,
                send_state: UpstreamSendState::Sent,
                attempt_count: self.attempts,
                downstream_committed_at: self.downstream_committed_at,
                client_status_code: self.client_status_code,
                upstream_status_code,
                client_response_id: self.client_response_id.clone(),
                upstream_request_id,
                upstream_response_id: self.upstream_response_id.clone(),
                upstream_transport,
                http_version,
                error: None,
                provider_error_code: None,
                retry_after_ms: None,
                usage: self.usage.clone(),
                cost: self.cost.clone(),
                timings: self.timings.clone(),
                completed_at,
            })
            .await?;
        self.finalized = true;
        Ok(())
    }

    async fn finish_provider_error(&mut self, error: &ProviderError) -> Result<(), EngineError> {
        self.finish_provider_error_with_send_state(error, self.current_send_state())
            .await
    }

    async fn finish_provider_error_with_send_state(
        &mut self,
        error: &ProviderError,
        send_state: UpstreamSendState,
    ) -> Result<(), EngineError> {
        let outcome = if error.kind() == ProviderErrorKind::Cancelled {
            ExecutionOutcome::Cancelled
        } else if self.downstream_committed_at.is_some() {
            ExecutionOutcome::Incomplete
        } else {
            ExecutionOutcome::Failed
        };
        self.finish_failure(FailureFinalization {
            outcome,
            send_state,
            error: GatewayError::from_provider(error),
            upstream_status_code: error.upstream_status(),
            provider_error_code: error.upstream_code().map(|code| code.as_str().to_owned()),
            retry_after_ms: error.retry_after().map(duration_ms),
            provider_response_id: error
                .upstream_response_id()
                .map(|value| value.as_str().to_owned()),
        })
        .await
    }

    async fn finish_interruption(&mut self, error: EngineError) -> Result<(), EngineError> {
        let (outcome, gateway_error) = match error {
            EngineError::Cancelled => (
                ExecutionOutcome::Cancelled,
                GatewayError::new(GatewayErrorKind::Cancelled, "request was cancelled"),
            ),
            EngineError::Deadline => (
                if self.downstream_committed_at.is_some() {
                    ExecutionOutcome::Incomplete
                } else {
                    ExecutionOutcome::Failed
                },
                GatewayError::new(GatewayErrorKind::Timeout, "request deadline elapsed"),
            ),
            _ => (
                ExecutionOutcome::Failed,
                GatewayError::new(GatewayErrorKind::Internal, "request execution failed"),
            ),
        };
        let send_state = if self.attempts == 0 {
            UpstreamSendState::NotSent
        } else if self.downstream_committed_at.is_some()
            || self
                .current
                .as_ref()
                .is_some_and(|current| current.send_observed)
        {
            UpstreamSendState::Sent
        } else {
            UpstreamSendState::Ambiguous
        };
        if self.attempts > 0 {
            self.engine
                .store()
                .mark_send_state(&self.request_id, send_state)
                .await?;
        }
        self.finish_failure(FailureFinalization {
            outcome,
            send_state,
            error: gateway_error,
            upstream_status_code: None,
            provider_error_code: None,
            retry_after_ms: None,
            provider_response_id: None,
        })
        .await
    }

    async fn finish_failure(
        &mut self,
        finalization: FailureFinalization,
    ) -> Result<(), EngineError> {
        if self.finalized {
            return Ok(());
        }
        let completed_at = SystemTime::now();
        self.timings.latency_ms = Some(elapsed_ms(self.request_started_at, completed_at));
        let upstream_request_id = self.current.as_ref().and_then(|current| {
            current
                .response_observation
                .as_ref()
                .and_then(|observation| {
                    observation
                        .request_id()
                        .map(|value| value.as_str().to_owned())
                })
                .or_else(|| {
                    current
                        .metadata
                        .upstream_request_id()
                        .map(|value| value.as_str().to_owned())
                })
        });
        let observed_status_code = self
            .current
            .as_ref()
            .and_then(|current| current.response_observation.as_ref())
            .and_then(ProviderResponseObservation::status_code);
        let (upstream_transport, http_version) = self.current_transport_observation();
        self.engine
            .store()
            .finalize_model_request(ModelRequestFinalization {
                request_id: self.request_id.clone(),
                outcome: finalization.outcome,
                send_state: finalization.send_state,
                attempt_count: self.attempts,
                downstream_committed_at: self.downstream_committed_at,
                client_status_code: self.client_status_code,
                upstream_status_code: finalization.upstream_status_code.or(observed_status_code),
                client_response_id: self.client_response_id.clone(),
                upstream_request_id,
                upstream_response_id: finalization
                    .provider_response_id
                    .or_else(|| self.upstream_response_id.clone()),
                upstream_transport,
                http_version,
                error: Some(finalization.error),
                provider_error_code: finalization.provider_error_code,
                retry_after_ms: finalization.retry_after_ms,
                usage: self.usage.clone(),
                cost: self.cost.clone(),
                timings: self.timings.clone(),
                completed_at,
            })
            .await?;
        self.finalized = true;
        Ok(())
    }

    fn current_transport_observation(&self) -> (Option<String>, Option<String>) {
        let Some(current) = self.current.as_ref() else {
            return (None, None);
        };
        let Some(observation) = current.response_observation.as_ref() else {
            return (None, None);
        };
        (
            Some(observation.transport().as_str().to_owned()),
            observation
                .http_version()
                .map(|version| version.as_str().to_owned()),
        )
    }

    fn current_send_state(&self) -> UpstreamSendState {
        if self.attempts == 0 {
            UpstreamSendState::NotSent
        } else if self
            .current
            .as_ref()
            .is_some_and(|current| current.send_observed)
        {
            UpstreamSendState::Sent
        } else {
            UpstreamSendState::NotSent
        }
    }

    fn record_current_provider_success(&mut self) {
        let instance_id = self
            .current
            .as_ref()
            .map(|current| current.metadata.instance().clone());
        if let Some(instance_id) = instance_id {
            self.provider_attempt_outcomes
                .push(ProviderAttemptOutcome::Succeeded {
                    provider_instance_id: instance_id,
                });
        }
    }

    fn record_current_provider_failure(&mut self, error_kind: ProviderErrorKind) {
        let instance_id = self
            .current
            .as_ref()
            .map(|current| current.metadata.instance().clone());
        if let Some(instance_id) = instance_id {
            self.record_provider_failure(instance_id, error_kind);
        }
    }

    fn record_provider_failure(
        &mut self,
        provider_instance_id: crate::routing::ProviderInstanceId,
        error_kind: ProviderErrorKind,
    ) {
        self.provider_attempt_outcomes
            .push(ProviderAttemptOutcome::Failed {
                provider_instance_id,
                error_kind,
            });
    }
}

enum PullOutcome {
    Event(ProviderEvent),
    AttemptDiscarded,
    End,
}

enum PollBoundary {
    Item(Option<Result<ProviderEvent, ProviderError>>),
    Cancelled,
    Deadline,
}

async fn poll_stream_item(
    stream: &mut ProviderStream,
    cancellation: CancellationToken,
    deadline: SystemTime,
) -> PollBoundary {
    let Ok(remaining) = deadline.duration_since(SystemTime::now()) else {
        return PollBoundary::Deadline;
    };
    let next = stream.next().fuse();
    let cancelled = cancellation.cancelled().fuse();
    let timeout = Delay::new(remaining).fuse();
    pin_mut!(next, cancelled, timeout);
    select_biased! {
        _ = cancelled => PollBoundary::Cancelled,
        _ = timeout => PollBoundary::Deadline,
        item = next => PollBoundary::Item(item),
    }
}

enum ProviderBoundary {
    Result(Box<Result<ProviderStream, ProviderError>>),
    Cancelled,
    Deadline,
}

async fn poll_provider(
    provider: Arc<dyn Provider>,
    request: ProviderRequest,
    context: AttemptContext,
    cancellation: CancellationToken,
    deadline: SystemTime,
) -> ProviderBoundary {
    let Ok(remaining) = deadline.duration_since(SystemTime::now()) else {
        return ProviderBoundary::Deadline;
    };
    let execution = provider.execute(request, context).fuse();
    let cancelled = cancellation.cancelled().fuse();
    let timeout = Delay::new(remaining).fuse();
    pin_mut!(execution, cancelled, timeout);
    select_biased! {
        _ = cancelled => ProviderBoundary::Cancelled,
        _ = timeout => ProviderBoundary::Deadline,
        result = execution => ProviderBoundary::Result(Box::new(result)),
    }
}

fn initial_continuation_attempt(
    operation: &Operation,
    plan: &RoutingPlan,
    continuation: Option<&ContinuationBinding>,
) -> ContinuationAttempt {
    match continuation {
        None => ContinuationAttempt::None,
        Some(ContinuationBinding::External(_))
            if plan.candidates().first().is_some_and(|candidate| {
                operation
                    .provider_session_state(candidate.provider().as_str())
                    .is_some()
            }) =>
        {
            ContinuationAttempt::ReplayAny
        }
        Some(_) => ContinuationAttempt::Native,
    }
}

fn provider_engine_error(error: &ProviderError) -> EngineError {
    if error.kind() == ProviderErrorKind::Cancelled {
        EngineError::Cancelled
    } else {
        EngineError::Provider(ProviderError::new(error.kind(), error.send_state()))
    }
}

fn provider_proved_replay_safe(error: &ProviderError) -> bool {
    error.send_state() == UpstreamSendState::NotSent
        || (error.send_state() != UpstreamSendState::Ambiguous && error.replay_is_safe())
}

fn observe_event_timing(timings: &mut ModelRequestTimings, event: &GatewayEvent, elapsed_ms: u64) {
    timings.first_event_ms.get_or_insert(elapsed_ms);
    match event {
        GatewayEvent::ReasoningDelta(_) => {
            timings.first_reasoning_ms.get_or_insert(elapsed_ms);
            timings.first_token_ms.get_or_insert(elapsed_ms);
        }
        GatewayEvent::TextDelta(_) => {
            timings.first_text_ms.get_or_insert(elapsed_ms);
            timings.first_token_ms.get_or_insert(elapsed_ms);
        }
        GatewayEvent::CalculatedCost(_) | GatewayEvent::ProviderCost(_) => {}
        _ => {}
    }
}

fn elapsed_ms(started_at: SystemTime, observed_at: SystemTime) -> u64 {
    duration_ms(observed_at.duration_since(started_at).unwrap_or_default())
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
