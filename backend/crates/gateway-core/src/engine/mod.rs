//! 模型请求生命周期、单行持久化 port 与 commit/send/cancellation 边界。

pub mod admission;
pub mod continuation;
pub mod coordinator;
pub mod credential;
pub mod execution;
pub mod probe;
pub mod provider;

pub use coordinator::{AttemptCoordinator, ResponseExecutionSession};

use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::channel::oneshot;
use thiserror::Error;

use crate::accounting::{CostEstimate, Usage};
use crate::engine::continuation::{ContinuationBinding, NativeContinuationPin};
use crate::engine::credential::{AccountSelectionPolicy, ProviderAccountId};
use crate::error::{
    GatewayError, IdentifierError, ProviderError, ProviderErrorKind, StoreError, validate_text,
};
use crate::event::ProviderEvent;
use crate::operation::OperationKind;
use crate::policy::ClientApiKeyId;
use crate::routing::{
    ConfigRevision, ProviderInstanceId, ProviderKind, PublicModelId, UpstreamModelId,
};

/// `model_requests.id`。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelRequestId(String);

impl ModelRequestId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 128, false, Some("req_"))?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelRequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 上游是否可能已经收到业务 payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpstreamSendState {
    NotSent,
    Sent,
    Ambiguous,
}

impl UpstreamSendState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotSent => "not_sent",
            Self::Sent => "sent",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// `model_requests.outcome`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionOutcome {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Incomplete,
}

impl ExecutionOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Incomplete => "incomplete",
        }
    }
}

/// Request-local attempt 的原因，不对应数据库表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttemptTrigger {
    Initial,
    AccountRetry,
    InstanceFallback,
}

impl AttemptTrigger {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::AccountRetry => "account_retry",
            Self::InstanceFallback => "instance_fallback",
        }
    }
}

/// 一次实际上游调用对 Provider instance 健康度产生的事实。
///
/// 该事实只描述调用结果，不在 Core 中定义 circuit 策略或持久化方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAttemptOutcome {
    /// 上游流自然完成且通过 canonical event 序列校验。
    Succeeded {
        provider_instance_id: ProviderInstanceId,
    },
    /// 上游打开或流式阶段返回了稳定 Provider 错误。
    Failed {
        provider_instance_id: ProviderInstanceId,
        error_kind: ProviderErrorKind,
    },
}

impl ProviderAttemptOutcome {
    /// 返回本次调用实际归属的 Provider instance。
    #[must_use]
    pub const fn provider_instance_id(&self) -> &ProviderInstanceId {
        match self {
            Self::Succeeded {
                provider_instance_id,
            }
            | Self::Failed {
                provider_instance_id,
                ..
            } => provider_instance_id,
        }
    }

    /// 成功返回 `None`，失败返回稳定 Provider 错误分类。
    #[must_use]
    pub const fn error_kind(&self) -> Option<ProviderErrorKind> {
        match self {
            Self::Succeeded { .. } => None,
            Self::Failed { error_kind, .. } => Some(*error_kind),
        }
    }
}

struct CancellationState {
    cancelled: AtomicBool,
    waiters: Mutex<Vec<oneshot::Sender<()>>>,
}

/// 可克隆的请求取消信号。
#[derive(Clone)]
pub struct CancellationToken(Arc<CancellationState>);

impl fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            waiters: Mutex::new(Vec::new()),
        }))
    }

    pub fn cancel(&self) {
        if self.0.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let waiters = {
            let mut guard = lock_unpoisoned(&self.0.waiters);
            std::mem::take(&mut *guard)
        };
        for waiter in waiters {
            let _ = waiter.send(());
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut waiters = lock_unpoisoned(&self.0.waiters);
            if self.is_cancelled() {
                return;
            }
            waiters.push(sender);
        }
        let _ = receiver.await;
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 单次 attempt 的账号选择与账号绑定状态事实。
///
/// `required_account` 只用于管理端 connection test 等必须命中唯一账号的内部请求；
/// 一旦设置，Core 与 Provider 都不得换号或切换 target。
#[derive(Debug, Clone, Default)]
pub struct AccountAttemptContext {
    excluded_accounts: BTreeSet<ProviderAccountId>,
    required_account: Option<ProviderAccountId>,
    state_owner: Option<ProviderAccountStateOwner>,
    credential_recovery_attempted: bool,
}

impl AccountAttemptContext {
    #[must_use]
    pub const fn new(
        excluded_accounts: BTreeSet<ProviderAccountId>,
        required_account: Option<ProviderAccountId>,
        state_owner: Option<ProviderAccountStateOwner>,
    ) -> Self {
        Self {
            excluded_accounts,
            required_account,
            state_owner,
            credential_recovery_attempted: false,
        }
    }

    /// 标记本请求已对即将选择的固定账号执行过一次凭据恢复。
    #[must_use]
    pub const fn with_credential_recovery_attempted(mut self, attempted: bool) -> Self {
        self.credential_recovery_attempted = attempted;
        self
    }

    #[must_use]
    pub const fn excluded_accounts(&self) -> &BTreeSet<ProviderAccountId> {
        &self.excluded_accounts
    }

    #[must_use]
    pub const fn required_account(&self) -> Option<&ProviderAccountId> {
        self.required_account.as_ref()
    }

    #[must_use]
    pub const fn state_owner(&self) -> Option<&ProviderAccountStateOwner> {
        self.state_owner.as_ref()
    }

    #[must_use]
    pub const fn credential_recovery_attempted(&self) -> bool {
        self.credential_recovery_attempted
    }
}

/// 请求中 Provider 账号绑定状态的唯一归属。
///
/// `turn_state` 等 opaque 状态只能发送给创建它的 Provider、instance 与账号；
/// Core 在首次真实选号后冻结该事实，Provider 据此决定是否清理跨账号状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountStateOwner {
    provider: ProviderKind,
    instance: ProviderInstanceId,
    account: ProviderAccountId,
}

impl ProviderAccountStateOwner {
    #[must_use]
    pub const fn new(
        provider: ProviderKind,
        instance: ProviderInstanceId,
        account: ProviderAccountId,
    ) -> Self {
        Self {
            provider,
            instance,
            account,
        }
    }

    #[must_use]
    pub fn from_continuation(pin: &NativeContinuationPin) -> Self {
        Self::new(
            pin.provider().clone(),
            pin.instance().clone(),
            pin.account().clone(),
        )
    }

    #[must_use]
    pub fn matches(
        &self,
        provider: &ProviderKind,
        instance: &ProviderInstanceId,
        account: &ProviderAccountId,
    ) -> bool {
        self.provider == *provider && self.instance == *instance && self.account == *account
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    #[must_use]
    pub const fn instance(&self) -> &ProviderInstanceId {
        &self.instance
    }

    #[must_use]
    pub const fn account(&self) -> &ProviderAccountId {
        &self.account
    }
}

/// 当前 attempt 对 previous-response 的唯一处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationAttempt {
    /// 没有 previous-response。
    None,
    /// 使用 Store 解析出的原生 handle 与账号绑定。
    Native,
    /// 在原账号上用完整连接内 transcript 重放。
    ReplayOwner,
    /// transcript 已可携带，允许选择其他账号。
    ReplayAny,
}

/// Provider 每次执行可见的 request-local context。
#[derive(Debug, Clone)]
pub struct RequestAttemptContext {
    request_id: ModelRequestId,
    client_api_key_ref: ClientApiKeyId,
}

impl RequestAttemptContext {
    #[must_use]
    pub const fn new(request_id: ModelRequestId, client_api_key_ref: ClientApiKeyId) -> Self {
        Self {
            request_id,
            client_api_key_ref,
        }
    }

    #[must_use]
    pub const fn request_id(&self) -> &ModelRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn client_api_key_ref(&self) -> &ClientApiKeyId {
        &self.client_api_key_ref
    }
}

/// Provider 每次执行可见的 request-local context。
#[derive(Debug, Clone)]
pub struct AttemptContext {
    request: RequestAttemptContext,
    attempt_index: NonZeroU32,
    deadline: SystemTime,
    account_selection_policy: AccountSelectionPolicy,
    account: AccountAttemptContext,
    continuation: Option<ContinuationBinding>,
    continuation_attempt: ContinuationAttempt,
    cancellation: CancellationToken,
}

impl AttemptContext {
    #[must_use]
    pub const fn new(
        request: RequestAttemptContext,
        attempt_index: NonZeroU32,
        deadline: SystemTime,
        account_selection_policy: AccountSelectionPolicy,
        account: AccountAttemptContext,
        continuation: Option<ContinuationBinding>,
        cancellation: CancellationToken,
    ) -> Self {
        let continuation_attempt = if continuation.is_some() {
            ContinuationAttempt::Native
        } else {
            ContinuationAttempt::None
        };
        Self {
            request,
            attempt_index,
            deadline,
            account_selection_policy,
            account,
            continuation,
            continuation_attempt,
            cancellation,
        }
    }

    /// 覆盖本次 attempt 的 continuation 恢复方式。
    #[must_use]
    pub const fn with_continuation_attempt(
        mut self,
        continuation_attempt: ContinuationAttempt,
    ) -> Self {
        self.continuation_attempt = continuation_attempt;
        self
    }

    #[must_use]
    pub const fn request_id(&self) -> &ModelRequestId {
        self.request.request_id()
    }

    /// 返回隔离 Provider 会话与缓存身份的下游租户引用。
    #[must_use]
    pub const fn client_api_key_ref(&self) -> &ClientApiKeyId {
        self.request.client_api_key_ref()
    }

    #[must_use]
    pub const fn attempt_index(&self) -> NonZeroU32 {
        self.attempt_index
    }

    #[must_use]
    pub const fn deadline(&self) -> SystemTime {
        self.deadline
    }

    #[must_use]
    pub const fn account_selection_policy(&self) -> AccountSelectionPolicy {
        self.account_selection_policy
    }

    #[must_use]
    pub const fn excluded_accounts(&self) -> &BTreeSet<ProviderAccountId> {
        self.account.excluded_accounts()
    }

    /// 管理端 connection test 等内部请求强制使用的唯一账号。
    #[must_use]
    pub const fn required_account(&self) -> Option<&ProviderAccountId> {
        self.account.required_account()
    }

    #[must_use]
    pub const fn account_state_owner(&self) -> Option<&ProviderAccountStateOwner> {
        self.account.state_owner()
    }

    /// 同一请求是否已经为当前固定账号执行过一次 OAuth 恢复。
    #[must_use]
    pub const fn credential_recovery_attempted(&self) -> bool {
        self.account.credential_recovery_attempted()
    }

    #[must_use]
    pub const fn continuation(&self) -> Option<&ContinuationBinding> {
        self.continuation.as_ref()
    }

    #[must_use]
    pub const fn continuation_attempt(&self) -> ContinuationAttempt {
        self.continuation_attempt
    }

    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

/// 创建唯一 `model_requests` 行所需的入口事实。
#[derive(Debug, Clone)]
pub struct NewModelRequest {
    pub id: ModelRequestId,
    pub client_api_key_id: Option<ClientApiKeyId>,
    pub client_api_key_ref: ClientApiKeyId,
    pub config_revision: ConfigRevision,
    pub protocol: String,
    pub operation: OperationKind,
    pub endpoint: String,
    pub client_transport: String,
    pub requested_model: PublicModelId,
    pub input_token_estimate: u64,
    pub client_ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub reasoning_effort: Option<String>,
    pub reasoning_preset: Option<String>,
    pub request_kind: Option<String>,
    pub subagent_kind: Option<String>,
    pub compact: bool,
    pub started_at: SystemTime,
    pub deadline_at: SystemTime,
}

/// 每次真实上游发送前对同一 `model_requests` 行的更新。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRecord {
    pub request_id: ModelRequestId,
    pub attempt_count: NonZeroU32,
    pub trigger: AttemptTrigger,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_kind: ProviderKind,
    pub provider_account_id: Option<ProviderAccountId>,
    pub provider_account_ref: Option<ProviderAccountId>,
    pub upstream_model_id: UpstreamModelId,
    pub upstream_transport: String,
    pub http_version: Option<String>,
}

/// 需要解释换号或 Provider instance 切换的中间失败。
#[derive(Debug)]
pub struct IntermediateFailure {
    pub request_id: ModelRequestId,
    pub attempt_index: NonZeroU32,
    pub trigger: AttemptTrigger,
    pub instance_id: ProviderInstanceId,
    pub provider_kind: ProviderKind,
    pub account_id: Option<ProviderAccountId>,
    pub upstream_model_id: UpstreamModelId,
    pub upstream_status_code: Option<u16>,
    pub upstream_request_id: Option<String>,
    pub error: ProviderError,
    pub latency: Duration,
}

/// `model_requests` 可用的毫秒级阶段耗时。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelRequestTimings {
    pub transport_decision_wait_ms: Option<u64>,
    pub connect_ms: Option<u64>,
    pub headers_ms: Option<u64>,
    pub first_event_ms: Option<u64>,
    pub first_reasoning_ms: Option<u64>,
    pub first_text_ms: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub provider_processing_ms: Option<u64>,
    pub latency_ms: Option<u64>,
}

/// 单行模型请求的终态写回。
#[derive(Debug)]
pub struct ModelRequestFinalization {
    pub request_id: ModelRequestId,
    pub outcome: ExecutionOutcome,
    pub send_state: UpstreamSendState,
    pub attempt_count: u32,
    pub downstream_committed_at: Option<SystemTime>,
    pub client_status_code: Option<u16>,
    pub client_response_id: Option<String>,
    pub upstream_status_code: Option<u16>,
    pub upstream_request_id: Option<String>,
    pub upstream_response_id: Option<String>,
    pub upstream_transport: Option<String>,
    pub http_version: Option<String>,
    pub error: Option<GatewayError>,
    pub provider_error_code: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub usage: Usage,
    pub cost: CostEstimate,
    pub timings: ModelRequestTimings,
    pub completed_at: SystemTime,
}

/// 进程崩溃后按冻结 deadline 收敛的 running 请求数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub requests: u64,
}

/// `model_requests` 与必要 `ops_events` 的唯一 Core port。
#[async_trait]
pub trait ExecutionStore: Send + Sync {
    async fn create_model_request(&self, request: NewModelRequest) -> Result<(), StoreError>;
    async fn record_attempt(&self, attempt: AttemptRecord) -> Result<(), StoreError>;
    async fn mark_send_state(
        &self,
        request_id: &ModelRequestId,
        state: UpstreamSendState,
    ) -> Result<(), StoreError>;
    async fn mark_downstream_committed(
        &self,
        request_id: &ModelRequestId,
        committed_at: SystemTime,
        client_status_code: Option<u16>,
    ) -> Result<(), StoreError>;
    async fn record_client_status(
        &self,
        request_id: &ModelRequestId,
        client_status_code: u16,
    ) -> Result<(), StoreError>;
    async fn record_intermediate_failure(
        &self,
        failure: IntermediateFailure,
    ) -> Result<(), StoreError>;
    async fn finalize_model_request(
        &self,
        finalization: ModelRequestFinalization,
    ) -> Result<(), StoreError>;

    async fn recover_expired(&self, now: SystemTime) -> Result<RecoveryReport, StoreError>;
}

/// 首事件能否交付客户端的持久化屏障。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitRequirement {
    CommitBeforeDelivery,
    AlreadyCommitted,
}

#[derive(Debug)]
pub struct CoordinatedEvent {
    events: Vec<ProviderEvent>,
    commit_requirement: CommitRequirement,
}

impl CoordinatedEvent {
    #[must_use]
    pub fn single(event: ProviderEvent, commit_requirement: CommitRequirement) -> Self {
        Self {
            events: vec![event],
            commit_requirement,
        }
    }

    /// 创建一个必须在同一下游提交边界内按序编码的事件批次。
    ///
    /// # Errors
    ///
    /// 空批次无法建立交付边界。
    pub fn try_batch(
        events: Vec<ProviderEvent>,
        commit_requirement: CommitRequirement,
    ) -> Result<Self, EngineError> {
        if events.is_empty() {
            return Err(EngineError::InvalidDeliveryState);
        }
        Ok(Self {
            events,
            commit_requirement,
        })
    }

    #[must_use]
    pub const fn commit_requirement(&self) -> CommitRequirement {
        self.commit_requirement
    }

    #[must_use]
    pub fn into_provider_events(self) -> Vec<ProviderEvent> {
        self.events
    }
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("execution store failed")]
    Store(#[from] StoreError),
    #[error("provider `{provider}` is not registered")]
    ProviderNotRegistered { provider: String },
    #[error("provider metadata did not match the selected Provider instance and account")]
    ProviderMetadataMismatch,
    #[error("native continuation pin did not match the selected account")]
    ContinuationPinMismatch,
    #[error("provider did not use the account required by this execution")]
    RequiredAccountMismatch,
    #[error("provider execution failed")]
    Provider(ProviderError),
    #[error("request was cancelled")]
    Cancelled,
    #[error("request deadline elapsed")]
    Deadline,
    #[error("routing plan has no candidate")]
    EmptyRoutingPlan,
    #[error("downstream delivery must be committed before execution can continue")]
    DownstreamCommitRequired,
    #[error("downstream delivery cannot be committed in the current state")]
    InvalidDeliveryState,
}

/// Engine 只组合 Store 与 Provider Registry；重试算法完全位于 coordinator。
pub struct GatewayEngine<S: ?Sized> {
    store: Arc<S>,
    providers: provider::ProviderRegistry,
}

impl<S: ?Sized> GatewayEngine<S> {
    #[must_use]
    pub const fn new(store: Arc<S>, providers: provider::ProviderRegistry) -> Self {
        Self { store, providers }
    }

    #[must_use]
    pub const fn store(&self) -> &Arc<S> {
        &self.store
    }

    #[must_use]
    pub const fn providers(&self) -> &provider::ProviderRegistry {
        &self.providers
    }
}
