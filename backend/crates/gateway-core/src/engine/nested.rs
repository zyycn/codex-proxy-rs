//! 插件宿主模型调用与亲和查询的中立 Core 端口
//!
//! Runtime 只传递宿主签发的父请求关联和业务目标；调用方身份、
//! 快照、准入、续写资格与计费始终由 [`super::execution::DefaultExecutionService`]
//! 持有和复核

use std::{
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};

use futures::future::BoxFuture;

use crate::{
    account::ProviderAccountId, error::GatewayError, identity::ProviderKind,
    lifecycle::CancellationToken, operation::Operation, policy::ClientApiKeyId,
    provider_ports::ProviderSessionAffinityKey, routing::PublicModelId,
};

use super::{
    ModelRequestId,
    execution::{BoundModelExecutionContext, ExecutionRequestMetadata, StartedExecution},
};

/// 同一请求图内不可逆外部副作用的单调水位
///
/// 受信 Runtime 只在受管子执行或外部 HTTP 已发送、或无法证明未发送时调用
/// [`Self::observe`]
/// 它不承载费用或插件输入；Coordinator 用每次执行自己的基线
/// 阻止把随后 Provider 的 `not_sent` 错当作可重放
#[derive(Debug, Default)]
pub struct ExecutionEffects(AtomicUsize);

impl ExecutionEffects {
    pub fn observe(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn epoch(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}

/// 插件发起的一个子模型请求
///
/// `parent_request_id` 与 `initiating_plugin_instance_id` 都来自宿主保存的
/// `CallContext`，不能接受插件自行构造的关联事实
pub struct NestedModelExecutionRequest {
    pub parent_request_id: ModelRequestId,
    pub initiating_plugin_instance_id: String,
    pub public_model: PublicModelId,
    pub operation: Operation,
    pub metadata: ExecutionRequestMetadata,
    pub provider: Option<ProviderKind>,
    pub account: Option<ProviderAccountId>,
    /// 发起回调时父 attempt 已持有的账号
    /// 该事实由宿主从 `CallContext` 注入，
    /// 用于阻止子请求在父请求等待其返回时再次等待同一账号 lease
    pub parent_account: Option<ProviderAccountId>,
}

/// 插件为当前调用或连接显式绑定的模型执行身份
///
/// Key ID 由插件选择，发起实例由宿主确定
/// Core 在绑定时冻结当前
/// Key 策略；每个实际模型请求仍独立经过准入、预算、账本与计费
pub struct BoundModelExecutionBinding {
    pub settings: Option<crate::settings::RequestSettings>,
    pub client_key_id: ClientApiKeyId,
    pub initiating_plugin_instance_id: String,
    /// 身份随父调用或连接回收；每个模型请求使用独立的执行期限
    pub cancellation: CancellationToken,
    /// 宿主保留的扩展调用链，异步观察不能通过新建模型请求清空防递归事实
    pub extension_scope: super::extensions::ExtensionCallScope,
}

impl fmt::Debug for BoundModelExecutionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundModelExecutionBinding")
            .field("client_key_id", &self.client_key_id)
            .field(
                "initiating_plugin_instance_id",
                &self.initiating_plugin_instance_id,
            )
            .finish_non_exhaustive()
    }
}

/// 显式身份作用域内的一次模型请求
pub struct BoundModelExecutionRequest {
    pub context: BoundModelExecutionContext,
    pub public_model: PublicModelId,
    pub operation: Operation,
    pub metadata: ExecutionRequestMetadata,
    pub provider: Option<ProviderKind>,
    pub account: Option<ProviderAccountId>,
}

impl fmt::Debug for BoundModelExecutionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundModelExecutionRequest")
            .field("context", &self.context)
            .field("public_model", &self.public_model)
            .field("operation", &self.operation.kind())
            .field("provider", &self.provider)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for NestedModelExecutionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NestedModelExecutionRequest")
            .field("parent_request_id", &self.parent_request_id)
            .field(
                "initiating_plugin_instance_id",
                &self.initiating_plugin_instance_id,
            )
            .field("public_model", &self.public_model)
            .field("operation", &self.operation.kind())
            .field("provider", &self.provider)
            .field("account", &self.account)
            .field("parent_account", &self.parent_account)
            .finish_non_exhaustive()
    }
}

/// 嵌套模型执行只进入 Core 的既有执行会话；Runtime 不实现重试、commit 或计费
pub trait NestedModelExecutionPort: Send + Sync {
    fn models(
        &self,
        context: BoundModelExecutionContext,
        protocol: String,
        client_version: String,
    ) -> BoxFuture<'_, Result<Vec<PublicModelId>, GatewayError>>;

    /// 为插件调用冻结显式执行身份；取消父调用会取消其所有模型执行
    fn bind(
        &self,
        binding: BoundModelExecutionBinding,
    ) -> BoxFuture<'_, Result<BoundModelExecutionContext, GatewayError>>;

    fn start(
        &self,
        request: NestedModelExecutionRequest,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>>;

    fn start_bound(
        &self,
        request: BoundModelExecutionRequest,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>>;
}

/// 带父请求身份与授权范围的会话亲和查询
pub struct AffinityLookupRequest {
    pub parent_request_id: ModelRequestId,
    pub provider: ProviderKind,
    pub key: ProviderSessionAffinityKey,
}

impl fmt::Debug for AffinityLookupRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AffinityLookupRequest")
            .field("parent_request_id", &self.parent_request_id)
            .field("provider", &self.provider)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// 只公开后续请求可作为偏好提交、但仍须 Core 再次复核的亲和提示
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffinityLookupResult {
    provider: ProviderKind,
    account: ProviderAccountId,
}

impl AffinityLookupResult {
    #[must_use]
    pub const fn new(provider: ProviderKind, account: ProviderAccountId) -> Self {
        Self { provider, account }
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    #[must_use]
    pub const fn account(&self) -> &ProviderAccountId {
        &self.account
    }
}

/// 会话亲和查询失败沿用公开网关错误分类，不暴露响应句柄或归属细节
pub trait AffinityLookupPort: Send + Sync {
    fn lookup(
        &self,
        request: AffinityLookupRequest,
    ) -> BoxFuture<'_, Result<Option<AffinityLookupResult>, GatewayError>>;
}
