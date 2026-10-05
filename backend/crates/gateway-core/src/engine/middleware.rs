//! 数据面请求/响应的统一洋葱中间件端口
//!
//! 本模块只承载宿主内部组合合同
//! 插件 RPC 如何表示 `next` 和正文句柄由
//! Runtime/SDK 负责；身份、路由、账号租约、发送事实、重试、计量与结算仍由 Core
//! 持有，不能通过本端口交给插件解释

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU32;
use std::sync::{Arc, RwLock, Weak};

use bytes::Bytes;
use futures::future::BoxFuture;

use crate::account::{ProviderAccountId, scope::AccountGroupId};
use crate::engine::execution::ClientTransport;
use crate::engine::extensions::ExtensionCallScope;
use crate::engine::nested::ExecutionEffects;
use crate::engine::provider::ProviderCallMetadata;
use crate::engine::{EngineError, ModelRequestId};
use crate::error::{GatewayError, ProviderError};
use crate::event::ProviderEvent;
use crate::identity::ProviderKind;
use crate::lifecycle::CancellationToken;
use crate::operation::{
    CapabilityRequirements, Feature, GenerateRequest, Operation, OperationKind, ProtocolPayload,
};
use crate::policy::ClientApiKeyId;
use crate::runtime::extensions::{ExtensionSetId, ExtensionSetReference};
use crate::settings::RequestSettings;

/// 中间件在一次逻辑请求中的挂载位置
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MiddlewareMount {
    /// 所有 HTTP 接口的认证、路由和正文解析之前
    Http,
    /// 下游 WebSocket 消息在默认解析和写入之前组合
    WebSocket,
    /// 类型化公开服务在执行默认业务逻辑前组合
    Service,
    /// 客户端请求外层；一次逻辑请求只执行一次
    Request,
    /// 受管 Provider attempt 内层；每次真实 retry 都重新建立
    Attempt,
}

/// 中间件正文块的宿主边界
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiddlewareFraming {
    /// 一个完整 JSON document
    JsonDocument,
    /// 一个完整 SSE event，包含终止该事件的空行
    SseEvent,
    /// 不承诺 UTF-8 或消息边界的原始字节
    RawBytes,
}

/// 允许进入中间件合同的多值 HTTP 头
#[derive(Clone, PartialEq, Eq)]
pub struct MiddlewareHeader {
    name: String,
    value: Bytes,
}

impl MiddlewareHeader {
    #[must_use]
    pub fn new(name: impl Into<String>, value: Bytes) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn value(&self) -> &Bytes {
        &self.value
    }

    #[must_use]
    pub fn into_parts(self) -> (String, Bytes) {
        (self.name, self.value)
    }
}

impl fmt::Debug for MiddlewareHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewareHeader")
            .field("name", &self.name)
            .field("value", &"<not included in Debug>")
            .finish()
    }
}

/// 一次中间件调用可修改的请求事实
#[derive(Clone, PartialEq, Eq)]
pub struct MiddlewareRequest {
    settings: Option<RequestSettings>,
    protocol: String,
    headers: Vec<MiddlewareHeader>,
    body: Bytes,
    original_body: Bytes,
    original_protocol: String,
    capabilities: Option<MiddlewareCapabilityDeclaration>,
}

/// 只声明具体功能的责任归属，实际正文中的需求始终继续生效
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MiddlewareCapabilityDeclaration {
    pub handled: BTreeSet<Feature>,
    pub required: BTreeSet<Feature>,
}

impl MiddlewareRequest {
    #[must_use]
    pub fn new(protocol: impl Into<String>, headers: Vec<MiddlewareHeader>, body: Bytes) -> Self {
        let protocol = protocol.into();
        Self {
            settings: None,
            original_protocol: protocol.clone(),
            original_body: body.clone(),
            capabilities: None,
            protocol,
            headers,
            body,
        }
    }

    #[must_use]
    pub fn with_settings(mut self, settings: RequestSettings) -> Self {
        self.settings = Some(settings);
        self
    }

    #[must_use]
    pub fn settings(&self) -> Option<&RequestSettings> {
        self.settings.as_ref()
    }

    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    #[must_use]
    pub fn headers(&self) -> &[MiddlewareHeader] {
        &self.headers
    }

    #[must_use]
    pub const fn body(&self) -> &Bytes {
        &self.body
    }

    #[must_use]
    pub fn into_parts(self) -> (String, Vec<MiddlewareHeader>, Bytes) {
        (self.protocol, self.headers, self.body)
    }

    /// 保留链入口的语义来源；新增声明必须对应当前这一层实际消除的功能字段
    pub fn replace_parts(
        mut self,
        protocol: String,
        headers: Vec<MiddlewareHeader>,
        body: Bytes,
        declaration: Option<MiddlewareCapabilityDeclaration>,
    ) -> Result<Self, MiddlewareError> {
        if let Some(declaration) = declaration {
            if self.protocol != "openai"
                || protocol != "openai"
                || self.original_protocol != "openai"
                || declaration.handled.contains(&Feature::NativeContinuation)
                || !declaration.handled.is_disjoint(&declaration.required)
                || (declaration.handled.is_empty() && declaration.required.is_empty())
            {
                return Err(MiddlewareError::InvalidState);
            }
            let before = generate_requirements(&self.body)?;
            let after = generate_requirements(&body)?;
            if !declaration.handled.is_subset(before.features())
                || !declaration.handled.is_disjoint(after.features())
            {
                return Err(MiddlewareError::InvalidState);
            }
            let accumulated = self.capabilities.get_or_insert_default();
            accumulated.handled.extend(declaration.handled);
            accumulated.required.extend(declaration.required);
        }
        self.protocol = protocol;
        self.headers = headers;
        self.body = body;
        Ok(self)
    }

    /// API 在最终解码后应用；没有声明的透传路径不解析或重新编码正文
    pub fn apply_capabilities(&self, operation: Operation) -> Result<Operation, MiddlewareError> {
        let Some(declaration) = &self.capabilities else {
            return Ok(operation);
        };
        if operation.kind() != OperationKind::Generate || operation.protocol() != "openai" {
            return Err(MiddlewareError::InvalidState);
        }
        let original = generate_requirements(&self.original_body)?;
        let mut effective = CapabilityRequirements::new(OperationKind::Generate);
        for feature in original
            .features()
            .difference(&declaration.handled)
            .chain(declaration.required.iter())
        {
            effective = effective.require(*feature);
        }
        operation
            .with_inherited_capability_requirements(effective)
            .map_err(|_| MiddlewareError::InvalidState)
    }

    #[must_use]
    pub fn has_capability_declaration(&self) -> bool {
        self.capabilities.is_some()
    }
}

fn generate_requirements(body: &[u8]) -> Result<CapabilityRequirements, MiddlewareError> {
    let body = serde_json::from_slice(body).map_err(|_| MiddlewareError::InvalidState)?;
    let payload =
        ProtocolPayload::json_object("openai", body).map_err(|_| MiddlewareError::InvalidState)?;
    Ok(
        Operation::Generate(GenerateRequest::from_protocol_payload(payload))
            .capability_requirements(),
    )
}

impl fmt::Debug for MiddlewareRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewareRequest")
            .field("protocol", &self.protocol)
            .field("headers", &self.headers)
            .field("body", &"<not included in Debug>")
            .finish()
    }
}

/// 中间件逐次拉取的完整正文边界
pub struct MiddlewareFrame {
    bytes: Bytes,
    framing: MiddlewareFraming,
    terminal: bool,
    transformed: bool,
    envelope: Option<MiddlewareFrameEnvelope>,
}

impl MiddlewareFrame {
    #[must_use]
    pub const fn new(bytes: Bytes, framing: MiddlewareFraming, terminal: bool) -> Self {
        Self {
            bytes,
            framing,
            terminal,
            transformed: false,
            envelope: None,
        }
    }

    #[must_use]
    pub const fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    #[must_use]
    pub const fn framing(&self) -> MiddlewareFraming {
        self.framing
    }

    #[must_use]
    pub const fn terminal(&self) -> bool {
        self.terminal
    }

    /// 返回正文是否经过至少一个中间件的替换、展开或丢弃映射
    ///
    /// 插件可读取该事实；对快照的修改不改变宿主确认的来源
    #[must_use]
    pub const fn transformed(&self) -> bool {
        self.transformed
    }

    /// 返回来源事件的只读视图；读取不转移原结算和会话状态的所有权
    #[must_use]
    pub fn event(&self) -> Option<&ProviderEvent> {
        self.envelope.as_ref().map(MiddlewareFrameEnvelope::event)
    }

    /// 继承宿主确认的改写事实；`false` 不能清除内层已经记录的改写
    #[must_use]
    pub fn with_transformed(mut self, transformed: bool) -> Self {
        self.transformed |= transformed;
        self
    }

    #[must_use]
    pub fn into_bytes(self) -> Bytes {
        self.bytes
    }

    /// 拆出插件可见正文与只能由宿主原样搬运的事实信封
    ///
    /// Runtime 在一对多转换时必须把信封附到最后一个输出；这样 `Completed` 不会在
    /// 前置输出交付前提前终结
    /// 读取快照不会转移信封的计量所有权
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Bytes,
        MiddlewareFraming,
        bool,
        Option<MiddlewareFrameEnvelope>,
    ) {
        (self.bytes, self.framing, self.terminal, self.envelope)
    }

    /// 把宿主持有的事实信封附到改写后的最后一个输出
    #[must_use]
    pub fn with_envelope(mut self, envelope: MiddlewareFrameEnvelope) -> Self {
        self.envelope = Some(envelope);
        self
    }

    pub(crate) fn from_provider_event(
        bytes: Bytes,
        framing: MiddlewareFraming,
        terminal: bool,
        event: ProviderEvent,
    ) -> Self {
        let transformed = event.middleware_transformed();
        Self {
            bytes,
            framing,
            terminal,
            transformed,
            envelope: Some(MiddlewareFrameEnvelope { event }),
        }
    }

    pub(crate) fn into_provider_parts(
        self,
    ) -> (Bytes, MiddlewareFraming, bool, Option<ProviderEvent>) {
        (
            self.bytes,
            self.framing,
            self.terminal,
            self.envelope.map(|envelope| envelope.event),
        )
    }
}

impl fmt::Debug for MiddlewareFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewareFrame")
            .field("bytes", &"<not included in Debug>")
            .field("framing", &self.framing)
            .field("terminal", &self.terminal)
            .field("transformed", &self.transformed)
            .field("has_host_envelope", &self.envelope.is_some())
            .finish()
    }
}

/// 可读取完整事件、只能随正文移动一次的 Core 事实信封
///
/// 该类型故意不实现 `Clone`，防止同一 usage/cost 在一对多响应中被复制
pub struct MiddlewareFrameEnvelope {
    event: ProviderEvent,
}

impl MiddlewareFrameEnvelope {
    #[must_use]
    pub const fn event(&self) -> &ProviderEvent {
        &self.event
    }
}

impl fmt::Debug for MiddlewareFrameEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewareFrameEnvelope")
            .finish_non_exhaustive()
    }
}

/// 洋葱返回路径上的惰性正文
///
/// 实现必须让 `Drop` 与 [`Self::close`] 都回收底层流
/// 请求 finalization 继续委托
/// 原有 [`crate::engine::execution::ExecutionSession`]，本端口不建立第二套清理器
pub trait MiddlewareBody: Send {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>>;

    /// 在最外层输出准备完成后提交原有 Core delivery barrier
    /// 中间件实现只能向内委托
    fn commit_downstream(
        &mut self,
        _client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async { Ok(()) })
    }

    /// 首字节前失败时记录最终返回给客户端的状态
    /// 中间件实现只能向内委托
    fn record_client_status(
        &mut self,
        _client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async { Ok(()) })
    }

    /// 没有调用 `next` 的短路正文视为无需 Core 结算；调用过 `next` 的包装器必须委托
    fn is_finalized(&self) -> bool {
        true
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()>;
}

/// `next` 或短路返回的响应头与惰性正文
pub struct MiddlewareResponse {
    protocol: String,
    status_code: u16,
    headers: Vec<MiddlewareHeader>,
    body: Box<dyn MiddlewareBody>,
    envelope: Option<MiddlewareResponseEnvelope>,
}

impl MiddlewareResponse {
    #[must_use]
    pub fn new(
        protocol: String,
        status_code: u16,
        headers: Vec<MiddlewareHeader>,
        body: Box<dyn MiddlewareBody>,
    ) -> Self {
        Self {
            protocol,
            status_code,
            headers,
            body,
            envelope: None,
        }
    }

    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    #[must_use]
    pub const fn status_code(&self) -> u16 {
        self.status_code
    }

    #[must_use]
    pub fn headers(&self) -> &[MiddlewareHeader] {
        &self.headers
    }

    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        String,
        u16,
        Vec<MiddlewareHeader>,
        Box<dyn MiddlewareBody>,
        Option<MiddlewareResponseEnvelope>,
    ) {
        (
            self.protocol,
            self.status_code,
            self.headers,
            self.body,
            self.envelope,
        )
    }

    /// 搬运宿主 terminal 的来源事实；读取快照不转移其所有权
    #[must_use]
    pub fn with_envelope(mut self, envelope: MiddlewareResponseEnvelope) -> Self {
        self.envelope = Some(envelope);
        self
    }

    pub(crate) fn with_provider_metadata(mut self, metadata: ProviderCallMetadata) -> Self {
        self.envelope = Some(MiddlewareResponseEnvelope { metadata });
        self
    }
}

impl fmt::Debug for MiddlewareResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewareResponse")
            .field("protocol", &self.protocol)
            .field("status_code", &self.status_code)
            .field("headers", &self.headers)
            .field("body", &"<stream>")
            .field("has_host_envelope", &self.envelope.is_some())
            .finish()
    }
}

/// Attempt terminal 的 Core 来源事实；中间件只能随响应原样搬运
pub struct MiddlewareResponseEnvelope {
    metadata: ProviderCallMetadata,
}

impl MiddlewareResponseEnvelope {
    #[must_use]
    pub const fn metadata(&self) -> &ProviderCallMetadata {
        &self.metadata
    }

    pub(crate) fn into_provider_metadata(self) -> ProviderCallMetadata {
        self.metadata
    }
}

impl fmt::Debug for MiddlewareResponseEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewareResponseEnvelope")
            .finish_non_exhaustive()
    }
}

/// Core 冻结的调用事实；完整业务身份可由插件读取
#[derive(Clone)]
pub struct MiddlewareContext {
    plan: Option<FrozenMiddlewarePlan>,
    request_id: ModelRequestId,
    mount: MiddlewareMount,
    attempt_index: Option<NonZeroU32>,
    operation: Option<OperationKind>,
    endpoint: String,
    transport: ClientTransport,
    provider: Option<ProviderKind>,
    model: Option<String>,
    account_id: Option<ProviderAccountId>,
    client_key_id: ClientApiKeyId,
    account_group_ids: Arc<[AccountGroupId]>,
    cancellation: CancellationToken,
    deadline: crate::lifecycle::Deadline,
    extension_scope: ExtensionCallScope,
    execution_effects: Option<Arc<ExecutionEffects>>,
}

impl MiddlewareContext {
    #[must_use]
    pub fn plan(&self) -> Option<FrozenMiddlewarePlan> {
        self.plan.clone()
    }

    #[must_use]
    pub fn new(target: MiddlewareTarget, authority: MiddlewareAuthority) -> Self {
        let MiddlewareTarget {
            request_id,
            mount,
            attempt_index,
            operation,
            endpoint,
            transport,
            provider,
            model,
            account_id,
        } = target;
        let MiddlewareAuthority {
            client_key_id,
            account_group_ids,
            cancellation,
            deadline,
            extension_scope,
            execution_effects,
        } = authority;
        Self {
            plan: None,
            request_id,
            mount,
            attempt_index,
            operation,
            endpoint,
            transport,
            provider,
            model,
            account_id,
            client_key_id,
            account_group_ids,
            cancellation,
            deadline,
            extension_scope,
            execution_effects,
        }
    }

    #[must_use]
    pub const fn request_id(&self) -> &ModelRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn mount(&self) -> MiddlewareMount {
        self.mount
    }

    #[must_use]
    pub const fn attempt_index(&self) -> Option<NonZeroU32> {
        self.attempt_index
    }

    #[must_use]
    pub const fn operation(&self) -> Option<OperationKind> {
        self.operation
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    #[must_use]
    pub const fn transport(&self) -> ClientTransport {
        self.transport
    }

    #[must_use]
    pub const fn provider(&self) -> Option<&ProviderKind> {
        self.provider.as_ref()
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    #[must_use]
    pub const fn account_id(&self) -> Option<&ProviderAccountId> {
        self.account_id.as_ref()
    }

    /// 返回冻结的业务归属，也用于匹配 binding
    #[must_use]
    pub const fn client_key_id(&self) -> &ClientApiKeyId {
        &self.client_key_id
    }

    /// 返回冻结的业务归属，也用于匹配 binding
    #[must_use]
    pub fn account_group_ids(&self) -> &[AccountGroupId] {
        &self.account_group_ids
    }

    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    #[must_use]
    pub const fn deadline(&self) -> crate::lifecycle::Deadline {
        self.deadline
    }

    #[must_use]
    pub const fn extension_scope(&self) -> &ExtensionCallScope {
        &self.extension_scope
    }

    /// 只供受信 Runtime 记录本次中间件调用已经产生的外部副作用，不进入插件 wire
    #[must_use]
    pub fn execution_effects(&self) -> Option<Arc<ExecutionEffects>> {
        self.execution_effects.as_ref().map(Arc::clone)
    }
}

/// 不含授权主体的调用位置与目标事实
pub struct MiddlewareTarget {
    pub request_id: ModelRequestId,
    pub mount: MiddlewareMount,
    pub attempt_index: Option<NonZeroU32>,
    pub operation: Option<OperationKind>,
    pub endpoint: String,
    pub transport: ClientTransport,
    pub provider: Option<ProviderKind>,
    pub model: Option<String>,
    pub account_id: Option<ProviderAccountId>,
}

/// 冻结的业务主体与生命周期；不用于裁剪插件权限
pub struct MiddlewareAuthority {
    pub client_key_id: ClientApiKeyId,
    pub account_group_ids: Arc<[AccountGroupId]>,
    pub cancellation: CancellationToken,
    pub deadline: crate::lifecycle::Deadline,
    pub extension_scope: ExtensionCallScope,
    pub execution_effects: Option<Arc<ExecutionEffects>>,
}

impl fmt::Debug for MiddlewareContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewareContext")
            .field("request_id", &self.request_id)
            .field("mount", &self.mount)
            .field("attempt_index", &self.attempt_index)
            .field("operation", &self.operation)
            .field("endpoint", &self.endpoint)
            .field("transport", &self.transport)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("account_id", &self.account_id)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// 中间件链调用错误
/// 原有域错误保持原类型，不能被插件故障改写为“未发送”
#[derive(Debug)]
pub enum MiddlewareError {
    Rejected,
    Fault,
    /// 保留跨进程失败，供外层插件读取；客户端响应由协议 owner 决定
    Remote {
        source: Box<dyn std::error::Error + Send + Sync>,
        rejected: bool,
    },
    InvalidState,
    Gateway(GatewayError),
    Engine(EngineError),
    Provider(ProviderError),
}

impl fmt::Display for MiddlewareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Rejected => "middleware rejected the request",
            Self::Fault => "middleware call failed",
            Self::Remote { source, .. } => return fmt::Display::fmt(source, formatter),
            Self::InvalidState => "middleware state is invalid",
            Self::Gateway(_) => "gateway middleware terminal failed",
            Self::Engine(_) => "execution middleware terminal failed",
            Self::Provider(_) => "provider middleware terminal failed",
        })
    }
}

impl std::error::Error for MiddlewareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Remote { source, .. } => Some(source.as_ref()),
            Self::Gateway(error) => Some(error),
            Self::Engine(error) => Some(error),
            Self::Provider(error) => Some(error),
            _ => None,
        }
    }
}

impl MiddlewareError {
    #[must_use]
    pub const fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected | Self::Remote { rejected: true, .. })
    }
}

impl From<GatewayError> for MiddlewareError {
    fn from(error: GatewayError) -> Self {
        Self::Gateway(error)
    }
}

impl From<EngineError> for MiddlewareError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}

impl From<ProviderError> for MiddlewareError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

/// 绑定当前调用且只能消费一次的宿主续体
pub type MiddlewareNext =
    crate::middleware::Next<MiddlewareRequest, MiddlewareResponse, MiddlewareError>;

/// 一个发布代次冻结的统一数据面中间件计划
pub trait MiddlewarePlan: Send + Sync + fmt::Debug {
    fn has_service(&self) -> bool {
        false
    }
    fn handle_service(
        &self,
        _context: crate::middleware::service::Context,
        input: crate::middleware::service::Value,
        next: crate::middleware::service::Next,
    ) -> BoxFuture<
        'static,
        Result<crate::middleware::service::Value, crate::middleware::service::Error>,
    > {
        next.run(input)
    }

    fn has_websocket(&self) -> bool {
        false
    }
    fn handle_websocket(
        &self,
        _context: crate::middleware::websocket::Context,
        message: crate::middleware::websocket::Message,
        next: crate::middleware::websocket::Next,
    ) -> BoxFuture<'static, Result<Option<crate::middleware::websocket::Message>, MiddlewareError>>
    {
        next.run(message)
    }

    /// HTTP 入口在认证、路由与正文解析之前组合，无匹配时保留直接路径
    fn has_http(&self) -> bool {
        false
    }

    fn handle_http(
        &self,
        _context: crate::middleware::http::Context,
        request: crate::middleware::http::Request,
        next: crate::middleware::http::Next,
    ) -> BoxFuture<'static, Result<crate::middleware::http::Response, MiddlewareError>> {
        next.run(request)
    }

    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>>;
}

/// 调用期间同时保活发布代次与中间件计划
#[derive(Clone)]
pub struct FrozenMiddlewarePlan {
    plan: Arc<dyn MiddlewarePlan>,
    _generation: ExtensionSetReference,
}

impl FrozenMiddlewarePlan {
    #[must_use]
    pub fn has_service(&self) -> bool {
        self.plan.has_service()
    }
    pub fn handle_service(
        &self,
        context: crate::middleware::service::Context,
        input: crate::middleware::service::Value,
        next: crate::middleware::service::Next,
    ) -> BoxFuture<
        'static,
        Result<crate::middleware::service::Value, crate::middleware::service::Error>,
    > {
        self.plan.handle_service(context, input, next)
    }

    #[must_use]
    pub fn has_websocket(&self) -> bool {
        self.plan.has_websocket()
    }
    pub fn handle_websocket(
        &self,
        context: crate::middleware::websocket::Context,
        message: crate::middleware::websocket::Message,
        next: crate::middleware::websocket::Next,
    ) -> BoxFuture<'static, Result<Option<crate::middleware::websocket::Message>, MiddlewareError>>
    {
        self.plan.handle_websocket(context, message, next)
    }

    #[must_use]
    pub fn has_http(&self) -> bool {
        self.plan.has_http()
    }

    pub fn handle_http(
        &self,
        mut context: crate::middleware::http::Context,
        request: crate::middleware::http::Request,
        next: crate::middleware::http::Next,
    ) -> BoxFuture<'static, Result<crate::middleware::http::Response, MiddlewareError>> {
        context.plan = Some(self.clone());
        self.plan.handle_http(context, request, next)
    }

    #[must_use]
    pub fn new(plan: Arc<dyn MiddlewarePlan>, generation: ExtensionSetReference) -> Self {
        Self {
            plan,
            _generation: generation,
        }
    }

    pub fn handle(
        &self,
        mut context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        context.plan = Some(self.clone());
        self.plan.handle(context, request, next)
    }
}

impl fmt::Debug for FrozenMiddlewarePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrozenMiddlewarePlan")
            .finish_non_exhaustive()
    }
}

/// 同一发布代次不能被静默替换
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("middleware generation is already registered")]
pub struct MiddlewareRegistrationError;

/// 按请求冻结的发布集合解析中间件计划；索引不延长旧代次寿命
#[derive(Clone, Default)]
pub struct MiddlewareExtensionIndex {
    sets: Arc<RwLock<BTreeMap<ExtensionSetId, Weak<dyn MiddlewarePlan>>>>,
}

impl MiddlewareExtensionIndex {
    pub fn register(
        &self,
        id: ExtensionSetId,
        plan: Arc<dyn MiddlewarePlan>,
    ) -> Result<Arc<dyn MiddlewarePlan>, MiddlewareRegistrationError> {
        let mut sets = self
            .sets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.retain(|_, plan| plan.strong_count() > 0);
        if sets.contains_key(&id) {
            return Err(MiddlewareRegistrationError);
        }
        sets.insert(id, Arc::downgrade(&plan));
        Ok(plan)
    }

    #[must_use]
    pub fn resolve(&self, generation: &ExtensionSetReference) -> Option<FrozenMiddlewarePlan> {
        self.sets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(generation.id())
            .and_then(Weak::upgrade)
            .map(|plan| FrozenMiddlewarePlan::new(plan, generation.clone()))
    }
}
