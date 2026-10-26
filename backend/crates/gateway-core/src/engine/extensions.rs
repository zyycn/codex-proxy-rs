//! 执行扩展计划的冻结解析、入口请求事实与插件调用作用域

use std::{collections::BTreeSet, sync::Arc};

/// 一次请求已经进入过的插件实例集合
///
/// Core 把它随子请求传播给策略与观察边界；Runtime 只据此跳过对应
/// 实例，不能自行扩大授权或改变路由
/// 集合同时用于拒绝 A → B → A 间接递归
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionCallScope {
    instance_ids: Arc<BTreeSet<String>>,
    request: Option<Arc<RequestContext>>,
}

impl ExtensionCallScope {
    #[must_use]
    pub fn with_request(mut self, request: Arc<RequestContext>) -> Self {
        self.request = Some(request);
        self
    }

    #[must_use]
    pub fn request(&self) -> Option<&RequestContext> {
        self.request.as_deref()
    }

    #[must_use]
    pub fn shared_request(&self) -> Option<Arc<RequestContext>> {
        self.request.clone()
    }

    pub const MAXIMUM_DEPTH: usize = 4;

    #[must_use]
    pub fn contains(&self, instance_id: &str) -> bool {
        self.instance_ids.contains(instance_id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.instance_ids.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instance_ids.is_empty()
    }

    /// 返回加入当前发起实例后的新作用域；重复实例表示间接递归
    #[must_use]
    pub fn extending(&self, instance_id: String) -> Option<Self> {
        if self.contains(&instance_id) {
            return None;
        }
        let mut instance_ids = self.instance_ids.as_ref().clone();
        instance_ids.insert(instance_id);
        Some(Self {
            instance_ids: Arc::new(instance_ids),
            request: self.request.clone(),
        })
    }
}

/// 宿主签发的父调用来源；身份裁决、执行链和实际资源仍由各自 owner 持有
#[derive(Clone, Debug)]
pub struct InvocationOrigin {
    pub request_id: String,
    pub call_id: String,
    pub cancellation: crate::lifecycle::CancellationToken,
    pub extensions: ExtensionCallScope,
    pub plan: Option<super::middleware::FrozenMiddlewarePlan>,
    pub effects: Option<Arc<super::nested::ExecutionEffects>>,
}

/// 同一代次的具体执行计划；不拥有发布状态，运行资源由对应发布集合保活
#[derive(Debug, Default)]
pub struct ExecutionExtensionPlans {
    pub middleware: Option<Arc<dyn super::middleware::MiddlewarePlan>>,
    pub upstream_adapters: Option<Arc<dyn super::upstream_adapter::UpstreamAdapterPlan>>,
}

/// 同一发布代次不能被静默替换
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("execution extension generation is already registered")]
pub struct ExtensionRegistrationError;

/// 仅以弱引用按冻结身份解析执行计划，不延长旧代次寿命或决定何时发布
#[derive(Clone, Default)]
pub struct ExecutionExtensionIndex {
    sets: Arc<
        std::sync::RwLock<
            std::collections::BTreeMap<
                crate::routing::extensions::ExtensionSetId,
                std::sync::Weak<ExecutionExtensionPlans>,
            >,
        >,
    >,
}

impl ExecutionExtensionIndex {
    pub fn register(
        &self,
        id: crate::routing::extensions::ExtensionSetId,
        plans: Arc<ExecutionExtensionPlans>,
    ) -> Result<Arc<ExecutionExtensionPlans>, ExtensionRegistrationError> {
        let mut sets = self
            .sets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.retain(|_, plans| plans.strong_count() > 0);
        if sets.contains_key(&id) {
            return Err(ExtensionRegistrationError);
        }
        sets.insert(id, Arc::downgrade(&plans));
        Ok(plans)
    }

    fn resolve(
        &self,
        generation: &crate::routing::extensions::ExtensionSetReference,
    ) -> Option<Arc<ExecutionExtensionPlans>> {
        self.sets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(generation.id())
            .and_then(std::sync::Weak::upgrade)
    }

    #[must_use]
    pub fn middleware(
        &self,
        generation: &crate::routing::extensions::ExtensionSetReference,
    ) -> Option<super::middleware::FrozenMiddlewarePlan> {
        let plan = self.resolve(generation)?.middleware.clone()?;
        Some(super::middleware::FrozenMiddlewarePlan::new(
            plan,
            generation.clone(),
        ))
    }

    #[must_use]
    pub fn upstream_adapters(
        &self,
        generation: &crate::routing::extensions::ExtensionSetReference,
    ) -> Option<super::upstream_adapter::FrozenUpstreamAdapterPlan> {
        let plan = self.resolve(generation)?.upstream_adapters.clone()?;
        Some(super::upstream_adapter::FrozenUpstreamAdapterPlan::new(
            plan,
            generation.clone(),
        ))
    }
}

/// 不可变入口快照；正文由所属中间件资源持有，不随每次扩展调用复制
#[derive(Clone, PartialEq, Eq)]
pub struct RequestContext {
    pub method: String,
    pub uri: String,
    pub http_version: String,
    pub headers: Vec<super::middleware::MiddlewareHeader>,
    pub peer_addr: Option<std::net::SocketAddr>,
    pub client_ip: Option<std::net::IpAddr>,
}

impl std::fmt::Debug for RequestContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // URI 查询与请求头可能包含凭据，不进入诊断日志
        formatter
            .debug_struct("RequestContext")
            .field("method", &self.method)
            .field("http_version", &self.http_version)
            .finish_non_exhaustive()
    }
}
