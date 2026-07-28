//! 唯一的账号重试、发送与下游 commit barrier owner。

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::accounting::{CostEstimate, CostSource, Usage};
use crate::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, NativeContinuationScope, PreviousResponseId,
};
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
use crate::operation::{Operation, ProviderSessionState, RetrySafety};
use crate::routing::RoutingPlan;
use futures::future::Fuse;
use futures::{FutureExt, StreamExt, pin_mut, select_biased};
use futures_timer::Delay;

/// Request 级协调器；不会创建或写入 `request_attempts`。
pub struct AttemptCoordinator<S: ?Sized> {
    engine: Arc<GatewayEngine<S>>,
}

#[derive(Debug, Clone)]
enum AccountSelection {
    Scheduled(Option<crate::engine::credential::ProviderAccountId>),
    Diagnostic(crate::engine::credential::ProviderAccountId),
}

impl AccountSelection {
    fn required_account(&self) -> Option<&crate::engine::credential::ProviderAccountId> {
        match self {
            Self::Scheduled(account) => account.as_ref(),
            Self::Diagnostic(account) => Some(account),
        }
    }
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

    /// 账号选择成功后创建唯一请求行，再返回由 Core 完整拥有 retry/commit 的流式会话。
    ///
    /// # Errors
    ///
    /// 账号选择后的持久化、取消或已过 deadline 时返回稳定错误。
    pub async fn start(
        &self,
        request: NewModelRequest,
        operation: Operation,
        plan: RoutingPlan,
        required_account: Option<crate::engine::credential::ProviderAccountId>,
        continuation: Option<ContinuationBinding>,
        cancellation: CancellationToken,
    ) -> Result<ResponseExecutionSession<S>, EngineError> {
        self.start_with_account_selection(
            request,
            operation,
            plan,
            AccountSelection::Scheduled(required_account),
            continuation,
            cancellation,
        )
        .await
    }

    /// 对固定账号执行诊断请求。
    ///
    /// 仅跳过该账号的本地可用性投影；账号租约、并发和请求间隔仍必须满足。
    pub async fn start_diagnostic(
        &self,
        request: NewModelRequest,
        operation: Operation,
        plan: RoutingPlan,
        required_account: crate::engine::credential::ProviderAccountId,
        continuation: Option<ContinuationBinding>,
        cancellation: CancellationToken,
    ) -> Result<ResponseExecutionSession<S>, EngineError> {
        self.start_with_account_selection(
            request,
            operation,
            plan,
            AccountSelection::Diagnostic(required_account),
            continuation,
            cancellation,
        )
        .await
    }

    async fn start_with_account_selection(
        &self,
        request: NewModelRequest,
        operation: Operation,
        plan: RoutingPlan,
        account_selection: AccountSelection,
        continuation: Option<ContinuationBinding>,
        cancellation: CancellationToken,
    ) -> Result<ResponseExecutionSession<S>, EngineError> {
        let request_id = request.id.clone();
        let client_api_key_ref = request.client_api_key_ref.clone();
        let timing_started_at = Instant::now();
        let deadline = request.deadline_at;
        let account_state_owner = continuation
            .as_ref()
            .and_then(ContinuationBinding::pinned)
            .map(ProviderAccountStateOwner::from_continuation);
        let continuation_attempt =
            initial_continuation_attempt(&operation, &plan, continuation.as_ref());
        let image_generation_requested = operation.image_generation_requested();
        let mut session = ResponseExecutionSession {
            engine: Arc::clone(&self.engine),
            request_id,
            client_api_key_ref,
            timing_started_at,
            deadline,
            deadline_timer: Delay::new(
                deadline
                    .duration_since(SystemTime::now())
                    .unwrap_or(Duration::ZERO),
            )
            .fuse(),
            pending_request: Some(request),
            request_persisted: false,
            operation,
            plan,
            account_selection,
            continuation,
            continuation_attempt,
            account_state_owner,
            cancellation,
            attempts: 0,
            excluded_accounts: BTreeSet::new(),
            credential_recovery_attempted_accounts: BTreeSet::new(),
            recovery_account: None,
            current: None,
            send_state_watermark: UpstreamSendState::NotSent,
            downstream_committed_at: None,
            client_status_code: None,
            delivery_pending: false,
            upstream_complete: false,
            finalized: false,
            usage: Usage::new(),
            image_generation_requested,
            cost: CostEstimate::unavailable(),
            timings: ModelRequestTimings::default(),
            client_response_id: None,
            upstream_response_id: None,
            last_account_exhaustion: None,
            provider_attempt_outcomes: Vec::new(),
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

/// API 可逐事件消费的 Core 执行会话。
///
/// API 只能提交下游 delivery 边界；账号重试、断流终结与
/// `model_requests` 写回均留在本类型内。
pub struct ResponseExecutionSession<S: ?Sized> {
    engine: Arc<GatewayEngine<S>>,
    request_id: ModelRequestId,
    client_api_key_ref: crate::policy::ClientApiKeyId,
    timing_started_at: Instant,
    deadline: SystemTime,
    /// 会话级 deadline 计时器；deadline 固定，帧循环内复用而非逐事件新建。
    deadline_timer: Fuse<Delay>,
    pending_request: Option<NewModelRequest>,
    request_persisted: bool,
    operation: Operation,
    plan: RoutingPlan,
    account_selection: AccountSelection,
    continuation: Option<ContinuationBinding>,
    continuation_attempt: ContinuationAttempt,
    account_state_owner: Option<ProviderAccountStateOwner>,
    cancellation: CancellationToken,
    attempts: u32,
    excluded_accounts: BTreeSet<crate::engine::credential::ProviderAccountId>,
    credential_recovery_attempted_accounts: BTreeSet<crate::engine::credential::ProviderAccountId>,
    /// 凭据恢复后的一次性同账号钉选；只约束紧随其后的 replay attempt，
    /// attempt 建立时即被消费，后续可重试错误仍可换号消耗剩余重试预算。
    /// 与 `required_account`（外部指定、贯穿整个请求）语义不同，不可合并。
    recovery_account: Option<crate::engine::credential::ProviderAccountId>,
    current: Option<CurrentAttempt>,
    /// 请求级发送状态水位；跨 attempt 单调不降，终态写回不得低于此档。
    send_state_watermark: UpstreamSendState,
    downstream_committed_at: Option<SystemTime>,
    client_status_code: Option<u16>,
    delivery_pending: bool,
    upstream_complete: bool,
    finalized: bool,
    usage: Usage,
    image_generation_requested: bool,
    cost: CostEstimate,
    timings: ModelRequestTimings,
    client_response_id: Option<String>,
    upstream_response_id: Option<String>,
    last_account_exhaustion: Option<ProviderError>,
    provider_attempt_outcomes: Vec<ProviderAttemptOutcome>,
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
        loop {
            match self.pull().await? {
                PullOutcome::Event(event) => {
                    if self.downstream_committed_at.is_some() {
                        return Ok(Some(CoordinatedEvent::single(
                            event,
                            CommitRequirement::AlreadyCommitted,
                        )));
                    }
                    self.delivery_pending = true;
                    return Ok(Some(CoordinatedEvent::single(
                        event,
                        CommitRequirement::CommitBeforeDelivery,
                    )));
                }
                PullOutcome::AttemptDiscarded => {}
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
        if self.request_persisted {
            self.engine
                .store()
                .record_client_status(&self.request_id, client_status_code)
                .await?;
        }
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

    /// 将已完成响应的账号事实与 Provider 私有状态封装为可丢失的亲和记录。
    ///
    /// Core 不读取 `state` 内容；Provider 将在后续同账号 continuation 时自行解释。
    #[must_use]
    pub fn native_continuation_pin(
        &self,
        state: &ProviderSessionState,
    ) -> Option<NativeContinuationPin> {
        let current = self.current.as_ref()?;
        let provider = current.metadata.provider().clone();
        if state.provider() != provider.as_str() {
            return None;
        }
        let account = current.metadata.provider_account_id()?.clone();
        let response_id = self.upstream_response_id.as_deref()?;
        let previous_response_id = PreviousResponseId::new(response_id.to_owned()).ok()?;
        let upstream_response_id =
            crate::error::SafeUpstreamValue::new(response_id.to_owned()).ok()?;
        Some(
            NativeContinuationPin::new(
                previous_response_id,
                upstream_response_id,
                provider,
                account,
            )
            .with_scope(NativeContinuationScope::Persisted)
            .with_session_state(state.clone()),
        )
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
                let current = self.current.as_mut().ok_or(EngineError::NoActiveAttempt)?;
                poll_stream_item(
                    &mut current.stream,
                    self.cancellation.clone(),
                    self.deadline,
                    &mut self.deadline_timer,
                )
                .await
            };

            match boundary {
                PollBoundary::Cancelled => {
                    self.finish_interruption(EngineError::Cancelled).await?;
                    return Err(EngineError::Cancelled);
                }
                PollBoundary::Deadline => {
                    // 会话 deadline 是网关自身的请求预算，不是上游超时；
                    // 真正的上游超时会作为流错误进入 `handle_stream_error` 记账。
                    // 这里不写 provider 失败，避免长流集中到期误触 provider 熔断。
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
                    for fact in event.canonical_facts() {
                        if let Err(error) = self.observe_response_identity(fact) {
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
        if self.attempts >= self.plan.max_attempts().get() {
            return Err(EngineError::EmptyRoutingPlan);
        }
        let Some(candidate) = self.plan.candidates().first().cloned() else {
            let error = GatewayError::new(
                GatewayErrorKind::NoAvailableProvider,
                "no upstream Provider is available",
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
        // recovery 钉选在此被一次性消费，只绑定本次 replay attempt；
        // 外部 required_account 每次 attempt 都重新生效。
        let pinned_account = match &self.account_selection {
            AccountSelection::Diagnostic(account) => Some(account.clone()),
            AccountSelection::Scheduled(_) => self
                .recovery_account
                .take()
                .or_else(|| self.account_selection.required_account().cloned()),
        };
        let account_context = match &self.account_selection {
            AccountSelection::Diagnostic(account) => AccountAttemptContext::diagnostic(
                self.excluded_accounts.clone(),
                account.clone(),
                self.account_state_owner.clone(),
            ),
            AccountSelection::Scheduled(_) => AccountAttemptContext::new(
                self.excluded_accounts.clone(),
                pinned_account.clone(),
                self.account_state_owner.clone(),
            ),
        }
        .with_credential_recovery_attempted(pinned_account.as_ref().is_some_and(|account| {
            self.credential_recovery_attempted_accounts
                .contains(account)
        }));
        let context = AttemptContext::new(
            RequestAttemptContext::new(self.request_id.clone(), self.client_api_key_ref.clone())
                .with_timing_started_at(self.timing_started_at),
            next_attempt,
            self.deadline,
            self.plan.account_selection_policy(),
            account_context,
            self.continuation.clone(),
            self.cancellation.clone(),
        )
        .with_continuation_attempt(self.continuation_attempt);
        let trigger = if self.attempts == 0 {
            AttemptTrigger::Initial
        } else {
            AttemptTrigger::AccountRetry
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
                // 网关预算到期同样不是候选 Provider 的上游超时，不计入熔断；
                // Provider 自身的握手/传输超时会以 `ProviderErrorKind::Timeout`
                // 错误返回并在下方 `Result` 分支记账。
                self.finish_interruption(EngineError::Deadline).await?;
                return Err(EngineError::Deadline);
            }
            ProviderBoundary::Result(result) => match *result {
                Ok(stream) => stream,
                Err(error) => {
                    if error.kind() == ProviderErrorKind::NoEligibleAccount
                        && let Some(exhaustion) = self.last_account_exhaustion.clone()
                    {
                        let engine_error = provider_engine_error(&exhaustion);
                        self.finish_provider_error(&exhaustion).await?;
                        return Err(engine_error);
                    }
                    if !(matches!(
                        error.kind(),
                        ProviderErrorKind::Unavailable | ProviderErrorKind::NoEligibleAccount
                    ) && error.send_state() == UpstreamSendState::NotSent)
                    {
                        self.record_provider_failure(candidate.provider().clone(), error.kind());
                    }
                    let engine_error = provider_engine_error(&error);
                    self.finish_provider_error(&error).await?;
                    return Err(engine_error);
                }
            },
        };
        if !stream.metadata().confirms(&candidate) {
            self.record_provider_failure(candidate.provider().clone(), ProviderErrorKind::Protocol);
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
        if pinned_account
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
            && !metadata
                .provider_account_id()
                .is_some_and(|account| pin.matches(metadata.provider(), account))
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
                account.clone(),
            ));
        }
        let attempt_record = AttemptRecord {
            request_id: self.request_id.clone(),
            attempt_count: next_attempt,
            trigger,
            provider_kind: metadata.provider().clone(),
            provider_account_id: metadata.provider_account_id().cloned(),
            provider_account_ref: metadata.provider_account_id().cloned(),
            upstream_model_id: metadata.upstream_model().clone(),
            upstream_transport: metadata.transport().as_str().to_owned(),
            http_version: None,
        };
        if self.request_persisted {
            self.engine.store().record_attempt(attempt_record).await?;
        } else {
            let request = self
                .pending_request
                .take()
                .ok_or(EngineError::InvalidDeliveryState)?;
            self.engine
                .store()
                .create_model_request_with_attempt(request, attempt_record)
                .await?;
            self.request_persisted = true;
        }
        self.attempts = next_attempt.get();
        self.current = Some(CurrentAttempt {
            stream,
            metadata,
            trigger,
            index: next_attempt,
            started_at: SystemTime::now(),
            send_observed: false,
            response_observation: None,
        });
        Ok(())
    }

    async fn observe_event(&mut self, event: &GatewayEvent) -> Result<(), EngineError> {
        let elapsed = elapsed_ms(self.timing_started_at);
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
        let current = self.current.as_mut().ok_or(EngineError::NoActiveAttempt)?;
        if !current.send_observed {
            self.engine
                .store()
                .mark_send_state(&self.request_id, UpstreamSendState::Sent)
                .await?;
            current.send_observed = true;
            self.send_state_watermark = UpstreamSendState::Sent;
        }
        Ok(())
    }

    async fn observe_wire_event(&mut self) -> Result<(), EngineError> {
        let current = self.current.as_mut().ok_or(EngineError::NoActiveAttempt)?;
        if !current.send_observed {
            self.engine
                .store()
                .mark_send_state(&self.request_id, UpstreamSendState::Sent)
                .await?;
            current.send_observed = true;
            self.send_state_watermark = UpstreamSendState::Sent;
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
        if let Some(value) = observed.first_reasoning_ms {
            self.timings.first_reasoning_ms = Some(value);
        }
        if let Some(value) = observed.first_text_ms {
            self.timings.first_text_ms = Some(value);
        }
        if let Some(value) = observed.first_token_ms {
            self.timings.first_token_ms = Some(value);
        }
        if let Some(value) = observed.provider_processing_ms {
            self.timings.provider_processing_ms = Some(value);
        }
        current.response_observation = Some(observation);
        Ok(())
    }

    fn observe_response_identity(&mut self, event: &GatewayEvent) -> Result<(), ProviderError> {
        let metadata = match event {
            GatewayEvent::Started(metadata) | GatewayEvent::Completed(metadata) => metadata,
            _ => return Ok(()),
        };
        let response_id = crate::error::SafeUpstreamValue::new(metadata.response_id().to_owned())
            .map_err(|_| {
            ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
        })?;
        let response_id = response_id.as_str();
        if response_id.is_empty() {
            return Ok(());
        }
        if self
            .upstream_response_id
            .as_deref()
            .is_some_and(|expected| expected != response_id)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::Protocol,
                UpstreamSendState::Sent,
            ));
        }
        let response_id = response_id.to_owned();
        self.client_response_id = Some(response_id.clone());
        self.upstream_response_id = Some(response_id);
        Ok(())
    }

    /// 返回 `true` 表示调用方必须丢弃本 attempt 已收集的未提交事件。
    async fn handle_stream_error(&mut self, error: ProviderError) -> Result<bool, EngineError> {
        let current = self.current.take().ok_or(EngineError::NoActiveAttempt)?;
        if matches!(
            error.kind(),
            ProviderErrorKind::RateLimited | ProviderErrorKind::QuotaExhausted
        ) {
            self.last_account_exhaustion = Some(error.clone());
        }
        self.record_provider_failure(current.metadata.provider().clone(), error.kind());
        // attempt_send_state 是本 attempt 自身的发送事实，驱动重试门；
        // 持久化与终态用请求级水位，二者不可混用（水位会把早先 attempt 的
        // sent 传染给本 attempt，从而错误放行/拦截重试）。
        let attempt_send_state = if current.send_observed {
            UpstreamSendState::Sent
        } else {
            error.send_state()
        };
        let send_state = self.raise_send_watermark(attempt_send_state);
        self.engine
            .store()
            .mark_send_state(&self.request_id, send_state)
            .await?;
        let provider_proved_replay_safe = provider_proved_replay_safe(&error);
        let continuation_retry = self.prepare_continuation_retry(
            &current,
            &error,
            attempt_send_state,
            provider_proved_replay_safe,
        );
        let ordinary_retry = self.account_selection.required_account().is_none()
            && self.continuation_attempt == ContinuationAttempt::None
            && self.downstream_committed_at.is_none()
            && !self.delivery_pending
            && attempt_send_state != UpstreamSendState::Ambiguous
            && (self.operation.retry_safety() == RetrySafety::Idempotent
                || provider_proved_replay_safe)
            && self.attempts < self.plan.max_attempts().get();
        let same_account_retry = error.retries_same_account()
            && provider_proved_replay_safe
            && self.downstream_committed_at.is_none()
            && !self.delivery_pending
            && attempt_send_state != UpstreamSendState::Ambiguous
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
                    // 只钉住紧随其后的 replay attempt；replay 再遇可重试错误时，
                    // ordinary/continuation 重试门不受影响，仍可换号。
                    self.recovery_account = Some(account.clone());
                }
            } else if !continuation_retry
                && let Some(account) = current.metadata.provider_account_id()
            {
                self.excluded_accounts.insert(account.clone());
            }
            self.engine
                .store()
                .record_intermediate_failure(IntermediateFailure {
                    request_id: self.request_id.clone(),
                    attempt_index: current.index,
                    trigger: current.trigger,
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
        if self.account_selection.required_account().is_some()
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
        self.timings.provider_processing_ms = None;
        self.upstream_complete = false;
    }

    async fn finish_success(&mut self) -> Result<(), EngineError> {
        if self.finalized {
            return Ok(());
        }
        let completed_at = SystemTime::now();
        self.timings.latency_ms = Some(elapsed_ms(self.timing_started_at));
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
            .and_then(ProviderResponseObservation::status_code);
        let (upstream_transport, http_version, websocket_pool) =
            self.current_transport_observation();
        let provider_metadata_json = self.current_provider_metadata_json();
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
                websocket_pool,
                provider_metadata_json,
                error: None,
                provider_error_code: None,
                retry_after_ms: None,
                usage: self.usage.clone(),
                image_generation_succeeded: self.image_generation_succeeded(),
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
        let send_state = self.raise_send_watermark(send_state);
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
        let send_state = self.raise_send_watermark(send_state);
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
        if !self.request_persisted {
            self.finalized = true;
            return Ok(());
        }
        let completed_at = SystemTime::now();
        self.timings.latency_ms = Some(elapsed_ms(self.timing_started_at));
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
        let (upstream_transport, http_version, websocket_pool) =
            self.current_transport_observation();
        let provider_metadata_json = self.current_provider_metadata_json();
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
                websocket_pool,
                provider_metadata_json,
                error: Some(finalization.error),
                provider_error_code: finalization.provider_error_code,
                retry_after_ms: finalization.retry_after_ms,
                usage: self.usage.clone(),
                image_generation_succeeded: self.image_generation_succeeded(),
                cost: self.cost.clone(),
                timings: self.timings.clone(),
                completed_at,
            })
            .await?;
        self.finalized = true;
        Ok(())
    }

    fn current_transport_observation(&self) -> (Option<String>, Option<String>, Option<String>) {
        let Some(current) = self.current.as_ref() else {
            return (None, None, None);
        };
        let Some(observation) = current.response_observation.as_ref() else {
            return (None, None, None);
        };
        (
            Some(observation.transport().as_str().to_owned()),
            observation
                .http_version()
                .map(|version| version.as_str().to_owned()),
            observation
                .websocket_pool()
                .map(|kind| kind.as_str().to_owned()),
        )
    }

    fn current_provider_metadata_json(&self) -> Option<String> {
        self.current
            .as_ref()
            .and_then(|current| current.response_observation.as_ref())
            .and_then(ProviderResponseObservation::provider_metadata)
            .map(|metadata| metadata.as_json().to_owned())
    }

    fn image_generation_succeeded(&self) -> Option<bool> {
        self.image_generation_requested
            .then(|| self.usage.image_output_tokens.unwrap_or_default() > 0)
    }

    fn current_send_state(&self) -> UpstreamSendState {
        let observed = if self
            .current
            .as_ref()
            .is_some_and(|current| current.send_observed)
        {
            UpstreamSendState::Sent
        } else {
            UpstreamSendState::NotSent
        };
        escalate_send_state(self.send_state_watermark, observed)
    }

    /// 抬升并返回请求级发送水位；attempt 间切换（`current` 被取走）后，
    /// 后续终态沿用已达到的最高档，不会把已落库的 `sent` 写回 `not_sent`。
    fn raise_send_watermark(&mut self, observed: UpstreamSendState) -> UpstreamSendState {
        self.send_state_watermark = escalate_send_state(self.send_state_watermark, observed);
        self.send_state_watermark
    }

    fn record_current_provider_success(&mut self) {
        let provider_kind = self
            .current
            .as_ref()
            .map(|current| current.metadata.provider().clone());
        if let Some(provider_kind) = provider_kind {
            self.provider_attempt_outcomes
                .push(ProviderAttemptOutcome::Succeeded { provider_kind });
        }
    }

    fn record_provider_failure(
        &mut self,
        provider_kind: crate::routing::ProviderKind,
        error_kind: ProviderErrorKind,
    ) {
        self.provider_attempt_outcomes
            .push(ProviderAttemptOutcome::Failed {
                provider_kind,
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
    mut deadline_timer: &mut Fuse<Delay>,
) -> PollBoundary {
    if SystemTime::now() >= deadline {
        return PollBoundary::Deadline;
    }
    let next = stream.next().fuse();
    let cancelled = cancellation.cancelled().fuse();
    pin_mut!(next, cancelled);
    select_biased! {
        _ = cancelled => PollBoundary::Cancelled,
        _ = deadline_timer => PollBoundary::Deadline,
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
        EngineError::Provider(error.clone())
    }
}

fn provider_proved_replay_safe(error: &ProviderError) -> bool {
    error.send_state() == UpstreamSendState::NotSent
        || (error.send_state() != UpstreamSendState::Ambiguous && error.replay_is_safe())
}

/// 发送状态合并档位：`Sent` > `Ambiguous` > `NotSent`。
/// 只要任一 attempt 达到过高档，请求整体就不允许回落到低档。
const fn escalate_send_state(a: UpstreamSendState, b: UpstreamSendState) -> UpstreamSendState {
    match (a, b) {
        (UpstreamSendState::Sent, _) | (_, UpstreamSendState::Sent) => UpstreamSendState::Sent,
        (UpstreamSendState::Ambiguous, _) | (_, UpstreamSendState::Ambiguous) => {
            UpstreamSendState::Ambiguous
        }
        (UpstreamSendState::NotSent, UpstreamSendState::NotSent) => UpstreamSendState::NotSent,
    }
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
        // `response.output_item.added` 会先投影一个空参数的 tool delta；它只是结构帧，
        // 不能抢在真实工具参数之前成为首个可消费 token。
        GatewayEvent::ToolCallDelta(delta) if !delta.arguments_delta.is_empty() => {
            timings.first_token_ms.get_or_insert(elapsed_ms);
        }
        GatewayEvent::CalculatedCost(_) | GatewayEvent::ProviderCost(_) => {}
        _ => {}
    }
}

fn elapsed_ms(started_at: Instant) -> u64 {
    duration_ms(started_at.elapsed())
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
