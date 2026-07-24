//! Provider 的唯一动态执行边界。

use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use thiserror::Error;

use crate::engine::credential::{
    AccountAttemptFeedback, AccountFeedbackStats, CredentialRevision, ProviderAccountId,
};
use crate::engine::{AttemptContext, UpstreamSendState};
use crate::error::{
    IdentifierError, ProviderError, ProviderErrorKind, SafeUpstreamValue, validate_text,
};
use crate::event::{EventSequenceValidator, ProviderEvent};
use crate::operation::Operation;
use crate::routing::{ModelCapabilities, ProviderCandidate, ProviderKind, UpstreamModelId};

/// Box 只出现在 Provider Registry 的统一 event envelope 边界。
pub type EventStream =
    Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send + 'static>>;

/// 匿名 credential/resource 引用。
///
/// 有 credential 的值必须是带算法/版本标识的不可逆伪名；无 credential 的
/// Provider 使用 `__none__`。邮箱、API Key prefix 等身份信息不能进入该类型。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(String);

impl ResourceId {
    /// 无 credential 的稳定 sentinel。
    #[must_use]
    pub fn none() -> Self {
        Self("__none__".to_owned())
    }

    /// 创建匿名资源 ID。
    ///
    /// # Errors
    ///
    /// ID 为空、过长、含控制字符或使用 `__` 保留前缀时返回错误。
    pub fn anonymous(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 128, true, None)?;
        Ok(Self(value))
    }

    /// 返回匿名引用。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Provider 本次调用实际使用的账号或匿名资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderResource {
    Account {
        id: ProviderAccountId,
        revision: CredentialRevision,
    },
    Anonymous(ResourceId),
}

impl ProviderResource {
    #[must_use]
    pub const fn account_id(&self) -> Option<&ProviderAccountId> {
        match self {
            Self::Account { id, .. } => Some(id),
            Self::Anonymous(_) => None,
        }
    }

    #[must_use]
    pub const fn credential_revision(&self) -> Option<CredentialRevision> {
        match self {
            Self::Account { revision, .. } => Some(*revision),
            Self::Anonymous(_) => None,
        }
    }

    #[must_use]
    pub const fn anonymous_id(&self) -> Option<&ResourceId> {
        match self {
            Self::Account { .. } => None,
            Self::Anonymous(resource) => Some(resource),
        }
    }
}

/// 上游 transport 注册名称。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UpstreamTransport(String);

impl UpstreamTransport {
    /// 校验 transport 名称。
    ///
    /// # Errors
    ///
    /// 名称无效时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 64, true, None)?;
        Ok(Self(value))
    }

    /// 返回 transport 名称。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Provider 选定单个 credential 后返回的事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCallMetadata {
    provider: ProviderKind,
    upstream_model: UpstreamModelId,
    resource: ProviderResource,
    upstream_request_id: Option<SafeUpstreamValue>,
    transport: UpstreamTransport,
}

impl ProviderCallMetadata {
    /// 创建一次调用的不可变事实。
    #[must_use]
    pub const fn new(
        provider: ProviderKind,
        upstream_model: UpstreamModelId,
        resource: ProviderResource,
        transport: UpstreamTransport,
    ) -> Self {
        Self {
            provider,
            upstream_model,
            resource,
            upstream_request_id: None,
            transport,
        }
    }

    /// 设置 adapter 已分类为非 bearer 的 request ID。
    #[must_use]
    pub fn with_upstream_request_id(mut self, request_id: SafeUpstreamValue) -> Self {
        self.upstream_request_id = Some(request_id);
        self
    }

    /// 返回 Provider。
    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    /// 返回实际模型；必须与冻结 target 一致。
    #[must_use]
    pub const fn upstream_model(&self) -> &UpstreamModelId {
        &self.upstream_model
    }

    /// 返回匿名资源；账号调用返回 `None`。
    #[must_use]
    pub const fn anonymous_resource(&self) -> Option<&ResourceId> {
        self.resource.anonymous_id()
    }

    /// 返回 live Provider account ID。
    #[must_use]
    pub const fn provider_account_id(&self) -> Option<&ProviderAccountId> {
        self.resource.account_id()
    }

    /// 返回本次冻结的 credential revision。
    #[must_use]
    pub const fn upstream_credential_revision(&self) -> Option<CredentialRevision> {
        self.resource.credential_revision()
    }

    /// 返回安全上游 request ID。
    #[must_use]
    pub const fn upstream_request_id(&self) -> Option<&SafeUpstreamValue> {
        self.upstream_request_id.as_ref()
    }

    /// 返回 transport。
    #[must_use]
    pub const fn transport(&self) -> &UpstreamTransport {
        &self.transport
    }

    /// 确认 metadata 没有替换请求计划中冻结的 Provider 候选。
    #[must_use]
    pub fn confirms(&self, candidate: &ProviderCandidate) -> bool {
        candidate.provider() == &self.provider && candidate.upstream_model() == &self.upstream_model
    }
}

/// Provider 自己持有的 credential/concurrency 租约。
///
/// 句柄必须通过 `Drop` 释放资源。Core 不读取 credential，也不为 lease 定义
/// 平台无关字段。
pub trait ResourceLease: Send + Sync + 'static {}

impl<T> ResourceLease for T where T: Send + Sync + 'static {}

/// Metadata、canonical event stream 与 owned lease 的统一返回值。
///
/// 底层 stream 必须是 cold stream：在第一次 poll 前不得发送请求级 handshake
/// 或业务 payload。这样 Coordinator 可以先持久化 attempt，再越过发送屏障。
pub struct ProviderStream {
    metadata: ProviderCallMetadata,
    events: EventStream,
    _lease: Box<dyn ResourceLease>,
    account_feedback: Option<ProviderStreamAccountFeedback>,
    validator: EventSequenceValidator,
    terminated: bool,
}

struct ProviderStreamAccountFeedback {
    stats: Arc<AccountFeedbackStats>,
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
    started_at: Option<Instant>,
    first_output_ms: Option<u64>,
    reported: bool,
}

impl ProviderStreamAccountFeedback {
    fn start(&mut self) {
        self.started_at.get_or_insert_with(Instant::now);
    }

    fn observe(&mut self, event: &ProviderEvent) {
        if self.first_output_ms.is_some()
            || !event.canonical_facts().iter().any(|event| {
                matches!(
                    event,
                    crate::event::GatewayEvent::TextDelta(_)
                        | crate::event::GatewayEvent::ReasoningDelta(_)
                        | crate::event::GatewayEvent::ToolCallDelta(_)
                        | crate::event::GatewayEvent::CompactionOutput(_)
                )
            })
        {
            return;
        }
        let Some(started_at) = self.started_at else {
            return;
        };
        self.first_output_ms =
            Some(u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX));
    }

    fn report_success(&mut self) {
        if self.reported {
            return;
        }
        self.stats.report(
            &self.provider_kind,
            &self.account_id,
            AccountAttemptFeedback::Succeeded {
                first_output_ms: self.first_output_ms,
            },
        );
        self.reported = true;
    }

    fn report_failure(&mut self, error: &ProviderError) {
        if self.reported
            || error.send_state() == UpstreamSendState::NotSent
            || matches!(
                error.kind(),
                ProviderErrorKind::Cancelled | ProviderErrorKind::ProcessTerminated
            )
        {
            return;
        }
        self.stats.report(
            &self.provider_kind,
            &self.account_id,
            AccountAttemptFeedback::Failed {
                first_output_ms: self.first_output_ms,
            },
        );
        self.reported = true;
    }
}

impl ProviderStream {
    /// 组装一次、且仅一次可见上游调用。
    #[must_use]
    pub fn new<S>(metadata: ProviderCallMetadata, events: S, lease: impl ResourceLease) -> Self
    where
        S: Stream<Item = Result<ProviderEvent, ProviderError>> + Send + 'static,
    {
        Self {
            metadata,
            events: Box::pin(events),
            _lease: Box::new(lease),
            account_feedback: None,
            validator: EventSequenceValidator::new(),
            terminated: false,
        }
    }

    /// 让公共 stream 边界统一回灌账号成功率与首个有效输出延迟。
    #[must_use]
    pub fn with_account_feedback(mut self, stats: Arc<AccountFeedbackStats>) -> Self {
        if let Some(account_id) = self.metadata.provider_account_id().cloned() {
            self.account_feedback = Some(ProviderStreamAccountFeedback {
                stats,
                provider_kind: self.metadata.provider().clone(),
                account_id,
                started_at: None,
                first_output_ms: None,
                reported: false,
            });
        }
        self
    }

    /// 返回调用事实。
    #[must_use]
    pub const fn metadata(&self) -> &ProviderCallMetadata {
        &self.metadata
    }
}

impl Stream for ProviderStream {
    type Item = Result<ProviderEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.terminated {
            return Poll::Ready(None);
        }
        if let Some(feedback) = this.account_feedback.as_mut() {
            feedback.start();
        }

        match this.events.as_mut().poll_next(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(event))) => {
                for fact in event.canonical_facts() {
                    if this.validator.observe(fact).is_err() {
                        this.terminated = true;
                        let error = ProviderError::new(
                            ProviderErrorKind::Protocol,
                            UpstreamSendState::Sent,
                        );
                        if let Some(feedback) = this.account_feedback.as_mut() {
                            feedback.report_failure(&error);
                        }
                        return Poll::Ready(Some(Err(error)));
                    }
                }
                if let Some(feedback) = this.account_feedback.as_mut() {
                    feedback.observe(&event);
                }
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.terminated = true;
                if let Some(feedback) = this.account_feedback.as_mut() {
                    feedback.report_failure(&error);
                }
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.terminated = true;
                match this.validator.finish() {
                    Ok(()) => {
                        if let Some(feedback) = this.account_feedback.as_mut() {
                            feedback.report_success();
                        }
                        Poll::Ready(None)
                    }
                    Err(_) => {
                        let error = ProviderError::new(
                            ProviderErrorKind::Protocol,
                            UpstreamSendState::Sent,
                        );
                        if let Some(feedback) = this.account_feedback.as_mut() {
                            feedback.report_failure(&error);
                        }
                        Poll::Ready(Some(Err(error)))
                    }
                }
            }
        }
    }
}

/// 传给 Provider 的单候选请求。
#[derive(Clone)]
pub struct ProviderRequest {
    operation: Operation,
    candidate: ProviderCandidate,
}

/// Provider 对公共观测表可解释的请求语义；未知字段保持空值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderRequestObservation {
    /// 客户端原始请求中的推理强度。
    pub reasoning_effort: Option<String>,
    pub reasoning_preset: Option<String>,
    pub request_kind: Option<String>,
    pub subagent_kind: Option<String>,
    pub compact: bool,
}

/// Provider 实时目录编译后的单模型能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCapabilities {
    upstream_model: UpstreamModelId,
    capabilities: ModelCapabilities,
}

/// Provider 实时目录成功发布后的进程内单调代次。
///
/// 代次只表达“目录内容已经变化”，不承载模型、ETag 或 Provider 私有数据。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderCatalogGeneration(u64);

impl ProviderCatalogGeneration {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl ProviderModelCapabilities {
    #[must_use]
    pub const fn new(upstream_model: UpstreamModelId, capabilities: ModelCapabilities) -> Self {
        Self {
            upstream_model,
            capabilities,
        }
    }

    #[must_use]
    pub const fn upstream_model(&self) -> &UpstreamModelId {
        &self.upstream_model
    }

    #[must_use]
    pub const fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }
}

impl ProviderRequest {
    /// 绑定 operation 与请求计划中冻结的 Provider 候选。
    #[must_use]
    pub const fn new(operation: Operation, candidate: ProviderCandidate) -> Self {
        Self {
            operation,
            candidate,
        }
    }

    /// 返回业务 operation。
    #[must_use]
    pub const fn operation(&self) -> &Operation {
        &self.operation
    }

    /// 返回冻结 Provider 候选。
    #[must_use]
    pub const fn candidate(&self) -> &ProviderCandidate {
        &self.candidate
    }
}

impl fmt::Debug for ProviderRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRequest")
            .field("operation", &self.operation)
            .field("provider", self.candidate.provider())
            .field("upstream_model", self.candidate.upstream_model())
            .finish()
    }
}

/// Provider 热路径唯一接口。
///
/// 每次 `execute` 只能选择一个 credential 并准备一次可见上游调用。实现不得
/// 在内部轮换 credential 或隐藏业务 retry；失败后由 Attempt Coordinator 使用
/// 新的 attempt 再次调用。
#[async_trait]
pub trait Provider: Send + Sync {
    /// 返回编译期注册名称。
    fn name(&self) -> &'static str;

    /// 返回当前进程已经成功发布的目录代次。
    fn catalog_generation(&self) -> ProviderCatalogGeneration;

    /// 解释 Provider 差异化观测字段；不参与路由和传输。
    fn request_observation(&self, _operation: &Operation) -> ProviderRequestObservation {
        ProviderRequestObservation::default()
    }

    /// 查询当前 Provider 的实时模型目录，并由 Provider 自己编译能力事实。
    ///
    /// # Errors
    ///
    /// 目录 transport、认证或 Provider 协议失败时返回稳定错误。
    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError>;

    /// 选择一个未被排除的资源并返回 cold [`ProviderStream`]。
    ///
    /// # Errors
    ///
    /// 没有可用资源、请求无效或在产生 stream 前失败时返回 Provider 错误。
    async fn execute(
        &self,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError>;
}

/// Provider Registry 构建错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    /// Provider 名称无效。
    #[error("invalid provider registry name")]
    InvalidName,
    /// Provider 重复注册。
    #[error("provider `{provider}` is already registered")]
    Duplicate {
        /// Provider 名称。
        provider: String,
    },
}

/// Registry 实时目录查询错误。
#[derive(Debug, Error)]
pub enum ProviderCatalogError {
    #[error("provider `{provider}` is not registered")]
    NotRegistered { provider: String },
    #[error("provider model catalog query failed")]
    Query(#[source] ProviderError),
}

/// 唯一保存 `Arc<dyn Provider>` 的异构注册表。
#[derive(Default)]
pub struct ProviderRegistryBuilder {
    providers: BTreeMap<ProviderKind, Arc<dyn Provider>>,
}

impl ProviderRegistryBuilder {
    /// 创建空 builder。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
        }
    }

    /// 注册 Provider。
    ///
    /// # Errors
    ///
    /// 名称无效或重复时返回错误。
    pub fn register(&mut self, provider: Arc<dyn Provider>) -> Result<(), RegistryError> {
        let kind = ProviderKind::new(provider.name()).map_err(|_| RegistryError::InvalidName)?;
        if self.providers.contains_key(&kind) {
            return Err(RegistryError::Duplicate {
                provider: kind.as_str().to_owned(),
            });
        }
        self.providers.insert(kind, provider);
        Ok(())
    }

    /// 冻结注册表。
    #[must_use]
    pub fn build(self) -> ProviderRegistry {
        ProviderRegistry {
            providers: Arc::new(self.providers),
        }
    }
}

/// Bootstrap 后不可变的 Provider Registry。
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: Arc<BTreeMap<ProviderKind, Arc<dyn Provider>>>,
}

impl ProviderRegistry {
    /// 从一组异构 Provider 构造冻结注册表。
    pub fn new(
        providers: impl IntoIterator<Item = Arc<dyn Provider>>,
    ) -> Result<Self, RegistryError> {
        let mut builder = ProviderRegistryBuilder::new();
        for provider in providers {
            builder.register(provider)?;
        }
        Ok(builder.build())
    }

    /// 创建 builder。
    #[must_use]
    pub const fn builder() -> ProviderRegistryBuilder {
        ProviderRegistryBuilder::new()
    }

    /// 按 Provider 名称查询 adapter。
    #[must_use]
    pub fn get(&self, provider: &ProviderKind) -> Option<&Arc<dyn Provider>> {
        self.providers.get(provider)
    }

    /// 返回全部已注册 Provider。
    pub fn provider_kinds(&self) -> impl Iterator<Item = &ProviderKind> {
        self.providers.keys()
    }

    #[must_use]
    pub fn request_observation(
        &self,
        provider: &ProviderKind,
        operation: &Operation,
    ) -> ProviderRequestObservation {
        self.providers
            .get(provider)
            .map_or_else(ProviderRequestObservation::default, |registered| {
                registered.request_observation(operation)
            })
    }

    /// 读取全部 Provider 的目录代次，用于 Core 快照稳定性校验与对账。
    #[must_use]
    pub fn catalog_generations(&self) -> BTreeMap<ProviderKind, ProviderCatalogGeneration> {
        self.providers
            .iter()
            .map(|(kind, provider)| (kind.clone(), provider.catalog_generation()))
            .collect()
    }

    /// 通过编译期 Provider adapter 查询并编译实时能力目录。
    ///
    /// # Errors
    ///
    /// Provider 未注册或目录查询失败时返回错误。
    pub async fn query_model_capabilities(
        &self,
        provider_kind: &ProviderKind,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderCatalogError> {
        let provider = self.providers.get(provider_kind).ok_or_else(|| {
            ProviderCatalogError::NotRegistered {
                provider: provider_kind.as_str().to_owned(),
            }
        })?;
        provider
            .query_model_capabilities()
            .await
            .map_err(ProviderCatalogError::Query)
    }

    /// 判断 Provider 是否已注册。
    #[must_use]
    pub fn contains(&self, provider: &ProviderKind) -> bool {
        self.providers.contains_key(provider)
    }

    /// 返回注册数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// 判断注册表是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}
