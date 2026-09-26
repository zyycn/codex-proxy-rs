//! 数据面执行用例：认证、准入、路由、continuation 与会话生命周期。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::IpAddr;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant, SystemTime};

use futures::{FutureExt, future::BoxFuture, pin_mut, select_biased};
use futures_timer::Delay;
use uuid::Uuid;

use crate::concurrency::{CapacityWait, ConcurrencyWaitBudget, ConcurrencyWaitQueue};
use crate::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionPort, ClientAdmissionRejection, ClientAdmissionRequest,
};
use crate::engine::authentication::{
    ClientAuthenticationRequest, FrontendAuthenticationDecision,
    FrontendAuthenticationExtensionIndex,
};
use crate::engine::budget::{ClientBudgetCharge, ClientBudgetPort};
use crate::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, NativeContinuationPort,
    NativeContinuationStoreErrorKind, PreviousResponseId,
};
use crate::engine::coordinator::{CoordinationExtensions, ResponseExecutionSession};
use crate::engine::extensions::ExtensionCallScope;
use crate::engine::middleware::{
    FrozenMiddlewarePlan, MiddlewareAuthority, MiddlewareContext, MiddlewareExtensionIndex,
    MiddlewareTarget,
};
use crate::engine::nested::{
    AffinityLookupPort, AffinityLookupRequest, AffinityLookupResult, BoundModelExecutionBinding,
    BoundModelExecutionRequest, ExecutionEffects, NestedModelExecutionPort,
    NestedModelExecutionRequest,
};
use crate::engine::observation::{
    FrozenRequestObservationContext, RequestObservationDispatch, RequestObserverExtensionIndex,
};
use crate::engine::policy::{
    ModelRouteDecision, RequestPolicyContext, RequestPolicyExtensionIndex,
};
use crate::engine::probe::{
    AccountProbe, AccountProbeError, AccountProbeErrorSource, AccountProbeRequest,
    AccountProbeResult, AccountProbeUpstreamResponse,
};
use crate::engine::provider::ProviderRegistry;
use crate::engine::{
    AttemptCoordinator, AttemptRecord, CoordinatedEvent, EngineError, ExecutionStore,
    GatewayEngine, IntermediateFailure, ModelRequestFinalization, ModelRequestId, NewModelRequest,
    ProbeFailure, ProviderAccountId, RecoveryReport, UpstreamSendState,
};
use crate::error::{GatewayError, GatewayErrorKind, StoreError};
use crate::event::{GatewayEvent, ProviderEvent, ProviderResponseHeader};
use crate::identity::ProviderKind;
use crate::lifecycle::CancellationToken;
use crate::operation::{Operation, ProviderSessionState};
use crate::policy::{ClientApiKeyId, ClientPolicy};
use crate::provider_ports::{ProviderSessionAffinityPort, ProviderStoreErrorKind};
use crate::routing::{
    FrozenAccountScope, ProviderCatalogUnavailable, PublicModelDescriptor, PublicModelId,
    RoutingContext, RuntimeSnapshot, UpstreamModelId,
};
use crate::runtime::{RuntimeSnapshotHandle, RuntimeSnapshotPublisher};

const MODEL_REQUEST_DEADLINE: Duration = Duration::from_secs(10 * 60);
const COORDINATION_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_NESTED_DEPTH: usize = 4;
const MAX_NESTED_EXECUTIONS: usize = 16;
const MAX_CONCURRENT_NESTED_EXECUTIONS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTransport {
    HttpJson,
    HttpSse,
    WebSocket,
    InternalProbe,
    InternalPlugin,
}

impl ClientTransport {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HttpJson => "http_json",
            Self::HttpSse => "http_sse",
            Self::WebSocket => "websocket",
            Self::InternalProbe => "internal",
            Self::InternalPlugin => "internal_plugin",
        }
    }
}

/// API 解码后交给 Core 的稳定请求元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRequestMetadata {
    pub protocol: String,
    pub endpoint: String,
    pub transport: ClientTransport,
    pub stream: bool,
    pub client_ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub previous_response_id: Option<PreviousResponseId>,
}

#[derive(Clone)]
pub struct AuthenticatedClient {
    snapshot: Arc<RuntimeSnapshot>,
    policy: ClientPolicy,
    authentication: Option<ClientAuthenticationRequest>,
}

impl AuthenticatedClient {
    #[must_use]
    pub const fn snapshot(&self) -> &Arc<RuntimeSnapshot> {
        &self.snapshot
    }

    #[must_use]
    pub const fn policy(&self) -> &ClientPolicy {
        &self.policy
    }
}

impl fmt::Debug for AuthenticatedClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedClient")
            .field("key_id", &self.policy.key_id())
            .field("revision", &self.snapshot.revision())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClientAuthenticationError {
    #[error("client API key is invalid")]
    InvalidKey,
    #[error("runtime snapshot is unavailable")]
    SnapshotUnavailable,
    #[error("frontend authentication provider is unavailable")]
    ProviderUnavailable,
}

pub struct StartExecution {
    pub client: AuthenticatedClient,
    pub public_model: PublicModelId,
    pub operation: Operation,
    pub metadata: ExecutionRequestMetadata,
}

/// 入口中间件开始前冻结的根请求身份与生命周期。
///
/// 中间件短路时本值直接释放；调用 `next` 后必须原样交回
/// [`ExecutionService::start_prepared`]，不能重新认证为另一个 Key。
pub struct PreparedRootExecution {
    client: AuthenticatedClient,
    request_id: ModelRequestId,
    started_at: SystemTime,
    deadline_at: SystemTime,
    cancellation: CancellationToken,
    execution_effects: Arc<ExecutionEffects>,
    execution_effects_baseline: usize,
}

impl PreparedRootExecution {
    fn new(client: AuthenticatedClient) -> Result<Self, GatewayError> {
        let started_at = SystemTime::now();
        let deadline_at = started_at
            .checked_add(MODEL_REQUEST_DEADLINE)
            .ok_or_else(|| {
                GatewayError::new(GatewayErrorKind::Internal, "system clock is invalid")
            })?;
        let execution_effects = Arc::new(ExecutionEffects::default());
        let execution_effects_baseline = execution_effects.epoch();
        Ok(Self {
            client,
            request_id: new_request_id()?,
            started_at,
            deadline_at,
            cancellation: CancellationToken::new(),
            execution_effects,
            execution_effects_baseline,
        })
    }

    #[must_use]
    pub const fn request_id(&self) -> &ModelRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn client(&self) -> &AuthenticatedClient {
        &self.client
    }

    #[must_use]
    pub const fn started_at(&self) -> SystemTime {
        self.started_at
    }

    #[must_use]
    pub const fn deadline_at(&self) -> SystemTime {
        self.deadline_at
    }

    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// 以冻结的入口身份补齐 Runtime-only binding 事实；调用位置事实由 API/Core 提供。
    #[must_use]
    pub fn middleware_context(
        &self,
        mut target: MiddlewareTarget,
        extension_scope: ExtensionCallScope,
    ) -> MiddlewareContext {
        target.request_id = self.request_id.clone();
        let account_group_ids = self
            .client
            .policy
            .account_scope()
            .routing_snapshot()
            .groups_snapshot()
            .iter()
            .map(|group| group.id().clone())
            .collect::<Vec<_>>();
        MiddlewareContext::new(
            target,
            MiddlewareAuthority {
                client_key_id: self.client.policy.key_id().clone(),
                account_group_ids: Arc::from(account_group_ids),
                cancellation: self.cancellation.clone(),
                deadline: self.deadline_at,
                extension_scope,
                execution_effects: Some(Arc::clone(&self.execution_effects)),
            },
        )
    }
}

/// 入口中间件调用 `next` 后交给 Core 的已解码执行输入。
pub struct PreparedExecutionRequest {
    pub public_model: PublicModelId,
    pub operation: Operation,
    pub metadata: ExecutionRequestMetadata,
}

/// 启动一个由协议 adapter 明确绑定到 Provider 自有端点的请求。
pub struct StartProviderExecution {
    pub client: AuthenticatedClient,
    pub provider: ProviderKind,
    /// Provider 自有模型端点可固定上游模型；非模型端点保持 `None`。
    pub upstream_model: Option<UpstreamModelId>,
    pub operation: Operation,
    pub metadata: ExecutionRequestMetadata,
}

enum ExecutionTarget {
    Model(PublicModelId),
    ProviderEndpoint {
        provider: ProviderKind,
        upstream_model: Option<UpstreamModelId>,
    },
}

impl ExecutionTarget {
    fn public_model(&self) -> Option<&PublicModelId> {
        match self {
            Self::Model(model) => Some(model),
            Self::ProviderEndpoint { .. } => None,
        }
    }

    fn into_public_model(self) -> Option<PublicModelId> {
        match self {
            Self::Model(model) => Some(model),
            Self::ProviderEndpoint { .. } => None,
        }
    }
}

struct PendingStartExecution {
    client: AuthenticatedClient,
    target: ExecutionTarget,
    operation: Operation,
    metadata: ExecutionRequestMetadata,
}

struct AuthorizedExecution {
    account_scope: Arc<FrozenAccountScope>,
    deadline_at: SystemTime,
    cancellation: CancellationToken,
    extension_scope: ExtensionCallScope,
    required_provider: Option<ProviderKind>,
    required_account: Option<ProviderAccountId>,
    nested: Option<NestedExecutionFacts>,
    bound: Option<BoundExecutionFacts>,
    graph: Option<Arc<NestedExecutionGraph>>,
    execution_effects_baseline: usize,
    nested_permit: Option<NestedExecutionPermit>,
}

struct ExecutionStartGuard {
    admission: ExecutionAdmission,
    active_request: Option<ActiveRequestLease>,
    concurrency_wait_budget: ConcurrencyWaitBudget,
    admission_decision_ms: Option<u64>,
}

struct PreparedExecutionStart {
    request_id: ModelRequestId,
    started_at: SystemTime,
    plan: crate::routing::RoutingPlan,
    extensions: CoordinationExtensions,
    authorization: AuthorizedExecution,
    guard: ExecutionStartGuard,
}

struct NestedExecutionFacts {
    parent_request_id: ModelRequestId,
    initiating_plugin_instance_id: String,
}

struct BoundExecutionFacts {
    initiating_plugin_instance_id: String,
}

struct NestedExecutionGraph {
    total_started: AtomicUsize,
    active: AtomicUsize,
    effects: Arc<ExecutionEffects>,
}

impl NestedExecutionGraph {
    fn new(effects: Arc<ExecutionEffects>) -> Self {
        Self {
            total_started: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            effects,
        }
    }

    fn acquire(self: &Arc<Self>) -> Result<NestedExecutionPermit, GatewayError> {
        if self
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONCURRENT_NESTED_EXECUTIONS).then_some(active + 1)
            })
            .is_err()
        {
            return Err(GatewayError::new(
                GatewayErrorKind::ConcurrencyQueueFull,
                "nested model execution concurrency is exhausted",
            ));
        }
        if self
            .total_started
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |total| {
                (total < MAX_NESTED_EXECUTIONS).then_some(total + 1)
            })
            .is_err()
        {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return Err(GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "nested model execution budget is exhausted",
            ));
        }
        Ok(NestedExecutionPermit {
            graph: Arc::clone(self),
            armed: true,
        })
    }
}

struct NestedExecutionPermit {
    graph: Arc<NestedExecutionGraph>,
    armed: bool,
}

impl NestedExecutionPermit {
    fn release(mut self) {
        if self.armed {
            self.graph.active.fetch_sub(1, Ordering::AcqRel);
            self.armed = false;
        }
    }
}

impl Drop for NestedExecutionPermit {
    fn drop(&mut self) {
        if self.armed {
            self.graph.active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

struct ActiveRequestAuthority {
    client: AuthenticatedClient,
    account_scope: Arc<FrozenAccountScope>,
    deadline_at: SystemTime,
    cancellation: CancellationToken,
    extension_scope: ExtensionCallScope,
    graph: Arc<NestedExecutionGraph>,
}

type ActiveRequestRegistry = Arc<Mutex<BTreeMap<ModelRequestId, Weak<ActiveRequestAuthority>>>>;

struct ActiveRequestLease {
    registry: ActiveRequestRegistry,
    request_id: ModelRequestId,
    authority: Arc<ActiveRequestAuthority>,
}

/// 管理页或 CLI 一次调用持有的显式模型执行身份。
///
/// 结构不暴露 Key 明文；Runtime 只能把它原样交回 Core 发起模型请求。
#[derive(Clone)]
pub struct BoundModelExecutionContext {
    authority: Arc<BoundModelExecutionAuthority>,
}

struct BoundModelExecutionAuthority {
    client: AuthenticatedClient,
    account_scope: Arc<FrozenAccountScope>,
    deadline_at: SystemTime,
    cancellation: CancellationToken,
    extension_scope: ExtensionCallScope,
    graph: Arc<NestedExecutionGraph>,
    initiating_plugin_instance_id: String,
}

impl fmt::Debug for BoundModelExecutionContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundModelExecutionContext")
            .field("key_id", &self.authority.client.policy.key_id())
            .field(
                "initiating_plugin_instance_id",
                &self.authority.initiating_plugin_instance_id,
            )
            .field("deadline_at", &self.authority.deadline_at)
            .finish_non_exhaustive()
    }
}

impl Drop for BoundModelExecutionAuthority {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl ActiveRequestLease {
    fn release(self) {
        drop(self);
    }
}

impl Drop for ActiveRequestLease {
    fn drop(&mut self) {
        self.authority.cancellation.cancel();
        let mut active = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active
            .get(&self.request_id)
            .and_then(Weak::upgrade)
            .is_some_and(|authority| Arc::ptr_eq(&authority, &self.authority))
        {
            active.remove(&self.request_id);
        }
    }
}

pub struct StartedExecution {
    pub request_id: ModelRequestId,
    pub created_at: SystemTime,
    pub stream: bool,
    pub session: Box<dyn ExecutionSession>,
}

pub trait ExecutionSession: Send {
    fn trace(&self) -> crate::diagnostics::TraceContext {
        crate::diagnostics::TraceContext::default()
    }
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>>;
    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>>;
    fn response_headers(&self) -> &[ProviderResponseHeader];
    fn response_status_code(&self) -> Option<u16> {
        None
    }
    fn discard_pending_delivery(&mut self) -> Result<(), EngineError> {
        Err(EngineError::InvalidDeliveryState)
    }
    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), EngineError>>;
    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), EngineError>>;
    /// 执行已终结且结算、准入释放均已返回，协议层才可以放弃清理责任。
    /// 结算失败仍由 Store 保留费用重试，释放失败仍按租约 TTL 收敛。
    fn is_finalized(&self) -> bool;
    fn cancel(&self);
    /// 将会话交给宿主持续驱动清理；取消请求事件的等待不会丢弃已开始的结算。
    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()>;
}

pub trait ExecutionService: Send + Sync {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError>;
    fn authenticate_request(
        &self,
        request: ClientAuthenticationRequest,
    ) -> BoxFuture<'_, Result<AuthenticatedClient, ClientAuthenticationError>> {
        Box::pin(async move {
            let plaintext = request
                .native_bearer()
                .ok_or(ClientAuthenticationError::InvalidKey)?;
            self.authenticate(plaintext.expose_for_auth())
        })
    }
    /// 只验证入口认证信封，不记录 Key 使用事实或执行推理准入。
    fn verify_request(
        &self,
        _request: ClientAuthenticationRequest,
    ) -> BoxFuture<'_, Result<AuthenticatedClient, ClientAuthenticationError>> {
        Box::pin(async { Err(ClientAuthenticationError::InvalidKey) })
    }
    fn public_models(&self, client: &AuthenticatedClient) -> Vec<PublicModelId>;
    fn client_model_catalog<'a>(
        &'a self,
        _client: &'a AuthenticatedClient,
        _protocol: &'a str,
        _client_version: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PublicModelDescriptor>, ProviderCatalogUnavailable>> {
        Box::pin(async { Err(ProviderCatalogUnavailable) })
    }
    fn contains_public_model(&self, client: &AuthenticatedClient, model: &PublicModelId) -> bool;

    /// 重新鉴权并冻结本次根请求身份、请求 ID、deadline 与取消域。
    fn prepare_execution(
        &self,
        client: AuthenticatedClient,
    ) -> BoxFuture<'_, Result<PreparedRootExecution, GatewayError>> {
        Box::pin(async move { PreparedRootExecution::new(client) })
    }

    /// 由已经验证管理会话与插件 models 域的宿主入口选择 Key。
    /// 不向页面提供明文，也不跳过普通请求的扩展、准入和计量。
    fn prepare_plugin_execution(
        &self,
        _client_key_id: &ClientApiKeyId,
    ) -> BoxFuture<'_, Result<PreparedRootExecution, GatewayError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Unauthorized,
                "plugin model entry is unavailable",
            ))
        })
    }

    /// 为已经通过 `verify_request` 的只读入口建立中间件生命周期，不重复写 Key 使用事实。
    fn prepare_verified_execution(
        &self,
        client: AuthenticatedClient,
    ) -> Result<PreparedRootExecution, GatewayError> {
        PreparedRootExecution::new(client)
    }

    /// 解析与准备阶段冻结的发布代次一致的中间件计划。
    fn middleware_plan(&self, _prepared: &PreparedRootExecution) -> Option<FrozenMiddlewarePlan> {
        None
    }

    /// 消费一次已准备身份并进入原有 Core 路由、准入、attempt 与结算路径。
    fn start_prepared(
        &self,
        prepared: PreparedRootExecution,
        request: PreparedExecutionRequest,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            self.start(StartExecution {
                client: prepared.client,
                public_model: request.public_model,
                operation: request.operation,
                metadata: request.metadata,
            })
            .await
        })
    }

    /// 消费一次已准备身份，并进入 Provider 自有端点的原有准入与结算路径。
    fn start_prepared_provider_endpoint(
        &self,
        prepared: PreparedRootExecution,
        provider: ProviderKind,
        upstream_model: Option<UpstreamModelId>,
        operation: Operation,
        metadata: ExecutionRequestMetadata,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            self.start_provider_endpoint(StartProviderExecution {
                client: prepared.client,
                provider,
                upstream_model,
                operation,
                metadata,
            })
            .await
        })
    }

    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>>;
    fn start_provider_endpoint(
        &self,
        request: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>>;
}

/// 只验证 Client API Key 并返回稳定 Key ID，不产生请求使用事实。
///
/// 管理侧登录等非数据面场景必须使用该端口，避免把认证本身误记为一次 Key 使用。
pub trait ClientKeyVerifier: Send + Sync {
    fn verify_client_key(
        &self,
        plaintext: &str,
    ) -> Result<ClientApiKeyId, ClientAuthenticationError>;
}

/// 成功认证后的 API Key 使用事实接收器。
///
/// 认证仍是同步快照读取；实现必须自行异步、去重地持久化，不得阻塞客户端请求。
pub trait ClientApiKeyUsageSink: Send + Sync {
    fn record_used(&self, key_id: &ClientApiKeyId);
}

pub struct DefaultExecutionService {
    snapshots: RuntimeSnapshotHandle,
    /// probe 自身走 transient store，探测失败仍写入持久 store 的 ops_events。
    observations: Arc<dyn ExecutionStore>,
    providers: ProviderRegistry,
    admissions: Arc<dyn ClientAdmissionPort>,
    admission_waiting: ConcurrencyWaitQueue<ClientApiKeyId>,
    continuation: Arc<dyn NativeContinuationPort>,
    client_api_key_usage: Arc<dyn ClientApiKeyUsageSink>,
    budget: Option<Arc<dyn ClientBudgetPort>>,
    request_observers: Option<RequestObserverExtensionIndex>,
    request_policies: Option<RequestPolicyExtensionIndex>,
    middlewares: Option<MiddlewareExtensionIndex>,
    frontend_authentication: Option<FrontendAuthenticationExtensionIndex>,
    session_affinity: Option<Arc<dyn ProviderSessionAffinityPort>>,
    snapshot_refresh: Option<Arc<RuntimeSnapshotPublisher>>,
    active_requests: ActiveRequestRegistry,
}

impl DefaultExecutionService {
    #[must_use]
    pub fn new(
        snapshots: RuntimeSnapshotHandle,
        execution: Arc<dyn ExecutionStore>,
        providers: ProviderRegistry,
        admissions: Arc<dyn ClientAdmissionPort>,
        continuation: Arc<dyn NativeContinuationPort>,
        client_api_key_usage: Arc<dyn ClientApiKeyUsageSink>,
    ) -> Self {
        Self {
            snapshots,
            observations: execution,
            providers,
            admissions,
            admission_waiting: ConcurrencyWaitQueue::default(),
            continuation,
            client_api_key_usage,
            budget: None,
            request_observers: None,
            request_policies: None,
            middlewares: None,
            frontend_authentication: None,
            session_affinity: None,
            snapshot_refresh: None,
            active_requests: Arc::default(),
        }
    }

    #[must_use]
    pub fn with_budget(mut self, budget: Arc<dyn ClientBudgetPort>) -> Self {
        self.budget = Some(budget);
        self
    }

    #[must_use]
    pub fn with_request_observers(mut self, observers: RequestObserverExtensionIndex) -> Self {
        self.request_observers = Some(observers);
        self
    }

    #[must_use]
    pub fn with_request_policies(mut self, policies: RequestPolicyExtensionIndex) -> Self {
        self.request_policies = Some(policies);
        self
    }

    #[must_use]
    pub fn with_middlewares(mut self, middlewares: MiddlewareExtensionIndex) -> Self {
        self.middlewares = Some(middlewares);
        self
    }

    #[must_use]
    pub fn with_frontend_authentication(
        mut self,
        authentication: FrontendAuthenticationExtensionIndex,
    ) -> Self {
        self.frontend_authentication = Some(authentication);
        self
    }

    #[must_use]
    pub fn with_session_affinity(
        mut self,
        session_affinity: Arc<dyn ProviderSessionAffinityPort>,
    ) -> Self {
        self.session_affinity = Some(session_affinity);
        self
    }

    /// CLI command-plane 没有后台订阅；每次绑定显式身份前主动编译最新事实。
    #[must_use]
    pub fn with_snapshot_refresh(mut self, refresh: Arc<RuntimeSnapshotPublisher>) -> Self {
        self.snapshot_refresh = Some(refresh);
        self
    }

    fn authenticate_without_usage(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        let authentication = ClientAuthenticationRequest::bearer(plaintext)
            .map_err(|_| ClientAuthenticationError::InvalidKey)?;
        let snapshot = self
            .snapshots
            .acquire()
            .map_err(|_| ClientAuthenticationError::SnapshotUnavailable)?;
        let policy = snapshot
            .client_policies()
            .filter(|policy| {
                constant_time_equal(plaintext, policy.plaintext_key().expose_for_auth())
            })
            .find(|policy| policy.authorize().is_ok())
            .cloned()
            .ok_or(ClientAuthenticationError::InvalidKey)?;
        Ok(AuthenticatedClient {
            snapshot,
            policy,
            authentication: Some(authentication),
        })
    }

    async fn authenticate_request_without_usage(
        &self,
        authentication: ClientAuthenticationRequest,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        let snapshot = self
            .snapshots
            .acquire()
            .map_err(|_| ClientAuthenticationError::SnapshotUnavailable)?;
        let frontend = self
            .frontend_authentication
            .as_ref()
            .and_then(|index| snapshot.extensions().and_then(|set| index.resolve(set)));
        let policy = if let Some(frontend) = frontend {
            match frontend
                .authenticate(&authentication)
                .await
                .map_err(|_| ClientAuthenticationError::ProviderUnavailable)?
            {
                FrontendAuthenticationDecision::Authenticated { principal } => {
                    let id = frontend
                        .client_key_id(&principal)
                        .ok_or(ClientAuthenticationError::InvalidKey)?;
                    snapshot
                        .client_policy(&id)
                        .filter(|policy| policy.authorize().is_ok())
                        .cloned()
                        .ok_or(ClientAuthenticationError::InvalidKey)?
                }
                FrontendAuthenticationDecision::Rejected => {
                    return Err(ClientAuthenticationError::InvalidKey);
                }
                FrontendAuthenticationDecision::NotMatched if frontend.exclusive() => {
                    return Err(ClientAuthenticationError::InvalidKey);
                }
                FrontendAuthenticationDecision::NotMatched => {
                    native_policy(&snapshot, &authentication)?
                }
            }
        } else {
            native_policy(&snapshot, &authentication)?
        };
        Ok(AuthenticatedClient {
            snapshot,
            policy,
            authentication: Some(authentication),
        })
    }

    async fn start_inner(&self, request: StartExecution) -> Result<StartedExecution, GatewayError> {
        let StartExecution {
            client,
            public_model,
            operation,
            metadata,
        } = request;
        let prepared = self.prepare_root_execution_inner(client).await?;
        self.start_prepared_with_target(
            prepared,
            ExecutionTarget::Model(public_model),
            operation,
            metadata,
        )
        .await
    }

    async fn start_provider_endpoint_inner(
        &self,
        request: StartProviderExecution,
    ) -> Result<StartedExecution, GatewayError> {
        let StartProviderExecution {
            client,
            provider,
            upstream_model,
            operation,
            metadata,
        } = request;
        let prepared = self.prepare_root_execution_inner(client).await?;
        self.start_prepared_with_target(
            prepared,
            ExecutionTarget::ProviderEndpoint {
                provider,
                upstream_model,
            },
            operation,
            metadata,
        )
        .await
    }

    async fn prepare_root_execution_inner(
        &self,
        mut client: AuthenticatedClient,
    ) -> Result<PreparedRootExecution, GatewayError> {
        // 长连接每次执行都重新鉴权并冻结当前策略，确保限额和授权变更对新请求生效。
        let expected_key_id = client.policy.key_id().clone();
        let authentication = client.authentication.clone().ok_or_else(|| {
            GatewayError::new(
                GatewayErrorKind::Internal,
                "bound execution identity cannot enter the public start path",
            )
        })?;
        client = self
            .authenticate_request_without_usage(authentication)
            .await
            .and_then(|client| {
                (client.policy.key_id() == &expected_key_id)
                    .then_some(client)
                    .ok_or(ClientAuthenticationError::InvalidKey)
            })
            .map_err(authentication_gateway_error)?;
        self.client_api_key_usage
            .record_used(client.policy().key_id());
        PreparedRootExecution::new(client)
    }

    async fn start_prepared_with_target(
        &self,
        prepared: PreparedRootExecution,
        target: ExecutionTarget,
        operation: Operation,
        metadata: ExecutionRequestMetadata,
    ) -> Result<StartedExecution, GatewayError> {
        let PreparedRootExecution {
            client,
            request_id,
            started_at,
            deadline_at,
            cancellation,
            execution_effects,
            execution_effects_baseline,
        } = prepared;
        let authorization = AuthorizedExecution {
            account_scope: Arc::clone(client.policy.account_scope()),
            deadline_at,
            cancellation,
            extension_scope: ExtensionCallScope::default(),
            required_provider: None,
            required_account: None,
            nested: None,
            bound: None,
            graph: client
                .snapshot
                .extensions()
                .map(|_| Arc::new(NestedExecutionGraph::new(execution_effects))),
            execution_effects_baseline,
            nested_permit: None,
        };
        self.start_authorized(
            PendingStartExecution {
                client,
                target,
                operation,
                metadata,
            },
            authorization,
            started_at,
            Some(request_id),
        )
        .await
    }

    async fn start_authorized(
        &self,
        mut request: PendingStartExecution,
        mut authorization: AuthorizedExecution,
        started_at: SystemTime,
        request_id: Option<ModelRequestId>,
    ) -> Result<StartedExecution, GatewayError> {
        let request_id = request_id.map_or_else(new_request_id, Ok)?;
        if authorization.cancellation.is_cancelled() {
            return Err(GatewayError::new(
                GatewayErrorKind::Cancelled,
                "parent request was cancelled",
            ));
        }
        if authorization
            .deadline_at
            .duration_since(SystemTime::now())
            .unwrap_or_default()
            .is_zero()
        {
            return Err(GatewayError::new(
                GatewayErrorKind::Timeout,
                "request deadline elapsed",
            ));
        }
        let account_group_ids = authorization
            .account_scope
            .routing_snapshot()
            .groups_snapshot()
            .iter()
            .map(|group| group.id().clone())
            .collect::<Vec<_>>();
        let middleware = self.middlewares.as_ref().and_then(|middlewares| {
            let generation = request.client.snapshot.extensions()?.clone();
            middlewares.resolve(&generation)
        });
        let request_policy = self.request_policies.as_ref().and_then(|policies| {
            let generation = request.client.snapshot.extensions()?.clone();
            let plan = policies.resolve(&generation)?;
            Some(
                RequestPolicyContext::new(
                    plan,
                    generation,
                    request_id.clone(),
                    request.client.policy.key_id().clone(),
                    account_group_ids.clone(),
                )
                .with_extension_scope(authorization.extension_scope.clone())
                .with_execution_effects(Arc::clone(&authorization.graph.as_ref()?.effects)),
            )
        });
        // 路由插件可以调用 host.model/host.affinity；仅这条扩展路径必须在首次 RPC
        // 前取得父 Key 准入。无策略的原生路由仍保持“先路由、后准入”的既有顺序。
        let mut start_guard = None;
        let request_observation = self.request_observers.as_ref().and_then(|observers| {
            let generation = request.client.snapshot.extensions()?.clone();
            let plan = observers.resolve(&generation)?;
            Some(RequestObservationDispatch::new(
                plan,
                generation,
                FrozenRequestObservationContext::new(
                    request_id.clone(),
                    request.client.snapshot.revision(),
                    request.client.policy.key_id().clone(),
                    account_group_ids.clone(),
                    request.operation.kind(),
                    request.target.public_model().cloned(),
                    authorization.extension_scope.clone(),
                ),
            ))
        });
        let budget_key_id = request.client.policy.key_id().clone();
        let mut entered_execution = false;
        let result = async {
            if request_policy.is_some() {
                start_guard = Some(self.prepare_execution_start(&request, &request_id, &mut authorization).await?);
            }
            let mut routing_context = RoutingContext::default();
            if let Some(provider) = authorization.required_provider.clone() {
                if !authorization.account_scope.provider_kinds().contains(&provider) {
                    return Err(GatewayError::new(
                        GatewayErrorKind::PolicyDenied,
                        "nested provider is outside the frozen account scope",
                    ));
                }
                routing_context.required_provider = Some(provider);
            }
            let account_scope = Arc::clone(&authorization.account_scope);
            let plan = match &request.target {
                ExecutionTarget::ProviderEndpoint {
                    provider,
                    upstream_model,
                } => request.client.snapshot.plan_provider_endpoint(
                    provider,
                    upstream_model.as_ref(),
                    &request.operation,
                    account_scope,
                    &routing_context,
                ),
                ExecutionTarget::Model(public_model) => {
                    let mut target_model = public_model.clone();
                    let mut target_context = routing_context.clone();
                    if let Some(policy) = &request_policy {
                        let available = request
                            .client
                            .snapshot
                            .available_providers(&account_scope, &routing_context);
                        match policy
                            .route_model(request.operation.clone(), public_model.clone(), available)
                            .await
                            .map_err(|_| {
                                GatewayError::new(
                                    GatewayErrorKind::Internal,
                                    "model routing policy failed",
                                )
                            })? {
                            ModelRouteDecision::Unhandled => {}
                            ModelRouteDecision::Reject => {
                                return Err(GatewayError::new(
                                    GatewayErrorKind::PolicyDenied,
                                    "model routing policy rejected the request",
                                ));
                            }
                            ModelRouteDecision::Route { provider, model } => {
                                if provider.is_none() && model.is_none() {
                                    return Err(GatewayError::new(
                                        GatewayErrorKind::Internal,
                                        "model routing policy returned an empty target",
                                    ));
                                }
                                if let Some(provider) = provider {
                                    if authorization
                                        .required_provider
                                        .as_ref()
                                        .is_some_and(|required| required != &provider)
                                        || !authorization
                                            .account_scope
                                            .provider_kinds()
                                            .contains(&provider)
                                    {
                                        return Err(GatewayError::new(
                                            GatewayErrorKind::PolicyDenied,
                                            "model routing policy exceeded the frozen provider scope",
                                        ));
                                    }
                                    target_context.required_provider = Some(provider);
                                }
                                if let Some(model) = model {
                                    target_model = model;
                                }
                            }
                        }
                    }
                    request.client.snapshot.plan(
                        &target_model,
                        &request.operation,
                        account_scope,
                        &target_context,
                    )
                }
            }
            .map_err(map_routing_error)?;
            let continuation = match request.metadata.previous_response_id.as_ref() {
                Some(previous) => {
                    let resolve = self
                        .continuation
                        .resolve(request.client.policy.key_id(), previous)
                        .fuse();
                    let timeout = Delay::new(COORDINATION_TIMEOUT).fuse();
                    pin_mut!(resolve, timeout);
                    let pin = select_biased! {
                        result = resolve => match result {
                            Ok(pin) => pin,
                            Err(error)
                                if error.kind() == NativeContinuationStoreErrorKind::Unavailable =>
                            {
                                tracing::debug!(request_id = request_id.as_str(), %error, "Continuation affinity 查询失败，退化为外部续接");
                                None
                            }
                            Err(error)
                                if error.kind() == NativeContinuationStoreErrorKind::OwnershipMismatch =>
                            {
                                return Err(GatewayError::new(GatewayErrorKind::PolicyDenied, "continuation does not belong to this client API key"));
                            }
                            Err(error) => {
                                tracing::warn!(request_id = request_id.as_str(), %error, "Continuation affinity 记录无效，已拒绝续接");
                                return Err(GatewayError::new(GatewayErrorKind::Internal, "continuation state is invalid"));
                            }
                        },
                        _ = timeout => None,
                    };
                    Some(match pin {
                        Some(pin) if !pin.matches_client(request.client.policy.key_id()) => {
                            return Err(GatewayError::new(GatewayErrorKind::PolicyDenied, "continuation does not belong to this client API key"));
                        }
                        Some(pin) if !authorization.account_scope.allows(pin.account()) => {
                            return Err(GatewayError::new(GatewayErrorKind::PolicyDenied, "continuation account is outside the client account scope"));
                        }
                        Some(pin)
                            if !plan.candidates().iter().any(|candidate| candidate.provider() == pin.provider()) =>
                        {
                            return Err(GatewayError::new(GatewayErrorKind::NoAvailableProvider, "continuation provider is not available"));
                        }
                        Some(pin) => {
                            attach_continuation_session_state(&mut request.operation, &pin);
                            ContinuationBinding::Pinned(pin)
                        }
                        None => ContinuationBinding::External(previous.clone()),
                    })
                }
                None => None,
            };
            if start_guard.is_none() {
                start_guard = Some(
                    self.prepare_execution_start(&request, &request_id, &mut authorization)
                        .await?,
                );
            }
            let execution_effects = authorization
                .graph
                .as_ref()
                .map(|graph| Arc::clone(&graph.effects));
            let extensions =
                CoordinationExtensions::new(continuation, request_observation.clone())
                    .with_request_policy(request_policy)
                    .with_execution_effects(
                        execution_effects,
                        authorization.execution_effects_baseline,
                    )
                    .with_middleware(
                        middleware,
                        Arc::from(account_group_ids.clone()),
                        request.metadata.endpoint.clone(),
                        request.metadata.transport,
                    )
                    .with_extension_scope(authorization.extension_scope.clone());
            entered_execution = true;
            self.start_without_continuation(
                request,
                PreparedExecutionStart {
                    request_id: request_id.clone(),
                    started_at,
                    plan,
                    extensions,
                    authorization,
                    guard: start_guard
                        .take()
                        .expect("execution start guard was prepared"),
                },
            )
            .await
        }
        .await;
        if !entered_execution && let Err(error) = &result {
            tracing::warn!(
                request_id = request_id.as_str(),
                key_id = budget_key_id.as_str(),
                failure_kind = error.kind().as_str(),
                "请求在路由或准入阶段被拒绝"
            );
            let rejection = super::EntryRejection {
                request_id: request_id.clone(),
                client_key_id: budget_key_id.clone(),
                error: error.clone(),
                latency: started_at.elapsed().unwrap_or_default(),
            };
            let write = self.observations.record_entry_rejection(rejection).fuse();
            let timeout = Delay::new(COORDINATION_TIMEOUT).fuse();
            pin_mut!(write, timeout);
            select_biased! {
                result = write => { if result.is_err() { tracing::warn!("入口拒绝观测写入失败"); } },
                _ = timeout => tracing::warn!("入口拒绝观测写入超时"),
            }
        }
        if let (Some(observation), Err(error)) = (&request_observation, &result) {
            observation.reject(error);
        }
        if result.is_err()
            && let Some(guard) = start_guard.take()
        {
            guard
                .release_failed(self.budget.as_deref(), request_id, budget_key_id)
                .await;
        }
        result
    }

    async fn prepare_execution_start(
        &self,
        request: &PendingStartExecution,
        request_id: &ModelRequestId,
        authorization: &mut AuthorizedExecution,
    ) -> Result<ExecutionStartGuard, GatewayError> {
        let concurrency_wait_budget = ConcurrencyWaitBudget::default();
        let (admission, admission_decision_ms) = if authorization.nested.is_some() {
            let permit = authorization.nested_permit.take().ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::Internal,
                    "nested execution admission is unavailable",
                )
            })?;
            (ExecutionAdmission::Nested(permit), None)
        } else {
            let admission_started_at = Instant::now();
            let admission = self
                .acquire_client_admission(
                    &request.client,
                    request_id,
                    authorization.deadline_at,
                    &concurrency_wait_budget,
                )
                .await?;
            let admission = if let Some(permit) = authorization.nested_permit.take() {
                ExecutionAdmission::Bound(admission, permit)
            } else {
                ExecutionAdmission::Client(admission)
            };
            (admission, Some(duration_ms(admission_started_at.elapsed())))
        };
        if let Some(budget) = &self.budget
            && let Err(error) = budget.admit(request.client.policy.key_id().clone()).await
        {
            admission.release().await;
            return Err(error);
        }
        let active_request = authorization.graph.as_ref().map(|graph| {
            self.register_active_request(
                request_id.clone(),
                ActiveRequestAuthority {
                    client: request.client.clone(),
                    account_scope: Arc::clone(&authorization.account_scope),
                    deadline_at: authorization.deadline_at,
                    // 回收嵌套调用权限只取消子作用域，父请求仍需完成响应流终态加工。
                    cancellation: authorization.cancellation.child_token(),
                    extension_scope: authorization.extension_scope.clone(),
                    graph: Arc::clone(graph),
                },
            )
        });
        Ok(ExecutionStartGuard {
            admission,
            active_request,
            concurrency_wait_budget,
            admission_decision_ms,
        })
    }

    async fn start_without_continuation(
        &self,
        request: PendingStartExecution,
        prepared: PreparedExecutionStart,
    ) -> Result<StartedExecution, GatewayError> {
        let PreparedExecutionStart {
            request_id,
            started_at,
            plan,
            extensions,
            authorization,
            guard: start_guard,
        } = prepared;
        let PendingStartExecution {
            client,
            target,
            operation,
            metadata,
        } = request;
        let providers = self.providers.clone();
        let ExecutionStartGuard {
            admission,
            active_request,
            concurrency_wait_budget,
            admission_decision_ms,
        } = start_guard;
        let observation = plan
            .candidates()
            .first()
            .map_or_else(Default::default, |candidate| {
                providers.request_observation(
                    candidate.provider(),
                    &operation,
                    client.policy.key_id(),
                )
            });
        let (request_kind, subagent_kind) = if let Some(nested) = authorization.nested.as_ref() {
            (
                Some("plugin_child_model".to_owned()),
                Some(nested.initiating_plugin_instance_id.clone()),
            )
        } else if let Some(bound) = authorization.bound.as_ref() {
            (
                Some("plugin_bound_model".to_owned()),
                Some(bound.initiating_plugin_instance_id.clone()),
            )
        } else {
            (observation.request_kind, observation.subagent_kind)
        };
        if let Some(nested) = authorization.nested.as_ref() {
            tracing::debug!(
                parent_request_id = nested.parent_request_id.as_str(),
                child_request_id = request_id.as_str(),
                plugin_instance_id = nested.initiating_plugin_instance_id,
                depth = authorization.extension_scope.len(),
                "插件子模型请求已继承父请求身份与 deadline"
            );
        }
        let new_request = NewModelRequest {
            id: request_id.clone(),
            client_api_key_id: Some(client.policy.key_id().clone()),
            client_api_key_ref: client.policy.key_id().clone(),
            config_revision: plan.config_revision(),
            routing: authorization.account_scope.routing_snapshot(),
            protocol: metadata.protocol,
            operation: operation.kind(),
            endpoint: metadata.endpoint,
            client_transport: metadata.transport.as_str().to_owned(),
            requested_model: target.into_public_model().or(observation.requested_model),
            client_ip: metadata.client_ip,
            user_agent: metadata.user_agent,
            reasoning_effort: observation.reasoning_effort,
            reasoning_preset: observation.reasoning_preset,
            request_kind,
            subagent_kind,
            compact: observation.compact,
            continuation: observation.continuation,
            image_generation_requested: operation.image_generation_requested(),
            admission_decision_ms,
            started_at,
            deadline_at: authorization.deadline_at,
        };
        let coordinator =
            AttemptCoordinator::new(GatewayEngine::new(self.observations.clone(), providers));
        let core = match coordinator
            .start_observed(
                new_request,
                operation,
                plan,
                authorization.required_account,
                extensions,
                authorization.cancellation,
            )
            .await
        {
            Ok(core) => core.with_concurrency_wait_budget(concurrency_wait_budget),
            Err(error) => {
                if let Some(budget) = &self.budget {
                    settle_budget(
                        budget.as_ref(),
                        ClientBudgetCharge {
                            key_id: client.policy.key_id().clone(),
                            request_id: request_id.clone(),
                            amount_usd: crate::metering::Decimal::ZERO,
                            completed_at: SystemTime::now(),
                        },
                    )
                    .await;
                }
                if let Some(active_request) = active_request {
                    active_request.release();
                }
                admission.release().await;
                return Err(gateway_error_from_engine(&error));
            }
        };
        Ok(StartedExecution {
            request_id,
            created_at: started_at,
            stream: metadata.stream,
            session: Box::new(DefaultExecutionSession::new(
                core,
                admission,
                active_request,
                Arc::clone(&self.continuation),
                self.budget.clone(),
            )),
        })
    }

    fn register_active_request(
        &self,
        request_id: ModelRequestId,
        authority: ActiveRequestAuthority,
    ) -> ActiveRequestLease {
        let authority = Arc::new(authority);
        let mut active = self
            .active_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = active.insert(request_id.clone(), Arc::downgrade(&authority));
        debug_assert!(previous.is_none(), "model request IDs must be unique");
        drop(active);
        ActiveRequestLease {
            registry: Arc::clone(&self.active_requests),
            request_id,
            authority,
        }
    }

    fn active_request(
        &self,
        request_id: &ModelRequestId,
    ) -> Result<Arc<ActiveRequestAuthority>, GatewayError> {
        let authority = self
            .active_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(request_id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::PolicyDenied,
                    "parent model request is not active",
                )
            })?;
        if authority.cancellation.is_cancelled() {
            return Err(GatewayError::new(
                GatewayErrorKind::Cancelled,
                "parent request was cancelled",
            ));
        }
        if authority
            .deadline_at
            .duration_since(SystemTime::now())
            .unwrap_or_default()
            .is_zero()
        {
            return Err(GatewayError::new(
                GatewayErrorKind::Timeout,
                "parent request deadline elapsed",
            ));
        }
        Ok(authority)
    }

    async fn start_nested_inner(
        &self,
        request: NestedModelExecutionRequest,
    ) -> Result<StartedExecution, GatewayError> {
        let NestedModelExecutionRequest {
            parent_request_id,
            initiating_plugin_instance_id,
            public_model,
            operation,
            mut metadata,
            provider,
            account,
            parent_account,
        } = request;
        let parent = self.active_request(&parent_request_id)?;
        if parent.extension_scope.len() >= MAX_NESTED_DEPTH {
            return Err(GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "nested model execution depth is exhausted",
            ));
        }
        let extension_scope = parent
            .extension_scope
            .extending(initiating_plugin_instance_id.clone())
            .ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::PolicyDenied,
                    "recursive plugin model execution is not allowed",
                )
            })?;
        let (account_scope, required_provider) = restrict_model_execution_scope(
            parent.account_scope.as_ref(),
            provider,
            account.as_ref(),
            parent_account.as_ref(),
        )?;
        let nested_permit = parent.graph.acquire()?;
        // Core.start 之后的路由/Provider 可以继续调用外部系统；在进入子执行前即按
        // 可能已出站记水位，不能让父 attempt 随后的 `not_sent` 触发重放。
        parent.graph.effects.observe();
        let execution_effects_baseline = parent.graph.effects.epoch();
        metadata.endpoint = "host.model".to_owned();
        metadata.transport = ClientTransport::InternalPlugin;
        metadata.client_ip = None;
        metadata.user_agent = None;
        let authorization = AuthorizedExecution {
            account_scope,
            deadline_at: parent.deadline_at,
            cancellation: parent.cancellation.child_token(),
            extension_scope,
            required_provider,
            required_account: account,
            nested: Some(NestedExecutionFacts {
                parent_request_id,
                initiating_plugin_instance_id,
            }),
            bound: None,
            graph: Some(Arc::clone(&parent.graph)),
            execution_effects_baseline,
            nested_permit: Some(nested_permit),
        };
        self.start_authorized(
            PendingStartExecution {
                client: parent.client.clone(),
                target: ExecutionTarget::Model(public_model),
                operation,
                metadata,
            },
            authorization,
            SystemTime::now(),
            None,
        )
        .await
    }

    async fn client_for_key_id(
        &self,
        client_key_id: &ClientApiKeyId,
    ) -> Result<AuthenticatedClient, GatewayError> {
        if let Some(refresh) = &self.snapshot_refresh {
            refresh.refresh().await.map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::Internal,
                    "current runtime snapshot is unavailable",
                )
            })?;
        }
        let snapshot = self.snapshots.acquire().map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::Internal,
                "current runtime snapshot is unavailable",
            )
        })?;
        let policy = snapshot
            .client_policy(client_key_id)
            .cloned()
            .ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::Unauthorized,
                    "client API key no longer exists",
                )
            })?;
        policy.authorize().map_err(|_| {
            GatewayError::new(GatewayErrorKind::PolicyDenied, "client API key is disabled")
        })?;
        Ok(AuthenticatedClient {
            snapshot,
            policy,
            authentication: None,
        })
    }

    async fn bind_bound_model_inner(
        &self,
        binding: BoundModelExecutionBinding,
    ) -> Result<BoundModelExecutionContext, GatewayError> {
        if binding.timeout.is_zero() {
            return Err(GatewayError::new(
                GatewayErrorKind::InvalidRequest,
                "bound model execution timeout is invalid",
            ));
        }
        let client = self.client_for_key_id(&binding.client_key_id).await?;
        let deadline_at = SystemTime::now()
            .checked_add(binding.timeout.min(MODEL_REQUEST_DEADLINE))
            .ok_or_else(|| {
                GatewayError::new(GatewayErrorKind::Internal, "system clock is invalid")
            })?;
        if binding.extension_scope.len() >= MAX_NESTED_DEPTH {
            return Err(GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "plugin model execution depth exceeded",
            ));
        }
        let extension_scope = binding
            .extension_scope
            .extending(binding.initiating_plugin_instance_id.clone())
            .ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::Internal,
                    "bound plugin execution scope is invalid",
                )
            })?;
        let account_scope = Arc::clone(client.policy.account_scope());
        Ok(BoundModelExecutionContext {
            authority: Arc::new(BoundModelExecutionAuthority {
                client,
                account_scope,
                deadline_at,
                cancellation: binding.cancellation,
                extension_scope,
                graph: Arc::new(NestedExecutionGraph::new(Arc::new(
                    ExecutionEffects::default(),
                ))),
                initiating_plugin_instance_id: binding.initiating_plugin_instance_id,
            }),
        })
    }

    async fn start_bound_model_inner(
        &self,
        request: BoundModelExecutionRequest,
    ) -> Result<StartedExecution, GatewayError> {
        let BoundModelExecutionRequest {
            context,
            public_model,
            operation,
            mut metadata,
            provider,
            account,
        } = request;
        let authority = &context.authority;
        if authority.cancellation.is_cancelled() {
            return Err(GatewayError::new(
                GatewayErrorKind::Cancelled,
                "bound plugin invocation was cancelled",
            ));
        }
        if authority
            .deadline_at
            .duration_since(SystemTime::now())
            .unwrap_or_default()
            .is_zero()
        {
            return Err(GatewayError::new(
                GatewayErrorKind::Timeout,
                "bound plugin invocation deadline elapsed",
            ));
        }
        let (account_scope, required_provider) = restrict_model_execution_scope(
            authority.account_scope.as_ref(),
            provider,
            account.as_ref(),
            None,
        )?;
        let permit = authority.graph.acquire()?;
        let execution_effects_baseline = authority.graph.effects.epoch();
        metadata.endpoint = "host.model".to_owned();
        metadata.transport = ClientTransport::InternalPlugin;
        metadata.client_ip = None;
        metadata.user_agent = None;
        self.client_api_key_usage
            .record_used(authority.client.policy.key_id());
        self.start_authorized(
            PendingStartExecution {
                client: authority.client.clone(),
                target: ExecutionTarget::Model(public_model),
                operation,
                metadata,
            },
            AuthorizedExecution {
                account_scope,
                deadline_at: authority.deadline_at,
                cancellation: authority.cancellation.child_token(),
                extension_scope: authority.extension_scope.clone(),
                required_provider,
                required_account: account,
                nested: None,
                bound: Some(BoundExecutionFacts {
                    initiating_plugin_instance_id: authority.initiating_plugin_instance_id.clone(),
                }),
                graph: Some(Arc::clone(&authority.graph)),
                execution_effects_baseline,
                nested_permit: Some(permit),
            },
            SystemTime::now(),
            None,
        )
        .await
    }

    async fn lookup_affinity_inner(
        &self,
        request: AffinityLookupRequest,
    ) -> Result<Option<AffinityLookupResult>, GatewayError> {
        let AffinityLookupRequest {
            parent_request_id,
            provider,
            key,
        } = request;
        let parent = self.active_request(&parent_request_id)?;
        if !parent.account_scope.provider_kinds().contains(&provider) {
            return Err(GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "affinity provider is outside the frozen authorization scope",
            ));
        }
        let affinity = self.session_affinity.as_ref().ok_or_else(|| {
            GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "provider session affinity is unavailable",
            )
        })?;
        let account = {
            let load = affinity.load(&provider, &key).fuse();
            let timeout = Delay::new(COORDINATION_TIMEOUT).fuse();
            pin_mut!(load, timeout);
            select_biased! {
                result = load => result.map_err(|error| match error.kind() {
                    ProviderStoreErrorKind::Unavailable => GatewayError::new(
                        GatewayErrorKind::ProviderInfrastructureUnavailable,
                        "provider session affinity is temporarily unavailable",
                    ),
                    ProviderStoreErrorKind::InvalidData | ProviderStoreErrorKind::Conflict => {
                        GatewayError::new(GatewayErrorKind::Internal, "provider session affinity is invalid")
                    }
                })?,
                _ = timeout => return Err(GatewayError::new(
                    GatewayErrorKind::ProviderInfrastructureUnavailable,
                    "provider session affinity lookup timed out",
                )),
            }
        };
        let Some(account) = account else {
            return Ok(None);
        };
        if !parent.account_scope.allows(&account)
            || parent.account_scope.account_provider(&account) != Some(&provider)
        {
            return Ok(None);
        }
        Ok(Some(AffinityLookupResult::new(provider, account)))
    }

    async fn acquire_client_admission(
        &self,
        client: &AuthenticatedClient,
        request_id: &ModelRequestId,
        deadline_at: SystemTime,
        budget: &ConcurrencyWaitBudget,
    ) -> Result<AdmissionLease, GatewayError> {
        let policy = client.snapshot.client_queue_policy();
        let limits = client.policy.limits();
        let key = client.policy.key_id();
        let mut waiting = CapacityWait::new(&self.admission_waiting, policy, deadline_at, budget);
        let mut admission = AdmissionLease {
            port: Arc::clone(&self.admissions),
            client_api_key_id: key.clone(),
            model_request_id: request_id.clone(),
            armed: false,
        };
        loop {
            let remaining = deadline_at
                .duration_since(SystemTime::now())
                .unwrap_or_default();
            if remaining.is_zero() {
                return Err(GatewayError::new(
                    GatewayErrorKind::Timeout,
                    "request deadline elapsed",
                ));
            }
            // 在发送原子准入之前接管取消清理，覆盖 Redis 已取得租约但返回尚未被观察的窗口。
            admission.armed = true;
            let acquire = self
                .admissions
                .admit(ClientAdmissionRequest {
                    model_request_id: request_id.clone(),
                    client_api_key_id: key.clone(),
                    lease_ttl: remaining,
                    allow_concurrency_acquire: limits.max_concurrency == 0 || waiting.can_try(key),
                    limits,
                })
                .fuse();
            let timeout = Delay::new(remaining).fuse();
            pin_mut!(acquire, timeout);
            let decision = select_biased! {
                result = acquire => result.map_err(|_| GatewayError::new(GatewayErrorKind::NoAvailableProvider, "request admission is temporarily unavailable"))?,
                _ = timeout => return Err(GatewayError::new(GatewayErrorKind::Timeout, "request deadline elapsed")),
            };
            match decision {
                ClientAdmissionDecision::Granted => {
                    if !waiting.elapsed().is_zero() {
                        tracing::info!(
                            request_id = request_id.as_str(),
                            queue_layer = "client_key",
                            queue_wait_ms = duration_ms(waiting.elapsed()),
                            "排队请求已取得 Key 并发槽位"
                        );
                    }
                    return Ok(admission);
                }
                ClientAdmissionDecision::Rejected(reason) => {
                    admission.armed = false;
                    if reason == ClientAdmissionRejection::RateLimited || policy.max_waiting == 0 {
                        return Err(GatewayError::new(
                            GatewayErrorKind::RateLimited,
                            "request exceeds client API key limits",
                        ));
                    }
                    if waiting.elapsed().is_zero()
                        && let Some(budget) = &self.budget
                    {
                        budget.admit(key.clone()).await?;
                    }
                    waiting.wait(std::slice::from_ref(key)).await.map_err(|error| {
                        tracing::info!(request_id = request_id.as_str(), queue_layer = "client_key", queue_wait_ms = duration_ms(waiting.elapsed()), reason = %error, "Key 排队请求被拒绝");
                        error.gateway_error()
                    })?;
                }
            }
        }
    }

    async fn probe_inner(
        &self,
        request: AccountProbeRequest,
        frozen: Option<Arc<crate::routing::RuntimeSnapshot>>,
    ) -> Result<AccountProbeResult, AccountProbeError> {
        let AccountProbeRequest {
            account_id,
            provider_kind,
            upstream_model,
            operation,
        } = request;
        let observed = ProbeObservation {
            provider_kind: provider_kind.clone(),
            account_id: account_id.clone(),
            upstream_model: upstream_model.clone(),
        };
        let snapshot = frozen
            .map_or_else(|| self.snapshots.acquire(), Ok)
            .map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::Internal,
                    "runtime snapshot is unavailable",
                )
            })?;
        let public_model =
            PublicModelId::new(upstream_model.as_str().to_owned()).map_err(|_| {
                GatewayError::new(GatewayErrorKind::Unsupported, "requested model is invalid")
            })?;
        let routing_context = RoutingContext {
            required_provider: Some(provider_kind),
            ..RoutingContext::default()
        };
        let plan = snapshot
            .plan_diagnostic(&public_model, &operation, &routing_context)
            .map_err(map_routing_error)?;
        let started_at = SystemTime::now();
        let deadline_at = started_at
            .checked_add(MODEL_REQUEST_DEADLINE)
            .ok_or_else(|| {
                GatewayError::new(GatewayErrorKind::Internal, "system clock is invalid")
            })?;
        let request_id = new_request_id()?;
        let actor = ClientApiKeyId::new("admin_connection_test")
            .map_err(|_| GatewayError::new(GatewayErrorKind::Internal, "invalid admin actor"))?;
        let new_request = NewModelRequest {
            id: request_id,
            client_api_key_id: None,
            client_api_key_ref: actor,
            config_revision: plan.config_revision(),
            routing: crate::routing::AccountRoutingSnapshot::all(),
            protocol: "admin_connection_test".to_owned(),
            operation: operation.kind(),
            endpoint: "/api/admin/accounts/connection-test".to_owned(),
            client_transport: ClientTransport::InternalProbe.as_str().to_owned(),
            requested_model: Some(public_model),
            client_ip: None,
            user_agent: None,
            reasoning_effort: None,
            reasoning_preset: None,
            request_kind: Some("account_connection_test".to_owned()),
            subagent_kind: None,
            compact: false,
            continuation: Default::default(),
            image_generation_requested: false,
            admission_decision_ms: None,
            started_at,
            deadline_at,
        };
        let providers = self.providers.clone();
        let transient: Arc<dyn ExecutionStore> = Arc::new(TransientExecutionStore);
        let coordinator = AttemptCoordinator::new(GatewayEngine::new(transient, providers));
        let mut session = match coordinator
            .start_diagnostic(
                new_request,
                operation,
                plan,
                account_id,
                None,
                CancellationToken::new(),
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                return Err(self
                    .observe_probe_failure(&observed, started_at, &error)
                    .await);
            }
        };
        let events = session.collect_uncommitted().await;
        let events = match events {
            Ok(events) => events,
            Err(error) => {
                return Err(self
                    .observe_probe_failure(&observed, started_at, &error)
                    .await);
            }
        };
        if let Err(error) = session.commit_downstream(Some(200)).await {
            return Err(self
                .observe_probe_failure(&observed, started_at, &error)
                .await);
        }
        Ok(AccountProbeResult {
            text: events
                .into_iter()
                .flat_map(|event| event.into_parts().0)
                .filter_map(|fact| match fact {
                    GatewayEvent::TextDelta(delta) => Some(delta.text),
                    _ => None,
                })
                .collect(),
        })
    }

    /// 探测失败先记录脱敏分类事实，再把请求局部的原始上游响应交给认证管理端。
    async fn observe_probe_failure(
        &self,
        observed: &ProbeObservation,
        started_at: SystemTime,
        error: &EngineError,
    ) -> AccountProbeError {
        let (source, send_state, upstream_response) = match error {
            EngineError::Provider(provider_error) => {
                let upstream_response = provider_error
                    .client_visible_upstream_response()
                    .map(AccountProbeUpstreamResponse::from_client_response);
                let has_upstream_facts = provider_error.send_state() != UpstreamSendState::NotSent
                    || provider_error.upstream_status().is_some()
                    || provider_error.upstream_code().is_some()
                    || provider_error.client_visible_upstream_error().is_some()
                    || upstream_response.is_some();
                let source = if has_upstream_facts {
                    AccountProbeErrorSource::Upstream
                } else {
                    AccountProbeErrorSource::Provider
                };
                (source, Some(provider_error.send_state()), upstream_response)
            }
            _ => (AccountProbeErrorSource::Gateway, None, None),
        };
        if let EngineError::Provider(provider_error) = error {
            let latency = started_at.elapsed().unwrap_or_default();
            let latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
            tracing::warn!(
                target: "gateway_probe",
                provider_kind = observed.provider_kind.as_str(),
                account_id = observed.account_id.as_str(),
                upstream_model = observed.upstream_model.as_str(),
                failure_kind = provider_error.kind().as_str(),
                send_state = ?provider_error.send_state(),
                upstream_status = ?provider_error.upstream_status(),
                provider_error_code = ?provider_error.upstream_code().map(|code| code.as_str()),
                latency_ms,
                "账号连接测试失败"
            );
            if let Err(store_error) = self
                .observations
                .record_probe_failure(ProbeFailure {
                    provider_kind: observed.provider_kind.clone(),
                    account_id: observed.account_id.clone(),
                    upstream_model_id: observed.upstream_model.clone(),
                    error: provider_error.clone(),
                    latency,
                })
                .await
            {
                tracing::warn!(
                    operation = "record_probe_failure",
                    provider_kind = observed.provider_kind.as_str(),
                    account_id = observed.account_id.as_str(),
                    error_kind = ?store_error.kind(),
                    "账号连接测试观测写入失败，测试结果不受影响"
                );
            }
        }
        AccountProbeError::new(
            gateway_error_from_engine(error),
            source,
            send_state,
            upstream_response,
        )
    }
}

struct ProbeObservation {
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
    upstream_model: UpstreamModelId,
}

struct TransientExecutionStore;

#[async_trait::async_trait]
impl ExecutionStore for TransientExecutionStore {
    async fn create_model_request(&self, _: NewModelRequest) -> Result<(), StoreError> {
        Ok(())
    }

    async fn record_attempt(&self, _: AttemptRecord) -> Result<(), StoreError> {
        Ok(())
    }

    async fn mark_send_state(
        &self,
        _: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    async fn mark_downstream_committed(
        &self,
        _: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    async fn record_client_status(&self, _: &ModelRequestId, _: u16) -> Result<(), StoreError> {
        Ok(())
    }

    async fn record_intermediate_failure(&self, _: IntermediateFailure) -> Result<(), StoreError> {
        Ok(())
    }

    async fn finalize_model_request(&self, _: ModelRequestFinalization) -> Result<(), StoreError> {
        Ok(())
    }

    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        Ok(RecoveryReport::default())
    }
}

impl ExecutionService for DefaultExecutionService {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        let client = self.authenticate_without_usage(plaintext)?;
        self.client_api_key_usage
            .record_used(client.policy().key_id());
        Ok(client)
    }

    fn authenticate_request(
        &self,
        request: ClientAuthenticationRequest,
    ) -> BoxFuture<'_, Result<AuthenticatedClient, ClientAuthenticationError>> {
        Box::pin(async move {
            let client = self.authenticate_request_without_usage(request).await?;
            self.client_api_key_usage
                .record_used(client.policy().key_id());
            Ok(client)
        })
    }

    fn verify_request(
        &self,
        request: ClientAuthenticationRequest,
    ) -> BoxFuture<'_, Result<AuthenticatedClient, ClientAuthenticationError>> {
        Box::pin(self.authenticate_request_without_usage(request))
    }

    fn public_models(&self, client: &AuthenticatedClient) -> Vec<PublicModelId> {
        client
            .snapshot
            .public_models_for_scope(client.policy.account_scope())
    }

    fn client_model_catalog<'a>(
        &'a self,
        client: &'a AuthenticatedClient,
        protocol: &'a str,
        client_version: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PublicModelDescriptor>, ProviderCatalogUnavailable>> {
        Box::pin(async move {
            let scope = client.policy.account_scope();
            let providers = &self.providers;
            let mut result = Vec::new();
            let mut seen = BTreeSet::new();
            for kind in scope.provider_kinds() {
                let provider = providers.get(kind).ok_or(ProviderCatalogUnavailable)?;
                let Some(models) = provider
                    .query_client_model_catalog(scope, protocol, client_version)
                    .await?
                else {
                    for profile in client.snapshot.public_model_profiles_for_provider(kind) {
                        if !client.snapshot.catalog_model_allowed_for_scope(
                            kind,
                            profile.model(),
                            scope,
                        ) {
                            continue;
                        }
                        if seen.insert(profile.model().clone()) {
                            result.push(PublicModelDescriptor::Adapted(profile));
                        }
                    }
                    continue;
                };
                // 保留上游顺序；映射只选择完整的目标对象，不能跨账号/模型混拼字段。
                let by_id = models
                    .iter()
                    .map(|entry| (entry.model.as_str(), entry))
                    .collect::<std::collections::BTreeMap<_, _>>();
                for entry in &models {
                    let target = client.snapshot.mapped_model(entry.model.as_str());
                    if !scope.allows_provider_model(kind, &target) {
                        continue;
                    }
                    let Some(source) = by_id.get(target.as_str()) else {
                        continue;
                    };
                    let model = PublicModelId::new(entry.model.as_str().to_owned())
                        .map_err(|_| ProviderCatalogUnavailable)?;
                    // 原生目录可能比路由快照更新，不能向客户端公布当前已知不可路由的模型。
                    if !client
                        .snapshot
                        .contains_public_model_for_provider(&model, kind)
                    {
                        continue;
                    }
                    if seen.insert(model.clone()) {
                        result.push(source.content.for_public_model(model));
                    }
                }
                for model in client.snapshot.public_models_for_provider(kind) {
                    let target = client.snapshot.mapped_model(model.as_str());
                    if !scope.allows_provider_model(kind, &target) {
                        continue;
                    }
                    if target == model.as_str() || seen.contains(&model) {
                        continue;
                    }
                    if !client
                        .snapshot
                        .contains_public_model_for_provider(&model, kind)
                    {
                        continue;
                    }
                    if let Some(source) = by_id.get(target.as_str()) {
                        seen.insert(model.clone());
                        result.push(source.content.for_public_model(model));
                    }
                }
            }
            Ok(result)
        })
    }

    fn contains_public_model(&self, client: &AuthenticatedClient, model: &PublicModelId) -> bool {
        client
            .snapshot
            .contains_public_model_for_scope(model, client.policy.account_scope())
    }

    fn prepare_execution(
        &self,
        client: AuthenticatedClient,
    ) -> BoxFuture<'_, Result<PreparedRootExecution, GatewayError>> {
        Box::pin(async move { self.prepare_root_execution_inner(client).await })
    }

    fn middleware_plan(&self, prepared: &PreparedRootExecution) -> Option<FrozenMiddlewarePlan> {
        let generation = prepared.client.snapshot.extensions()?.clone();
        self.middlewares.as_ref()?.resolve(&generation)
    }

    fn prepare_plugin_execution(
        &self,
        client_key_id: &ClientApiKeyId,
    ) -> BoxFuture<'_, Result<PreparedRootExecution, GatewayError>> {
        let client_key_id = client_key_id.clone();
        Box::pin(async move {
            let client = self.client_for_key_id(&client_key_id).await?;
            self.client_api_key_usage
                .record_used(client.policy.key_id());
            PreparedRootExecution::new(client)
        })
    }

    fn start_prepared(
        &self,
        prepared: PreparedRootExecution,
        request: PreparedExecutionRequest,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            self.start_prepared_with_target(
                prepared,
                ExecutionTarget::Model(request.public_model),
                request.operation,
                request.metadata,
            )
            .await
        })
    }

    fn start_prepared_provider_endpoint(
        &self,
        prepared: PreparedRootExecution,
        provider: ProviderKind,
        upstream_model: Option<UpstreamModelId>,
        operation: Operation,
        metadata: ExecutionRequestMetadata,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            self.start_prepared_with_target(
                prepared,
                ExecutionTarget::ProviderEndpoint {
                    provider,
                    upstream_model,
                },
                operation,
                metadata,
            )
            .await
        })
    }

    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move { self.start_inner(request).await })
    }

    fn start_provider_endpoint(
        &self,
        request: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move { self.start_provider_endpoint_inner(request).await })
    }
}

impl NestedModelExecutionPort for DefaultExecutionService {
    fn models(
        &self,
        context: BoundModelExecutionContext,
        protocol: String,
        client_version: String,
    ) -> BoxFuture<'_, Result<Vec<PublicModelId>, GatewayError>> {
        Box::pin(async move {
            let authority = &context.authority;
            let remaining = authority
                .deadline_at
                .duration_since(SystemTime::now())
                .unwrap_or_default();
            let cancellation = authority.cancellation.cancelled().fuse();
            let timeout = Delay::new(remaining).fuse();
            let catalog = self
                .client_model_catalog(&authority.client, &protocol, &client_version)
                .fuse();
            pin_mut!(cancellation, timeout, catalog);
            let models = select_biased! {
                () = cancellation => return Err(GatewayError::new(GatewayErrorKind::Cancelled, "model catalog call was cancelled")),
                () = timeout => return Err(GatewayError::new(GatewayErrorKind::Timeout, "model catalog deadline elapsed")),
                result = catalog => result.map_err(|_| GatewayError::new(GatewayErrorKind::Internal, "model catalog is unavailable"))?,
            };
            Ok(models
                .into_iter()
                .map(|model| match model {
                    PublicModelDescriptor::Native { model, .. } => model,
                    PublicModelDescriptor::Adapted(profile) => profile.model().clone(),
                })
                .collect())
        })
    }

    fn bind(
        &self,
        binding: BoundModelExecutionBinding,
    ) -> BoxFuture<'_, Result<BoundModelExecutionContext, GatewayError>> {
        Box::pin(async move { self.bind_bound_model_inner(binding).await })
    }

    fn start(
        &self,
        request: NestedModelExecutionRequest,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move { self.start_nested_inner(request).await })
    }

    fn start_bound(
        &self,
        request: BoundModelExecutionRequest,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move { self.start_bound_model_inner(request).await })
    }
}

impl AffinityLookupPort for DefaultExecutionService {
    fn lookup(
        &self,
        request: AffinityLookupRequest,
    ) -> BoxFuture<'_, Result<Option<AffinityLookupResult>, GatewayError>> {
        Box::pin(async move { self.lookup_affinity_inner(request).await })
    }
}

impl ClientKeyVerifier for DefaultExecutionService {
    fn verify_client_key(
        &self,
        plaintext: &str,
    ) -> Result<ClientApiKeyId, ClientAuthenticationError> {
        self.authenticate_without_usage(plaintext)
            .map(|client| client.policy().key_id().clone())
    }
}

impl AccountProbe for DefaultExecutionService {
    fn probe(
        &self,
        request: AccountProbeRequest,
        snapshot: Option<Arc<crate::routing::RuntimeSnapshot>>,
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
        Box::pin(async move { self.probe_inner(request, snapshot).await })
    }
}

struct AdmissionLease {
    armed: bool,
    port: Arc<dyn ClientAdmissionPort>,
    client_api_key_id: ClientApiKeyId,
    model_request_id: ModelRequestId,
}

async fn settle_budget(port: &dyn ClientBudgetPort, charge: ClientBudgetCharge) {
    if let Err(error) = port.settle(charge).await {
        tracing::error!(%error, "Client budget settlement failed; storage will retry on the next request");
    }
}

impl AdmissionLease {
    async fn release(mut self) {
        if let Err(error) = self
            .port
            .release(&self.client_api_key_id, &self.model_request_id)
            .await
        {
            tracing::warn!(%error, "Client admission 释放失败，依赖租约 TTL 收敛");
        }
        self.armed = false;
    }
}

impl Drop for AdmissionLease {
    fn drop(&mut self) {
        if self.armed {
            self.port
                .abandon(&self.client_api_key_id, &self.model_request_id);
        }
    }
}

enum ExecutionAdmission {
    Client(AdmissionLease),
    Nested(NestedExecutionPermit),
    Bound(AdmissionLease, NestedExecutionPermit),
}

impl ExecutionAdmission {
    async fn release(self) {
        match self {
            Self::Client(admission) => admission.release().await,
            Self::Nested(permit) => permit.release(),
            Self::Bound(admission, permit) => {
                admission.release().await;
                permit.release();
            }
        }
    }
}

impl ExecutionStartGuard {
    async fn release_failed(
        self,
        budget: Option<&dyn ClientBudgetPort>,
        request_id: ModelRequestId,
        key_id: ClientApiKeyId,
    ) {
        if let Some(active_request) = self.active_request {
            active_request.release();
        }
        if let Some(budget) = budget {
            settle_budget(
                budget,
                ClientBudgetCharge {
                    key_id,
                    request_id,
                    amount_usd: crate::metering::Decimal::ZERO,
                    completed_at: SystemTime::now(),
                },
            )
            .await;
        }
        self.admission.release().await;
    }
}

struct DefaultExecutionSession {
    core: ResponseExecutionSession<dyn ExecutionStore>,
    admission: Option<ExecutionAdmission>,
    active_request: Option<ActiveRequestLease>,
    cleanup: Option<BoxFuture<'static, ()>>,
    continuation: Arc<dyn NativeContinuationPort>,
    continuation_recorded: bool,
    budget: Option<Arc<dyn ClientBudgetPort>>,
}

impl DefaultExecutionSession {
    fn new(
        core: ResponseExecutionSession<dyn ExecutionStore>,
        admission: ExecutionAdmission,
        active_request: Option<ActiveRequestLease>,
        continuation: Arc<dyn NativeContinuationPort>,
        budget: Option<Arc<dyn ClientBudgetPort>>,
    ) -> Self {
        Self {
            core,
            admission: Some(admission),
            active_request,
            cleanup: None,
            continuation,
            continuation_recorded: false,
            budget,
        }
    }

    async fn settle_if_finalized(&mut self) {
        if self.core.is_finalized()
            && let Some(admission) = self.admission.take()
        {
            if let Some(active_request) = self.active_request.take() {
                active_request.release();
            }
            let budget = self.budget.take();
            let charge = self.core.budget_charge();
            // 在首次 await 前把完整清理责任留在会话内。事件等待被取消后，后续 poll
            // 或 detach 继续同一个 future，既不丢失费用，也不重启已完成的结算。
            self.cleanup = Some(Box::pin(async move {
                if let Some(budget) = budget {
                    settle_budget(budget.as_ref(), charge).await;
                }
                admission.release().await;
            }));
        }
        if let Some(cleanup) = self.cleanup.as_mut() {
            cleanup.await;
            self.cleanup = None;
        }
    }

    async fn record_continuation(&mut self, state: Option<&ProviderSessionState>) {
        if self.continuation_recorded {
            return;
        }
        let Some(state) = state else {
            return;
        };
        let Some(pin) = self.core.native_continuation_pin(state) else {
            return;
        };
        self.continuation_recorded = true;
        record_native_continuation(self.continuation.as_ref(), pin).await;
    }

    async fn finalize_detached(&mut self) {
        if let Err(error) = self.core.cancel_and_finalize().await {
            tracing::warn!(%error, "Detached execution 终态收敛失败");
        }
        self.settle_if_finalized().await;
    }
}

impl Drop for DefaultExecutionSession {
    fn drop(&mut self) {
        self.core.cancel();
        drop(self.active_request.take());
    }
}

impl ExecutionSession for DefaultExecutionSession {
    fn trace(&self) -> crate::diagnostics::TraceContext {
        self.core.trace()
    }
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            let result = self.core.next_event().await;
            if let Ok(Some(event)) = result.as_ref() {
                self.record_continuation(event.session_update()).await;
            }
            self.settle_if_finalized().await;
            result
        })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async move {
            let result = self.core.collect_uncommitted().await;
            if let Ok(events) = result.as_ref() {
                let state = events.iter().find_map(ProviderEvent::session_update);
                self.record_continuation(state).await;
            }
            self.settle_if_finalized().await;
            result
        })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        self.core.response_headers()
    }

    fn response_status_code(&self) -> Option<u16> {
        self.core.response_status_code()
    }

    fn discard_pending_delivery(&mut self) -> Result<(), EngineError> {
        self.core.discard_pending_delivery()
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            let result = self.core.commit_downstream(client_status_code).await;
            self.settle_if_finalized().await;
            result
        })
    }

    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            let result = self.core.record_client_status(client_status_code).await;
            self.settle_if_finalized().await;
            result
        })
    }

    fn is_finalized(&self) -> bool {
        self.core.is_finalized()
            && self.admission.is_none()
            && self.active_request.is_none()
            && self.cleanup.is_none()
    }

    fn cancel(&self) {
        self.core.cancel();
    }

    fn detach_finalize(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { self.finalize_detached().await })
    }
}

async fn record_native_continuation(
    continuation: &dyn NativeContinuationPort,
    pin: NativeContinuationPin,
) {
    let provider = pin.provider().as_str().to_owned();
    let account = pin.account().as_str().to_owned();
    let record = continuation.record(pin).fuse();
    let timeout = Delay::new(COORDINATION_TIMEOUT).fuse();
    pin_mut!(record, timeout);
    select_biased! {
        result = record => {
            if let Err(error) = result {
                tracing::warn!(
                    provider = %provider,
                    account = %account,
                    %error,
                    "Continuation affinity 写入失败，后续请求将退化为外部续接"
                );
            }
        },
        _ = timeout => {
            tracing::warn!(
                provider = %provider,
                account = %account,
                "Continuation affinity 后台写入超时，已丢弃本次亲和记录"
            );
        },
    }
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn restrict_model_execution_scope(
    parent_scope: &FrozenAccountScope,
    provider: Option<ProviderKind>,
    account: Option<&ProviderAccountId>,
    parent_account: Option<&ProviderAccountId>,
) -> Result<(Arc<FrozenAccountScope>, Option<ProviderKind>), GatewayError> {
    let mut account_scope = parent_scope.clone();
    if let Some(parent_account) = parent_account {
        if !parent_scope.allows(parent_account) {
            return Err(GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "parent attempt account is outside the frozen account scope",
            ));
        }
        if account == Some(parent_account) {
            return Err(GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "nested execution cannot wait for the parent attempt account",
            ));
        }
        account_scope = account_scope.excluding_account(parent_account.clone());
    }
    let required_provider = match account {
        Some(account) => {
            if !account_scope.allows(account) {
                return Err(GatewayError::new(
                    GatewayErrorKind::PolicyDenied,
                    "model account is outside the frozen authorization scope",
                ));
            }
            let account_provider = account_scope
                .account_provider(account)
                .cloned()
                .ok_or_else(|| {
                    GatewayError::new(
                        GatewayErrorKind::PolicyDenied,
                        "model account is unavailable in the frozen snapshot",
                    )
                })?;
            if provider
                .as_ref()
                .is_some_and(|provider| provider != &account_provider)
            {
                return Err(GatewayError::new(
                    GatewayErrorKind::PolicyDenied,
                    "model provider and account do not match",
                ));
            }
            Some(account_provider)
        }
        None => provider,
    };
    if required_provider
        .as_ref()
        .is_some_and(|provider| !account_scope.provider_kinds().contains(provider))
        || account_scope.provider_kinds().is_empty()
    {
        return Err(GatewayError::new(
            GatewayErrorKind::PolicyDenied,
            "model provider is outside the frozen authorization scope",
        ));
    }
    Ok((Arc::new(account_scope), required_provider))
}

fn native_policy(
    snapshot: &RuntimeSnapshot,
    authentication: &ClientAuthenticationRequest,
) -> Result<ClientPolicy, ClientAuthenticationError> {
    let plaintext = authentication
        .native_bearer()
        .ok_or(ClientAuthenticationError::InvalidKey)?
        .expose_for_auth();
    snapshot
        .client_policies()
        .filter(|policy| constant_time_equal(plaintext, policy.plaintext_key().expose_for_auth()))
        .find(|policy| policy.authorize().is_ok())
        .cloned()
        .ok_or(ClientAuthenticationError::InvalidKey)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn new_request_id() -> Result<ModelRequestId, GatewayError> {
    ModelRequestId::new(format!("req_{}", Uuid::now_v7().simple()))
        .map_err(|_| GatewayError::new(GatewayErrorKind::Internal, "failed to allocate request ID"))
}

fn authentication_gateway_error(error: ClientAuthenticationError) -> GatewayError {
    match error {
        ClientAuthenticationError::InvalidKey => {
            GatewayError::new(GatewayErrorKind::Unauthorized, "client API key is invalid")
        }
        ClientAuthenticationError::SnapshotUnavailable => GatewayError::new(
            GatewayErrorKind::Internal,
            "runtime snapshot is unavailable",
        ),
        ClientAuthenticationError::ProviderUnavailable => GatewayError::new(
            GatewayErrorKind::Internal,
            "frontend authentication provider is unavailable",
        ),
    }
}

fn map_routing_error(error: crate::validation::RoutingError) -> GatewayError {
    match error {
        crate::validation::RoutingError::ModelNotFound {
            model,
            mapped_model,
        } => GatewayError::new(
            GatewayErrorKind::ModelNotFound,
            if model == mapped_model {
                "the requested model was not found in the provider catalogs available to this API key; check the model name"
            } else {
                "the requested model maps to an upstream model that was not found in the provider catalogs available to this API key; check the configured model mapping"
            },
        ),
        crate::validation::RoutingError::NoCapableProvider { .. }
        | crate::validation::RoutingError::NoCapableProviderEndpoint { .. }
        | crate::validation::RoutingError::EmptyAccountScope => GatewayError::new(
            GatewayErrorKind::NoAvailableProvider,
            "no provider can execute this request",
        ),
        crate::validation::RoutingError::UnsupportedProviderEndpoint { .. } => GatewayError::new(
            GatewayErrorKind::Unsupported,
            "the selected provider does not support this operation",
        ),
        _ => GatewayError::new(
            GatewayErrorKind::Internal,
            "runtime routing configuration is invalid",
        ),
    }
}

fn attach_continuation_session_state(operation: &mut Operation, pin: &NativeContinuationPin) {
    let Some(state) = pin.session_state() else {
        return;
    };
    if state.provider() != pin.provider().as_str()
        || operation.provider_session_state(state.provider()).is_some()
    {
        return;
    }
    operation.set_provider_session_state(state.clone());
}

pub fn gateway_error_from_engine(error: &EngineError) -> GatewayError {
    match error {
        EngineError::Cancelled => {
            GatewayError::new(GatewayErrorKind::Cancelled, "request was cancelled")
        }
        EngineError::Deadline => {
            GatewayError::new(GatewayErrorKind::Timeout, "request deadline elapsed")
        }
        EngineError::Provider(provider) => GatewayError::from_provider(provider),
        EngineError::EmptyRoutingPlan | EngineError::ProviderNotRegistered { .. } => {
            GatewayError::new(
                GatewayErrorKind::NoAvailableProvider,
                "no provider is available",
            )
        }
        _ => GatewayError::new(GatewayErrorKind::Internal, "request execution failed"),
    }
}
