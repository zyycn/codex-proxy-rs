//! 执行请求、认证事实与对外会话端口

use crate::{
    engine::{
        CoordinatedEvent, EngineError, ModelRequestId,
        authentication::ClientAuthenticationRequest,
        continuation::PreviousResponseId,
        extensions::ExtensionCallScope,
        middleware::{
            FrozenMiddlewarePlan, MiddlewareAuthority, MiddlewareContext, MiddlewareTarget,
        },
        nested::ExecutionEffects,
    },
    error::{GatewayError, GatewayErrorKind},
    event::{ProviderEvent, ProviderResponseHeader},
    identity::ProviderKind,
    lifecycle::{CancellationToken, Deadline},
    operation::Operation,
    policy::{ClientApiKeyId, ClientPolicy},
    routing::{
        ProviderCatalogUnavailable, PublicModelDescriptor, PublicModelId, RuntimeSnapshot,
        UpstreamModelId, request_settings::RequestSettings,
    },
};
use futures::future::BoxFuture;
use std::{
    fmt,
    net::IpAddr,
    sync::Arc,
    time::{Duration, SystemTime},
};
use uuid::Uuid;

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

/// API 解码后交给 Core 的稳定请求元数据
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

impl ExecutionRequestMetadata {
    pub(super) fn inherit_request_origin(&mut self, scope: &ExtensionCallScope) {
        let Some(request) = scope.request() else {
            return;
        };
        // 执行入口可以是内部调用，客户端来源仍属于原始入口快照
        self.client_ip = request.client_ip;
        self.user_agent = request
            .headers
            .iter()
            .find(|header| header.name().eq_ignore_ascii_case("user-agent"))
            .and_then(|header| std::str::from_utf8(header.value()).ok())
            .map(str::to_owned);
    }
}

#[derive(Clone)]
pub struct AuthenticatedClient {
    pub(super) settings: Option<RequestSettings>,
    pub(super) snapshot: Arc<RuntimeSnapshot>,
    pub(super) policy: ClientPolicy,
    pub(super) authentication: Option<ClientAuthenticationRequest>,
}

impl AuthenticatedClient {
    pub(super) fn execution_timeout(&self) -> Option<Duration> {
        self.settings
            .as_ref()
            .and_then(|settings| settings.execution_timeout(self.policy.key_id()))
    }

    #[must_use]
    pub fn request_settings(&self) -> Option<&RequestSettings> {
        self.settings.as_ref()
    }

    /// 后续仍须重新认证；这里只更新下一次认证使用的冻结配置
    #[must_use]
    pub fn with_request_settings(mut self, settings: RequestSettings) -> Self {
        self.settings = Some(settings);
        self
    }

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

/// 入口中间件开始前冻结的根请求身份与生命周期
///
/// 中间件短路时本值直接释放；调用 `next` 后必须原样交回
/// [`ExecutionService::start_prepared`]，不能重新认证为另一个 Key
pub struct PreparedRootExecution {
    pub(super) extension_scope: ExtensionCallScope,
    pub(super) response_control: crate::engine::response_control::ResponseControl,
    pub(super) client: AuthenticatedClient,
    pub(super) request_id: ModelRequestId,
    pub(super) started_at: SystemTime,
    pub(super) deadline_at: Deadline,
    pub(super) cancellation: CancellationToken,
    pub(super) execution_effects: Arc<ExecutionEffects>,
    pub(super) execution_effects_baseline: usize,
}

impl PreparedRootExecution {
    #[must_use]
    pub fn request_settings(&self) -> RequestSettings {
        self.client
            .settings
            .clone()
            .unwrap_or_else(|| RequestSettings::new(self.client.snapshot.clone()))
            .with_execution(
                &self.client.policy,
                self.deadline_at
                    .at()
                    .map(|at| duration_ms(at.duration_since(self.started_at).unwrap_or_default())),
            )
    }

    /// 入口延续发起实例集合；保留已经交给调用方的取消句柄
    #[must_use]
    pub fn with_extension_scope(mut self, scope: ExtensionCallScope) -> Self {
        self.extension_scope = scope;
        self
    }

    #[must_use]
    pub fn extension_scope(&self) -> ExtensionCallScope {
        self.extension_scope.clone()
    }

    /// 设置已经在改写时完成编译；此处只将同一份结果应用到冻结身份和生命周期
    pub fn apply_settings(&mut self, settings: &RequestSettings) -> Result<(), GatewayError> {
        let deadline = settings
            .execution_deadline(self.client.policy.key_id(), self.started_at)
            .map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::InvalidRequest,
                    "execution settings are invalid",
                )
            })?;
        self.client.policy = settings.apply_policy(self.client.policy.clone());
        self.client.snapshot = settings.snapshot();
        self.client.settings = Some(settings.clone());
        self.deadline_at = deadline;
        Ok(())
    }

    #[must_use]
    pub fn response_control(&self) -> crate::engine::response_control::ResponseControl {
        self.response_control.clone()
    }

    pub(super) fn new(client: AuthenticatedClient) -> Result<Self, GatewayError> {
        let started_at = SystemTime::now();
        let deadline_at = Deadline::from_timeout(started_at, client.execution_timeout())
            .ok_or_else(|| {
                GatewayError::new(GatewayErrorKind::Internal, "system clock is invalid")
            })?;
        let execution_effects = Arc::new(ExecutionEffects::default());
        let execution_effects_baseline = execution_effects.epoch();
        Ok(Self {
            extension_scope: ExtensionCallScope::default(),
            client,
            response_control: crate::engine::response_control::ResponseControl::default(),
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
    pub const fn deadline_at(&self) -> Deadline {
        self.deadline_at
    }

    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// 以冻结的入口身份补齐 Runtime-only binding 事实；调用位置事实由 API/Core 提供
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

/// 入口中间件调用 `next` 后交给 Core 的已解码执行输入
pub struct PreparedExecutionRequest {
    pub public_model: PublicModelId,
    pub operation: Operation,
    pub metadata: ExecutionRequestMetadata,
}

/// 启动一个由协议 adapter 明确绑定到 Provider 自有端点的请求
pub struct StartProviderExecution {
    pub client: AuthenticatedClient,
    pub provider: ProviderKind,
    /// Provider 自有模型端点可固定上游模型；非模型端点保持 `None`
    pub upstream_model: Option<UpstreamModelId>,
    pub operation: Operation,
    pub metadata: ExecutionRequestMetadata,
}

pub struct StartedExecution {
    pub request_id: ModelRequestId,
    pub created_at: SystemTime,
    pub stream: bool,
    pub session: Box<dyn ExecutionSession>,
}

pub trait ExecutionSession: Send {
    /// 允许协议层在等待业务事件期间记录传输首字节，保活不提交业务批次
    fn response_commit(&self) -> super::ResponseCommit {
        super::ResponseCommit::default()
    }

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
    /// 执行已终结且结算、准入释放均已返回，协议层才可以放弃清理责任
    /// 结算失败仍由 Store 保留费用重试，释放失败仍按租约 TTL 收敛
    fn is_finalized(&self) -> bool;
    fn cancel(&self);
    /// 将会话交给宿主持续驱动清理；取消请求事件的等待不会丢弃已开始的结算
    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()>;
}

pub trait ExecutionService: Send + Sync {
    /// 未就绪时管理与健康接口仍可运行，模型认证按既有路径报告不可用
    fn request_settings(&self) -> Option<RequestSettings> {
        None
    }

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
    /// 只验证入口认证信封，不记录 Key 使用事实或执行推理准入
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

    /// 重新鉴权并冻结本次根请求身份、请求 ID、deadline 与取消域
    fn prepare_execution(
        &self,
        client: AuthenticatedClient,
    ) -> BoxFuture<'_, Result<PreparedRootExecution, GatewayError>> {
        Box::pin(async move { PreparedRootExecution::new(client) })
    }

    /// 由已经验证管理会话与插件 models 域的宿主入口选择 Key
    /// 不向页面提供明文，也不跳过普通请求的扩展、准入和计量
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

    /// 为已经通过 `verify_request` 的只读入口建立中间件生命周期，不重复写 Key 使用事实
    fn prepare_verified_execution(
        &self,
        client: AuthenticatedClient,
    ) -> Result<PreparedRootExecution, GatewayError> {
        PreparedRootExecution::new(client)
    }

    /// 解析与准备阶段冻结的发布代次一致的中间件计划
    fn middleware_plan(&self, _prepared: &PreparedRootExecution) -> Option<FrozenMiddlewarePlan> {
        None
    }

    /// 消费一次已准备身份并进入原有 Core 路由、准入、attempt 与结算路径
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

    /// 消费一次已准备身份，并进入 Provider 自有端点的原有准入与结算路径
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

    /// 返回 Provider 注册表中首个声明 Live sideband 能力的网关。
    /// `None` 表示当前组合不含语音 sideband；协议 adapter 据此回退到稳定 501。
    fn live_gateway(&self) -> Option<Arc<dyn crate::live::LiveGateway>> {
        None
    }
}

/// 只验证 Client API Key 并返回稳定 Key ID，不产生请求使用事实
///
/// 管理侧登录等非数据面场景必须使用该端口，避免把认证本身误记为一次 Key 使用
pub trait ClientKeyVerifier: Send + Sync {
    fn verify_client_key(
        &self,
        plaintext: &str,
    ) -> Result<ClientApiKeyId, ClientAuthenticationError>;
}

/// 成功认证后的 API Key 使用事实接收器
///
/// 认证仍是同步快照读取；实现必须自行异步、去重地持久化，不得阻塞客户端请求
pub trait ClientApiKeyUsageSink: Send + Sync {
    fn record_used(&self, key_id: &ClientApiKeyId);
}

pub(super) fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn new_request_id() -> Result<ModelRequestId, GatewayError> {
    ModelRequestId::new(format!("req_{}", Uuid::now_v7().simple()))
        .map_err(|_| GatewayError::new(GatewayErrorKind::Internal, "failed to allocate request ID"))
}
