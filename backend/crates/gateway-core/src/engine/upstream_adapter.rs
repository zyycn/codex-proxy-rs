//! 内置 Provider 已选账号上的受管上游适配端口
//!
//! 适配器不注册 Provider 或选择账号
//! Provider 持有租约、解释凭据并计算费用；
//! Runtime 只实现适配器与受管网络之间的连接，返回值继续经过原生事件和结算链

use std::{fmt, sync::Arc};

use futures::future::BoxFuture;

use crate::{
    account::{CredentialRevision, OutboundProxy, ProviderAccountId},
    error::ProviderError,
    identity::ProviderKind,
    metering::{CalculatedCost, Usage},
    operation::Operation,
    routing::UpstreamModelId,
    runtime::extensions::ExtensionSetReference,
};

use super::{
    AttemptContext,
    middleware::MiddlewareHeader,
    provider::{EventStream, ProviderCallMetadata},
};

/// 发布代次内冻结的适配器选择计划；匹配只读宿主身份，不调用插件进程
pub trait UpstreamAdapterPlan: fmt::Debug + Send + Sync {
    fn select(
        &self,
        context: &AttemptContext,
        provider: &ProviderKind,
        model: &UpstreamModelId,
    ) -> Result<Option<Arc<dyn UpstreamAdapter>>, ProviderError>;
}

/// 请求同时持有计划及发布集合，防止旧集合先排空进程而中断在途调用
#[derive(Debug, Clone)]
pub struct FrozenUpstreamAdapterPlan {
    plan: Arc<dyn UpstreamAdapterPlan>,
    _generation: ExtensionSetReference,
}

impl FrozenUpstreamAdapterPlan {
    #[must_use]
    pub const fn new(
        plan: Arc<dyn UpstreamAdapterPlan>,
        generation: ExtensionSetReference,
    ) -> Self {
        Self {
            plan,
            _generation: generation,
        }
    }

    pub(super) fn select(
        &self,
        context: &AttemptContext,
        provider: &ProviderKind,
        model: &UpstreamModelId,
    ) -> Result<Option<Arc<dyn UpstreamAdapter>>, ProviderError> {
        self.plan.select(context, provider, model)
    }
}

/// 为本次调用选定的适配器，持有对应代次的运行资源
pub trait UpstreamAdapter: fmt::Debug + Send + Sync {
    /// 返回冷流
    /// 首次消费前不得调用插件、建立业务连接或发送请求
    fn execute(self: Arc<Self>, invocation: UpstreamAdapterInvocation) -> EventStream;

    /// 注册时声明的上游传输，用于宿主登记 attempt
    fn transport(&self) -> &str;
}

/// 一次执行的宿主事实；账号及 Provider 归属不可由插件返回值覆盖
pub struct UpstreamAdapterInvocation {
    pub operation: Operation,
    pub headers: Vec<MiddlewareHeader>,
    pub context: AttemptContext,
    pub metadata: ProviderCallMetadata,
    pub account: Arc<dyn UpstreamAccountConnection>,
}

/// Provider 针对已选账号租约提供的凭据与反馈端口
///
/// 实现必须持有原账号租约
/// 鉴权头只交给宿主网络边界，不能投影到插件 RPC
/// 目标授权由适配器注册与受管网络检查，凭据格式及刷新协议仍归 Provider
pub trait UpstreamAccountConnection: Send + Sync {
    fn account_id(&self) -> &ProviderAccountId;
    fn credential_revision(&self) -> CredentialRevision;
    fn authentication_kind(&self) -> &str;
    fn outbound_proxy(&self) -> Option<&OutboundProxy>;
    fn authorization(&self) -> Result<Vec<MiddlewareHeader>, ProviderError>;

    /// 复用 Provider 的价格来源；未知用量、模型或档位不构造零费用
    fn calculate_cost(&self, service_tier: Option<&str>, usage: &Usage) -> Option<CalculatedCost>;

    /// 失败的刷新、账号状态和冷却反馈仍由 Provider 处理，不由 Runtime 写账号
    fn record_failure(&self, error: ProviderError) -> BoxFuture<'_, ProviderError>;
}
