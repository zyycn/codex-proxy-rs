//! 实例自有资源：稳定 resource_key 用于重试，名称只在首次创建时使用。

use serde::{Deserialize, Serialize};

pub const GROUP_ENSURE: &str = "host.groups.ensure";
pub const GROUP_MEMBERS: &str = "host.groups.change_members";
pub const KEY_ENSURE: &str = "host.keys.ensure";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupEnsureRequest {
    pub resource_key: String,
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupMembersChange {
    pub resource_key: String,
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyEnsureRequest {
    pub resource_key: String,
    pub name: String,
    /// 必须包含至少一个本实例分组，不能创建不受分组约束的 Key。
    pub group_resource_keys: Vec<String>,
    #[serde(default)]
    pub max_concurrency: u64,
    #[serde(default)]
    pub requests_per_minute: u64,
    pub daily_limit_usd: String,
    pub weekly_limit_usd: String,
}

/// 返回非秘密身份；调用模型时使用 id，密钥明文仍由管理员管理。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedResource {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupMembersChanged {
    pub added: u64,
    pub removed: u64,
}
