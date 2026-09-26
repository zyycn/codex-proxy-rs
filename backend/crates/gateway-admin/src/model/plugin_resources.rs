//! 插件资源归属由宿主调用上下文给出，不能由插件参数选择实例或代次。

use super::Revision;

#[derive(Debug, Clone)]
pub struct PluginResourceOwner {
    pub instance_id: String,
    pub artifact_sha256: String,
    pub revision: Revision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedResource {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

pub struct ResourceMutation<T> {
    /// 没有事实变化时不递增配置 revision，也不触发发布。
    pub revision: Option<Revision>,
    pub value: T,
}

pub struct GroupMembersChange {
    pub resource_key: String,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMembersChanged {
    pub added: u64,
    pub removed: u64,
}

/// 插件 Key 的首次创建设置；分组始终从本实例资源键解析。
pub struct ManagedKeyConfig {
    pub name: String,
    pub limits: gateway_core::policy::RateLimits,
    pub budget: gateway_core::engine::budget::ClientBudgetLimits,
}
