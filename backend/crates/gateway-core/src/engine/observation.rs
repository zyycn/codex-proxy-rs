//! 单次响应事实与请求终态扩展派发；重试丢弃时统一清理

use std::{
    collections::BTreeMap,
    num::NonZeroU32,
    sync::{
        Arc, RwLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime},
};

use super::{
    ExecutionOutcome, ModelRequestFinalization, ModelRequestId, ModelRequestTimings,
    extensions::ExtensionCallScope,
};
use crate::{
    account::ProviderAccountId,
    error::{GatewayError, GatewayErrorKind},
    event::{GatewayEvent, ProtocolWireEvent, ProviderResponseObservation},
    identity::ProviderKind,
    metering::{CostEstimate, CostSource, Usage},
    operation::OperationKind,
    policy::ClientApiKeyId,
    routing::{AccountGroupId, ConfigRevision, PublicModelId},
    runtime::extensions::{ExtensionSetId, ExtensionSetReference},
    upstream::UpstreamSendState,
};

/// 一条 WebSocket 响应事件所属的实际上游 attempt 身份
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSocketResponseAttempt {
    provider: ProviderKind,
    account_id: ProviderAccountId,
    attempt_index: NonZeroU32,
}

impl WebSocketResponseAttempt {
    /// 固定一条响应事件所属的实际上游 attempt
    #[must_use]
    pub const fn new(
        provider: ProviderKind,
        account_id: ProviderAccountId,
        attempt_index: NonZeroU32,
    ) -> Self {
        Self {
            provider,
            account_id,
            attempt_index,
        }
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn attempt_index(&self) -> NonZeroU32 {
        self.attempt_index
    }
}

/// 请求观察使用的冻结 Client Key 与账号组范围
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestObservationScope {
    client_key_id: ClientApiKeyId,
    account_group_ids: Arc<[AccountGroupId]>,
}

impl RequestObservationScope {
    #[must_use]
    pub fn new(client_key_id: ClientApiKeyId, mut account_group_ids: Vec<AccountGroupId>) -> Self {
        account_group_ids.sort();
        account_group_ids.dedup();
        Self {
            client_key_id,
            account_group_ids: account_group_ids.into(),
        }
    }

    #[must_use]
    pub const fn client_key_id(&self) -> &ClientApiKeyId {
        &self.client_key_id
    }

    #[must_use]
    pub fn account_group_ids(&self) -> &[AccountGroupId] {
        &self.account_group_ids
    }
}

/// 实际上游 WebSocket 响应事件的只读观察
///
/// Key 与账号组只用于 Runtime 匹配冻结绑定，不得进入插件 wire；`wire` 保留策略加工前
/// 的 Provider 原始事实，由 Runtime 再按正文读取授权投影
#[derive(Clone, PartialEq)]
pub struct WebSocketResponseObservation {
    event_id: String,
    request_id: ModelRequestId,
    config_revision: ConfigRevision,
    client_scope: RequestObservationScope,
    extension_scope: ExtensionCallScope,
    operation: OperationKind,
    requested_model: Option<PublicModelId>,
    attempt: WebSocketResponseAttempt,
    sequence: u64,
    wire: ProtocolWireEvent,
}

impl WebSocketResponseObservation {
    /// 创建一个已附加冻结 Key/组范围、尚未附加公开模型范围的实际上游事件
    #[must_use]
    pub fn new(
        request_id: ModelRequestId,
        config_revision: ConfigRevision,
        client_scope: RequestObservationScope,
        operation: OperationKind,
        attempt: WebSocketResponseAttempt,
        sequence: u64,
        wire: ProtocolWireEvent,
    ) -> Self {
        Self {
            event_id: format!(
                "{}:websocket:{}:{sequence}",
                request_id.as_str(),
                attempt.attempt_index().get()
            ),
            request_id,
            config_revision,
            client_scope,
            extension_scope: ExtensionCallScope::default(),
            operation,
            requested_model: None,
            attempt,
            sequence,
            wire,
        }
    }

    #[must_use]
    pub fn with_requested_model(mut self, model: PublicModelId) -> Self {
        self.requested_model = Some(model);
        self
    }

    #[must_use]
    pub fn with_extension_scope(mut self, extension_scope: ExtensionCallScope) -> Self {
        self.extension_scope = extension_scope;
        self
    }

    #[must_use]
    pub fn suppresses_plugin(&self, instance_id: &str) -> bool {
        self.extension_scope.contains(instance_id)
    }

    #[must_use]
    pub const fn extension_scope(&self) -> &ExtensionCallScope {
        &self.extension_scope
    }

    #[must_use]
    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    #[must_use]
    pub const fn request_id(&self) -> &ModelRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn config_revision(&self) -> ConfigRevision {
        self.config_revision
    }

    #[must_use]
    pub const fn client_key_id(&self) -> &ClientApiKeyId {
        self.client_scope.client_key_id()
    }

    #[must_use]
    pub fn account_group_ids(&self) -> &[AccountGroupId] {
        self.client_scope.account_group_ids()
    }

    #[must_use]
    pub const fn operation(&self) -> OperationKind {
        self.operation
    }

    #[must_use]
    pub const fn requested_model(&self) -> Option<&PublicModelId> {
        self.requested_model.as_ref()
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        self.attempt.provider()
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        self.attempt.account_id()
    }

    #[must_use]
    pub const fn attempt_index(&self) -> NonZeroU32 {
        self.attempt.attempt_index()
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn wire(&self) -> &ProtocolWireEvent {
        &self.wire
    }
}

/// 插件可观察的请求终态；拒绝表示请求没有进入 Provider 执行
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestObservationOutcome {
    Succeeded,
    Failed,
    Rejected,
    Cancelled,
    Incomplete,
}

/// Core 在业务结果确定后产生的一次最终观察，不包含原始正文或凭据
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestObservation {
    event_id: String,
    request_id: ModelRequestId,
    config_revision: ConfigRevision,
    client_scope: RequestObservationScope,
    extension_scope: ExtensionCallScope,
    operation: OperationKind,
    requested_model: Option<PublicModelId>,
    provider: Option<ProviderKind>,
    account_id: Option<ProviderAccountId>,
    upstream_model: Option<crate::routing::UpstreamModelId>,
    response_model: Option<String>,
    service_tier: Option<String>,
    outcome: RequestObservationOutcome,
    send_state: UpstreamSendState,
    attempt_count: u32,
    client_status_code: Option<u16>,
    upstream_status_code: Option<u16>,
    error_kind: Option<GatewayErrorKind>,
    retry_after_ms: Option<u64>,
    usage: Usage,
    cost: CostEstimate,
    timings: ModelRequestTimings,
    completed_at: SystemTime,
}

impl RequestObservation {
    /// 创建一个带冻结 Key/组范围、不带 Provider、模型、用量或错误详情的最终事实
    #[must_use]
    pub fn new(
        request_id: ModelRequestId,
        config_revision: ConfigRevision,
        client_scope: RequestObservationScope,
        operation: OperationKind,
        outcome: RequestObservationOutcome,
        send_state: UpstreamSendState,
        completed_at: SystemTime,
    ) -> Self {
        Self {
            event_id: format!("{}:terminal", request_id.as_str()),
            request_id,
            config_revision,
            client_scope,
            extension_scope: ExtensionCallScope::default(),
            operation,
            requested_model: None,
            provider: None,
            account_id: None,
            upstream_model: None,
            response_model: None,
            service_tier: None,
            outcome,
            send_state,
            attempt_count: 0,
            client_status_code: None,
            upstream_status_code: None,
            error_kind: None,
            retry_after_ms: None,
            usage: Usage::new(),
            cost: CostEstimate::unavailable(),
            timings: ModelRequestTimings::default(),
            completed_at,
        }
    }

    #[must_use]
    pub fn with_requested_model(mut self, model: PublicModelId) -> Self {
        self.requested_model = Some(model);
        self
    }

    #[must_use]
    pub fn with_extension_scope(mut self, extension_scope: ExtensionCallScope) -> Self {
        self.extension_scope = extension_scope;
        self
    }

    #[must_use]
    pub fn suppresses_plugin(&self, instance_id: &str) -> bool {
        self.extension_scope.contains(instance_id)
    }

    #[must_use]
    pub const fn extension_scope(&self) -> &ExtensionCallScope {
        &self.extension_scope
    }

    #[must_use]
    pub fn with_provider(mut self, provider: ProviderKind) -> Self {
        self.provider = Some(provider);
        self
    }

    #[must_use]
    pub fn account_id(&self) -> Option<&ProviderAccountId> {
        self.account_id.as_ref()
    }

    #[must_use]
    pub fn upstream_model(&self) -> Option<&crate::routing::UpstreamModelId> {
        self.upstream_model.as_ref()
    }

    #[must_use]
    pub fn response_model(&self) -> Option<&str> {
        self.response_model.as_deref()
    }

    #[must_use]
    pub fn service_tier(&self) -> Option<&str> {
        self.service_tier.as_deref()
    }

    #[must_use]
    pub const fn with_attempt_count(mut self, attempt_count: u32) -> Self {
        self.attempt_count = attempt_count;
        self
    }

    #[must_use]
    pub const fn with_client_status_code(mut self, client_status_code: u16) -> Self {
        self.client_status_code = Some(client_status_code);
        self
    }

    #[must_use]
    pub const fn with_upstream_status_code(mut self, upstream_status_code: u16) -> Self {
        self.upstream_status_code = Some(upstream_status_code);
        self
    }

    #[must_use]
    pub const fn with_error_kind(mut self, error_kind: GatewayErrorKind) -> Self {
        self.error_kind = Some(error_kind);
        self
    }

    #[must_use]
    pub const fn with_retry_after_ms(mut self, retry_after_ms: u64) -> Self {
        self.retry_after_ms = Some(retry_after_ms);
        self
    }

    #[must_use]
    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = usage;
        self
    }

    #[must_use]
    pub fn with_cost(mut self, cost: CostEstimate) -> Self {
        self.cost = cost;
        self
    }

    #[must_use]
    pub fn with_timings(mut self, timings: ModelRequestTimings) -> Self {
        self.timings = timings;
        self
    }

    #[must_use]
    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    #[must_use]
    pub const fn request_id(&self) -> &ModelRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn config_revision(&self) -> ConfigRevision {
        self.config_revision
    }

    #[must_use]
    pub const fn client_key_id(&self) -> &ClientApiKeyId {
        self.client_scope.client_key_id()
    }

    #[must_use]
    pub fn account_group_ids(&self) -> &[AccountGroupId] {
        self.client_scope.account_group_ids()
    }

    #[must_use]
    pub const fn operation(&self) -> OperationKind {
        self.operation
    }

    #[must_use]
    pub const fn requested_model(&self) -> Option<&PublicModelId> {
        self.requested_model.as_ref()
    }

    #[must_use]
    pub const fn provider(&self) -> Option<&ProviderKind> {
        self.provider.as_ref()
    }

    #[must_use]
    pub const fn outcome(&self) -> RequestObservationOutcome {
        self.outcome
    }

    #[must_use]
    pub const fn send_state(&self) -> UpstreamSendState {
        self.send_state
    }

    #[must_use]
    pub const fn attempt_count(&self) -> u32 {
        self.attempt_count
    }

    #[must_use]
    pub const fn client_status_code(&self) -> Option<u16> {
        self.client_status_code
    }

    #[must_use]
    pub const fn upstream_status_code(&self) -> Option<u16> {
        self.upstream_status_code
    }

    #[must_use]
    pub const fn error_kind(&self) -> Option<GatewayErrorKind> {
        self.error_kind
    }

    #[must_use]
    pub const fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }

    #[must_use]
    pub const fn usage(&self) -> &Usage {
        &self.usage
    }

    #[must_use]
    pub const fn cost(&self) -> &CostEstimate {
        &self.cost
    }

    #[must_use]
    pub const fn timings(&self) -> &ModelRequestTimings {
        &self.timings
    }

    #[must_use]
    pub const fn completed_at(&self) -> SystemTime {
        self.completed_at
    }
}

/// 一个发布代次的不可变请求观察计划；实现必须自行保证派发有界且不阻塞业务结果
pub trait RequestObserverPlan: Send + Sync {
    /// 旁路观察一条实际上游 WebSocket 事件；未启用观察时，默认实现不额外处理事件
    fn dispatch_websocket_response(
        &self,
        _generation: ExtensionSetReference,
        _observation: WebSocketResponseObservation,
    ) {
    }

    /// `generation` 必须由后台工作持有到本次派发结束，避免旧代次提前排空
    fn dispatch(&self, generation: ExtensionSetReference, observation: RequestObservation);
}

/// 观察计划注册冲突；同一个集合 ID 不能被静默替换
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("request observer generation is already registered")]
pub struct RequestObserverRegistrationError;

/// 按发布集合解析计划的非拥有索引；旧代次由发布视图和在途派发保活
#[derive(Clone, Default)]
pub struct RequestObserverExtensionIndex {
    sets: Arc<RwLock<BTreeMap<ExtensionSetId, Weak<dyn RequestObserverPlan>>>>,
}

impl RequestObserverExtensionIndex {
    /// 注册候选代次；返回值必须由候选集合强持有
    pub fn register(
        &self,
        id: ExtensionSetId,
        plan: Arc<dyn RequestObserverPlan>,
    ) -> Result<Arc<dyn RequestObserverPlan>, RequestObserverRegistrationError> {
        let mut sets = self
            .sets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.retain(|_, plan| plan.strong_count() > 0);
        if sets.contains_key(&id) {
            return Err(RequestObserverRegistrationError);
        }
        sets.insert(id, Arc::downgrade(&plan));
        Ok(plan)
    }

    /// 只解析请求已经冻结的集合；缺少计划表示该代次没有观察绑定
    #[must_use]
    pub fn resolve(
        &self,
        generation: &ExtensionSetReference,
    ) -> Option<Arc<dyn RequestObserverPlan>> {
        self.sets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(generation.id())
            .and_then(Weak::upgrade)
    }
}

/// 同一请求的所有终结分支共享这个一次性派发状态
#[derive(Clone)]
pub(super) struct RequestObservationDispatch {
    state: Arc<RequestObservationDispatchState>,
}

struct RequestObservationDispatchState {
    plan: Arc<dyn RequestObserverPlan>,
    generation: ExtensionSetReference,
    dispatched: AtomicBool,
    context: FrozenRequestObservationContext,
}

pub(super) struct FrozenRequestObservationContext {
    request_id: ModelRequestId,
    config_revision: ConfigRevision,
    client_scope: RequestObservationScope,
    operation: OperationKind,
    requested_model: Option<PublicModelId>,
    extension_scope: ExtensionCallScope,
}

impl FrozenRequestObservationContext {
    #[must_use]
    pub(super) fn new(
        request_id: ModelRequestId,
        config_revision: ConfigRevision,
        client_scope: RequestObservationScope,
        operation: OperationKind,
        requested_model: Option<PublicModelId>,
        extension_scope: ExtensionCallScope,
    ) -> Self {
        Self {
            request_id,
            config_revision,
            client_scope,
            operation,
            requested_model,
            extension_scope,
        }
    }
}

impl RequestObservationDispatch {
    #[must_use]
    pub(super) fn new(
        plan: Arc<dyn RequestObserverPlan>,
        generation: ExtensionSetReference,
        context: FrozenRequestObservationContext,
    ) -> Self {
        Self {
            state: Arc::new(RequestObservationDispatchState {
                plan,
                generation,
                dispatched: AtomicBool::new(false),
                context,
            }),
        }
    }

    pub(super) fn reject(&self, error: &GatewayError) {
        self.dispatch(RequestObservation {
            event_id: self.event_id(),
            request_id: self.state.context.request_id.clone(),
            config_revision: self.state.context.config_revision,
            client_scope: self.state.context.client_scope.clone(),
            extension_scope: self.state.context.extension_scope.clone(),
            operation: self.state.context.operation,
            requested_model: self.state.context.requested_model.clone(),
            provider: None,
            outcome: RequestObservationOutcome::Rejected,
            account_id: None,
            upstream_model: None,
            response_model: None,
            service_tier: None,
            send_state: UpstreamSendState::NotSent,
            attempt_count: 0,
            client_status_code: None,
            upstream_status_code: None,
            error_kind: Some(error.kind()),
            retry_after_ms: error.retry_after().map(duration_millis),
            usage: Usage::new(),
            cost: CostEstimate::unavailable(),
            timings: ModelRequestTimings::default(),
            completed_at: SystemTime::now(),
        });
    }

    pub(super) fn websocket_response(
        &self,
        attempt: WebSocketResponseAttempt,
        sequence: u64,
        wire: ProtocolWireEvent,
    ) {
        let mut observation = WebSocketResponseObservation::new(
            self.state.context.request_id.clone(),
            self.state.context.config_revision,
            self.state.context.client_scope.clone(),
            self.state.context.operation,
            attempt,
            sequence,
            wire,
        )
        .with_extension_scope(self.state.context.extension_scope.clone());
        if let Some(model) = self.state.context.requested_model.clone() {
            observation = observation.with_requested_model(model);
        }
        self.state
            .plan
            .dispatch_websocket_response(self.state.generation.clone(), observation);
    }

    #[must_use]
    pub(super) fn finalization(
        &self,
        finalization: &ModelRequestFinalization,
        provider: Option<ProviderKind>,
        attempt: Option<&super::provider::ProviderCallMetadata>,
    ) -> RequestObservation {
        let policy_rejected = finalization.send_state == UpstreamSendState::NotSent
            && finalization
                .error
                .as_ref()
                .is_some_and(|error| error.kind() == GatewayErrorKind::PolicyDenied);
        let outcome = match finalization.outcome {
            ExecutionOutcome::Succeeded => RequestObservationOutcome::Succeeded,
            ExecutionOutcome::Failed | ExecutionOutcome::Running if policy_rejected => {
                RequestObservationOutcome::Rejected
            }
            ExecutionOutcome::Failed | ExecutionOutcome::Running => {
                RequestObservationOutcome::Failed
            }
            ExecutionOutcome::Cancelled => RequestObservationOutcome::Cancelled,
            ExecutionOutcome::Incomplete => RequestObservationOutcome::Incomplete,
        };
        RequestObservation {
            event_id: self.event_id(),
            request_id: self.state.context.request_id.clone(),
            config_revision: self.state.context.config_revision,
            client_scope: self.state.context.client_scope.clone(),
            extension_scope: self.state.context.extension_scope.clone(),
            operation: self.state.context.operation,
            requested_model: self.state.context.requested_model.clone(),
            provider,
            account_id: attempt.map(|attempt| attempt.provider_account_id().clone()),
            upstream_model: attempt.and_then(|attempt| attempt.upstream_model().cloned()),
            response_model: finalization.upstream_response_model.clone(),
            service_tier: finalization.service_tier.clone(),
            outcome,
            send_state: finalization.send_state,
            attempt_count: finalization.attempt_count,
            client_status_code: finalization.client_status_code,
            upstream_status_code: finalization.upstream_status_code,
            error_kind: finalization.error.as_ref().map(GatewayError::kind),
            retry_after_ms: finalization.retry_after_ms,
            usage: finalization.usage.clone(),
            cost: finalization.cost.clone(),
            timings: finalization.timings.clone(),
            completed_at: finalization.completed_at,
        }
    }

    pub(super) fn dispatch(&self, observation: RequestObservation) {
        if self
            .state
            .dispatched
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.state
            .plan
            .dispatch(self.state.generation.clone(), observation);
    }

    fn event_id(&self) -> String {
        format!("{}:terminal", self.state.context.request_id.as_str())
    }
}

fn duration_millis(duration: std::time::Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

pub(super) struct ResponseObservation {
    pub(super) timing_started_at: Instant,
    pub(super) usage: Usage,
    pub(super) cost: CostEstimate,
    pub(super) timings: ModelRequestTimings,
    pub(super) client_response_id: Option<String>,
    pub(super) upstream_response_id: Option<String>,
}

impl ResponseObservation {
    pub(super) fn new(timing_started_at: Instant) -> Self {
        Self {
            timing_started_at,
            usage: Usage::new(),
            cost: CostEstimate::unavailable(),
            timings: ModelRequestTimings::default(),
            client_response_id: None,
            upstream_response_id: None,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.timing_started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(super) fn finish(&mut self) {
        self.timings.latency_ms = Some(self.elapsed_ms());
    }

    pub(super) fn observe_event(&mut self, event: &GatewayEvent) {
        let elapsed = self.elapsed_ms();
        observe_event_timing(&mut self.timings, event, elapsed);
        if let GatewayEvent::Usage(observed) = event {
            self.usage.merge(observed);
        }
        if let GatewayEvent::CalculatedCost(observed) = event
            && self.cost.source() != CostSource::ProviderReported
        {
            self.cost = observed.clone().into_estimate();
        }
        if let GatewayEvent::ProviderCost(observed) = event {
            self.cost = observed.into_estimate();
        }
    }

    pub(super) fn observe_identity(&mut self, event: &GatewayEvent) {
        let metadata = match event {
            GatewayEvent::Started(metadata) | GatewayEvent::Completed(metadata) => metadata,
            _ => return,
        };
        let response_id = metadata.response_id().to_owned();
        self.client_response_id = Some(response_id.clone());
        self.upstream_response_id = Some(response_id);
    }

    pub(super) fn reset_for_attempt(&mut self) {
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
    }

    pub(super) fn observe_response(&mut self, observation: &ProviderResponseObservation) {
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
        // 不能抢在真实工具参数之前成为首个可消费 token
        GatewayEvent::ToolCallDelta(delta) if !delta.arguments_delta.is_empty() => {
            timings.first_token_ms.get_or_insert(elapsed_ms);
        }
        GatewayEvent::CalculatedCost(_) | GatewayEvent::ProviderCost(_) => {}
        _ => {}
    }
}
