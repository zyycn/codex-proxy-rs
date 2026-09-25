//! 请求级插件路由与账号调度计划；Core 保留目标、资格和租约复核。

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    num::NonZeroU32,
    sync::{Arc, RwLock, Weak},
    time::SystemTime,
};

use futures::future::BoxFuture;

use crate::{
    account::{
        AccountCandidate, AccountSelection, AccountSelectionContext, AccountSelector,
        ProviderAccountId,
    },
    engine::{extensions::ExtensionCallScope, nested::ExecutionEffects},
    identity::ProviderKind,
    operation::Operation,
    policy::ClientApiKeyId,
    routing::{AccountGroupId, PublicModelId},
    runtime::extensions::{ExtensionSetId, ExtensionSetReference},
};

/// 模型路由插件的一次输入；正文仍保存在 `operation` 中，由 Runtime 按权限投影。
#[derive(Clone)]
pub struct ModelRouteInput {
    request_id: super::ModelRequestId,
    client_key_id: ClientApiKeyId,
    account_group_ids: Arc<[AccountGroupId]>,
    extension_scope: ExtensionCallScope,
    operation: Operation,
    requested_model: PublicModelId,
    available_providers: BTreeSet<ProviderKind>,
    execution_effects: Arc<ExecutionEffects>,
}

impl ModelRouteInput {
    #[must_use]
    pub const fn request_id(&self) -> &super::ModelRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn client_key_id(&self) -> &ClientApiKeyId {
        &self.client_key_id
    }

    #[must_use]
    pub fn account_group_ids(&self) -> &[AccountGroupId] {
        &self.account_group_ids
    }

    #[must_use]
    pub fn suppresses_plugin(&self, instance_id: &str) -> bool {
        self.extension_scope.contains(instance_id)
    }

    #[must_use]
    pub const fn operation(&self) -> &Operation {
        &self.operation
    }

    #[must_use]
    pub const fn requested_model(&self) -> &PublicModelId {
        &self.requested_model
    }

    #[must_use]
    pub const fn available_providers(&self) -> &BTreeSet<ProviderKind> {
        &self.available_providers
    }

    /// 只供受信 Runtime 记录本次策略调用已经产生的外部副作用，不进入插件 wire。
    #[must_use]
    pub fn execution_effects(&self) -> Arc<ExecutionEffects> {
        Arc::clone(&self.execution_effects)
    }
}

impl fmt::Debug for ModelRouteInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelRouteInput")
            .field("request_id", &self.request_id)
            .field("client_key_id", &self.client_key_id)
            .field("account_group_ids", &self.account_group_ids)
            .field("operation", &self.operation.kind())
            .field("requested_model", &self.requested_model)
            .field("available_providers", &self.available_providers)
            .finish()
    }
}

/// 模型路由结果；Runtime 区分未处理、明确拒绝和调用故障。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRouteDecision {
    Unhandled,
    Route {
        provider: Option<ProviderKind>,
        model: Option<PublicModelId>,
    },
    Reject,
}

/// 账号调度插件可见的单个候选事实，不包含凭据或账号资料。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountScheduleCandidate {
    account_id: ProviderAccountId,
    weight: u16,
    in_flight: u32,
    maximum_concurrency: u32,
    last_started_at: Option<SystemTime>,
    quota_reset_at: Option<SystemTime>,
    quota_remaining_rank: Option<u64>,
    failure_rate_basis_points: Option<u16>,
    first_output_latency_ms: Option<u64>,
}

impl AccountScheduleCandidate {
    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn weight(&self) -> u16 {
        self.weight
    }

    #[must_use]
    pub const fn in_flight(&self) -> u32 {
        self.in_flight
    }

    #[must_use]
    pub const fn maximum_concurrency(&self) -> u32 {
        self.maximum_concurrency
    }

    #[must_use]
    pub const fn last_started_at(&self) -> Option<SystemTime> {
        self.last_started_at
    }

    #[must_use]
    pub const fn quota_reset_at(&self) -> Option<SystemTime> {
        self.quota_reset_at
    }

    #[must_use]
    pub const fn quota_remaining_rank(&self) -> Option<u64> {
        self.quota_remaining_rank
    }

    #[must_use]
    pub const fn failure_rate_basis_points(&self) -> Option<u16> {
        self.failure_rate_basis_points
    }

    #[must_use]
    pub const fn first_output_latency_ms(&self) -> Option<u64> {
        self.first_output_latency_ms
    }
}

/// 账号调度输入；Key 与账号组仅用于 Runtime 匹配绑定，不发送给插件。
#[derive(Debug, Clone)]
pub struct AccountScheduleInput {
    request_id: super::ModelRequestId,
    attempt_index: NonZeroU32,
    client_key_id: ClientApiKeyId,
    account_group_ids: Arc<[AccountGroupId]>,
    extension_scope: ExtensionCallScope,
    provider: ProviderKind,
    model: Option<String>,
    candidates: Vec<AccountScheduleCandidate>,
    execution_effects: Arc<ExecutionEffects>,
}

impl AccountScheduleInput {
    #[must_use]
    pub const fn request_id(&self) -> &super::ModelRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn attempt_index(&self) -> NonZeroU32 {
        self.attempt_index
    }

    #[must_use]
    pub const fn client_key_id(&self) -> &ClientApiKeyId {
        &self.client_key_id
    }

    #[must_use]
    pub fn account_group_ids(&self) -> &[AccountGroupId] {
        &self.account_group_ids
    }

    #[must_use]
    pub fn suppresses_plugin(&self, instance_id: &str) -> bool {
        self.extension_scope.contains(instance_id)
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    #[must_use]
    pub fn candidates(&self) -> &[AccountScheduleCandidate] {
        &self.candidates
    }

    /// 只供受信 Runtime 记录本次策略调用已经产生的外部副作用，不进入插件 wire。
    #[must_use]
    pub fn execution_effects(&self) -> Arc<ExecutionEffects> {
        Arc::clone(&self.execution_effects)
    }
}

/// 账号调度结果；插件选中的账号仍须通过 Core 资格复核并取得 Provider 租约。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountScheduleDecision {
    Pick(ProviderAccountId),
    Delegate,
    Reject,
}

/// 重试策略只能收窄宿主决定；不能自行创造恢复路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Delegate,
    Stop,
    Retry,
}

/// 协调器计算的安全失败事实；无凭据、原始错误或请求正文。
#[derive(Debug, Clone)]
pub struct RetryFacts {
    pub attempt_index: NonZeroU32,
    pub provider: ProviderKind,
    pub model: Option<String>,
    pub error_kind: crate::error::ProviderErrorKind,
    pub upstream_status: Option<u16>,
    pub send_state: super::UpstreamSendState,
    pub remaining_routing_attempts: u32,
    pub remaining_deadline: std::time::Duration,
    pub retry_allowed: bool,
}

#[derive(Debug, Clone)]
pub struct RetryInput {
    pub request_id: super::ModelRequestId,
    pub client_key_id: ClientApiKeyId,
    pub account_group_ids: Arc<[AccountGroupId]>,
    pub extension_scope: ExtensionCallScope,
    pub facts: RetryFacts,
}

/// 策略调用失败不携带插件原始消息，避免跨层泄漏不可信诊断。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("request policy call failed")]
pub struct RequestPolicyFault;

/// Provider 选择器需要区分明确拒绝、策略故障和等待期间过期的选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccountPolicyError {
    #[error("account scheduling policy rejected the request")]
    Rejected,
    #[error("account scheduling policy failed")]
    Fault,
    #[error("account scheduling candidate became stale")]
    StaleCandidate,
}

/// 一个发布代次的不可变请求策略计划。
pub trait RequestPolicyPlan: Send + Sync + fmt::Debug {
    fn retry_decision(
        &self,
        _input: RetryInput,
    ) -> BoxFuture<'static, Result<RetryDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(RetryDecision::Delegate) })
    }

    fn route_model(
        &self,
        input: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>>;

    fn schedule_account(
        &self,
        input: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>>;
}

/// 同一次请求冻结的策略计划和授权身份；完整集合引用保活所有 RPC 会话。
#[derive(Clone)]
pub struct RequestPolicyContext {
    plan: Arc<dyn RequestPolicyPlan>,
    _generation: ExtensionSetReference,
    request_id: super::ModelRequestId,
    client_key_id: ClientApiKeyId,
    account_group_ids: Arc<[AccountGroupId]>,
    extension_scope: ExtensionCallScope,
    execution_effects: Arc<ExecutionEffects>,
}

impl fmt::Debug for RequestPolicyContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestPolicyContext")
            .field("request_id", &self.request_id)
            .field("client_key_id", &self.client_key_id)
            .field("account_group_ids", &self.account_group_ids)
            .finish_non_exhaustive()
    }
}

impl RequestPolicyContext {
    pub async fn retry_decision(&self, facts: RetryFacts) -> RetryDecision {
        self.plan
            .retry_decision(RetryInput {
                request_id: self.request_id.clone(),
                client_key_id: self.client_key_id.clone(),
                account_group_ids: self.account_group_ids.clone(),
                extension_scope: self.extension_scope.clone(),
                facts,
            })
            .await
            .unwrap_or_else(|_| {
                tracing::warn!(
                    request_id = self.request_id.as_str(),
                    "重试策略调用失败，使用宿主决定"
                );
                RetryDecision::Delegate
            })
    }

    #[must_use]
    pub fn new(
        plan: Arc<dyn RequestPolicyPlan>,
        generation: ExtensionSetReference,
        request_id: super::ModelRequestId,
        client_key_id: ClientApiKeyId,
        mut account_group_ids: Vec<AccountGroupId>,
    ) -> Self {
        account_group_ids.sort();
        account_group_ids.dedup();
        Self {
            plan,
            _generation: generation,
            request_id,
            client_key_id,
            account_group_ids: account_group_ids.into(),
            extension_scope: ExtensionCallScope::default(),
            execution_effects: Arc::new(ExecutionEffects::default()),
        }
    }

    /// 附着父子调用图；计划必须跳过集合中的实例，防止直接或间接重入。
    #[must_use]
    pub fn with_extension_scope(mut self, extension_scope: ExtensionCallScope) -> Self {
        self.extension_scope = extension_scope;
        self
    }

    #[must_use]
    pub(crate) fn with_execution_effects(mut self, effects: Arc<ExecutionEffects>) -> Self {
        self.execution_effects = effects;
        self
    }

    #[must_use]
    pub fn extension_scope(&self) -> &ExtensionCallScope {
        &self.extension_scope
    }

    pub async fn route_model(
        &self,
        operation: Operation,
        requested_model: PublicModelId,
        available_providers: BTreeSet<ProviderKind>,
    ) -> Result<ModelRouteDecision, RequestPolicyFault> {
        self.plan
            .route_model(ModelRouteInput {
                request_id: self.request_id.clone(),
                client_key_id: self.client_key_id.clone(),
                account_group_ids: Arc::clone(&self.account_group_ids),
                extension_scope: self.extension_scope.clone(),
                operation,
                requested_model,
                available_providers,
                execution_effects: Arc::clone(&self.execution_effects),
            })
            .await
    }

    /// 硬绑定已在候选资格中收窄；插件先决定，委托时再采用内置亲和与排序。
    pub async fn select_account<'a>(
        &self,
        attempt_index: NonZeroU32,
        provider: &ProviderKind,
        model: Option<&str>,
        candidates: &'a [AccountCandidate],
        context: &AccountSelectionContext,
    ) -> Result<Option<AccountSelection<'a>>, AccountPolicyError> {
        let eligible = AccountSelector.policy_candidates(candidates, context);
        if eligible.is_empty() {
            return Ok(None);
        }
        let policy = context.policy;
        let projected = eligible
            .iter()
            .map(|candidate| AccountScheduleCandidate {
                account_id: candidate.account.id().clone(),
                weight: candidate.account.weight().get(),
                in_flight: candidate.signals.in_flight,
                maximum_concurrency: candidate
                    .account
                    .effective_concurrency(policy.max_concurrent_per_account())
                    .get(),
                last_started_at: candidate.signals.last_started_at,
                quota_reset_at: candidate.signals.quota_reset_at,
                quota_remaining_rank: candidate.signals.quota_remaining_rank,
                failure_rate_basis_points: candidate.signals.failure_rate_basis_points,
                first_output_latency_ms: candidate.signals.first_output_latency_ms,
            })
            .collect();
        let decision = self
            .plan
            .schedule_account(AccountScheduleInput {
                request_id: self.request_id.clone(),
                attempt_index,
                client_key_id: self.client_key_id.clone(),
                account_group_ids: Arc::clone(&self.account_group_ids),
                extension_scope: self.extension_scope.clone(),
                provider: provider.clone(),
                model: model.map(str::to_owned),
                candidates: projected,
                execution_effects: Arc::clone(&self.execution_effects),
            })
            .await
            .map_err(|_| AccountPolicyError::Fault)?;
        // 插件调用可能消耗显著时间；无论插件选择还是委托，均用返回时刻重新执行
        // 同一资格判断，不能让调用前尚未过期的候选在调用后被内置策略选中。
        let mut current = context.clone();
        current.now = SystemTime::now();
        match decision {
            AccountScheduleDecision::Delegate => AccountSelector
                .select(candidates, &current)
                .map(Some)
                .ok_or(AccountPolicyError::StaleCandidate),
            AccountScheduleDecision::Reject => Err(AccountPolicyError::Rejected),
            AccountScheduleDecision::Pick(account_id) => {
                if !eligible
                    .iter()
                    .any(|candidate| candidate.account.id() == &account_id)
                {
                    return Err(AccountPolicyError::Fault);
                }
                AccountSelector
                    .select_policy_candidate(candidates, &current, &account_id)
                    .map(Some)
                    .ok_or(AccountPolicyError::StaleCandidate)
            }
        }
    }
}

/// 策略计划注册冲突；同一代次不能被静默替换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("request policy generation is already registered")]
pub struct RequestPolicyRegistrationError;

/// 按发布集合解析策略计划的非拥有索引。
#[derive(Clone, Default)]
pub struct RequestPolicyExtensionIndex {
    sets: Arc<RwLock<BTreeMap<ExtensionSetId, Weak<dyn RequestPolicyPlan>>>>,
}

impl RequestPolicyExtensionIndex {
    pub fn register(
        &self,
        id: ExtensionSetId,
        plan: Arc<dyn RequestPolicyPlan>,
    ) -> Result<Arc<dyn RequestPolicyPlan>, RequestPolicyRegistrationError> {
        let mut sets = self
            .sets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.retain(|_, plan| plan.strong_count() > 0);
        if sets.contains_key(&id) {
            return Err(RequestPolicyRegistrationError);
        }
        sets.insert(id, Arc::downgrade(&plan));
        Ok(plan)
    }

    #[must_use]
    pub fn resolve(
        &self,
        generation: &ExtensionSetReference,
    ) -> Option<Arc<dyn RequestPolicyPlan>> {
        self.sets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(generation.id())
            .and_then(Weak::upgrade)
    }
}
