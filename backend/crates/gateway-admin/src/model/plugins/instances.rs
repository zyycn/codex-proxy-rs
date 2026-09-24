use std::collections::BTreeMap;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::model::Revision;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginPermissionGrant {
    pub permission: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginFailurePolicy {
    Reject,
    Delegate,
    Observe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCapabilityBinding {
    pub contribution: String,
    pub stage: String,
    pub order: i32,
    pub failure_policy: PluginFailurePolicy,
    #[serde(default)]
    pub client_key_ids: Vec<String>,
    #[serde(default)]
    pub account_group_ids: Vec<String>,
    #[serde(default)]
    pub provider_ids: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub identity_bindings: Vec<PluginFrontendIdentityBinding>,
}

/// 外部 principal 到既有 Client Key 的显式宿主映射。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginFrontendIdentityBinding {
    pub principal: String,
    pub client_key_id: String,
}

impl std::fmt::Debug for PluginFrontendIdentityBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginFrontendIdentityBinding")
            .field("principal", &"[REDACTED]")
            .field("client_key_id", &self.client_key_id)
            .finish()
    }
}

/// 配置和 secret 分离；Debug 与 Serialize 不得无意展开实例配置。
#[derive(Clone)]
pub struct PluginInstance {
    pub id: String,
    pub name: String,
    pub artifact_sha256: String,
    pub enabled: bool,
    pub trusted_process: bool,
    pub configuration: serde_json::Value,
    pub secrets: BTreeMap<String, SecretString>,
    pub grants: Vec<PluginPermissionGrant>,
    pub bindings: Vec<PluginCapabilityBinding>,
    pub revision: Revision,
}

#[derive(Clone)]
pub struct PluginInstanceSnapshot {
    pub config_revision: Revision,
    pub instances: Vec<PluginInstance>,
}

pub struct PluginInstanceMutation {
    pub config_revision: Revision,
    pub instance: PluginInstance,
}

/// 控制面展示的单实例运行状态；它描述当前进程观察到的事实，不替代持久启用配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginInstanceRuntimeStatus {
    Disabled,
    AwaitingPublication,
    Preparing,
    Running,
    Blocked,
    PreparationFailed,
    Faulted,
    Draining,
}

/// 已脱敏且可安全返回给管理员的运行失败摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstanceRuntimeFailure {
    pub code: String,
    pub message: String,
}

/// 从当前发布视图、弱代次索引和 continuation retention 合成的只读诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstanceRuntime {
    pub status: PluginInstanceRuntimeStatus,
    pub actual_revision: Option<u64>,
    pub actual_artifact_sha256: Option<String>,
    pub failure: Option<PluginInstanceRuntimeFailure>,
    pub draining_revisions: Vec<u64>,
    pub retained_continuations: u32,
}

pub struct PluginInstanceView {
    pub instance: PluginInstance,
    pub configuration_required: bool,
    pub running: bool,
    pub published_revision: Option<u64>,
    pub runtime: PluginInstanceRuntime,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurePluginInstance {
    /// 创建草稿固定一个 UUID，重试复用同一配置，不能作为编辑已有配置的入口。
    pub creation_id: Option<String>,
    /// 编辑时检查用户看到的配置版本，避免覆盖另一窗口已经保存的内容。
    pub expected_revision: Option<u64>,
    /// 管理员确认停用的同插件配置，与当前配置在同一事务中切换。
    #[serde(default)]
    pub replace_instances: Vec<PluginInstanceReplacement>,
    pub name: String,
    pub artifact_sha256: String,
    pub enabled: bool,
    pub configuration: serde_json::Value,
    /// 缺省保留旧值；显式空对象清除全部 secret。
    pub secrets: Option<BTreeMap<String, SecretString>>,
    pub bindings: Vec<PluginCapabilityBinding>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInstanceReplacement {
    pub id: String,
    pub expected_revision: u64,
}

/// 恢复已安装旧版本的配置快照，并重新通过目标版本校验。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollbackPluginInstance {
    pub artifact_sha256: String,
    pub expected_revision: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRollbackTarget {
    pub artifact_sha256: String,
    pub version: String,
    pub platforms: Vec<String>,
}

/// 静态候选不启动插件；实际回滚仍须复验配置、平台和私有状态兼容性。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRollbackPlan {
    pub instance_revision: u64,
    pub current_version: String,
    pub targets: Vec<PluginRollbackTarget>,
}

/// 版本配置快照只用于服务端恢复，敏感值不实现 Debug 或 Serialize。
#[derive(Clone)]
pub struct PluginVersionConfiguration {
    pub configuration: serde_json::Value,
    pub secrets: BTreeMap<String, SecretString>,
    pub bindings: Vec<PluginCapabilityBinding>,
}

/// 切换版本的只读草稿，不包含密钥值，也不会启动插件或写入配置。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginVersionPlan {
    pub instance_revision: u64,
    pub artifact_sha256: String,
    pub configuration: serde_json::Value,
    pub secret_fields: Vec<String>,
    pub bindings: Vec<PluginCapabilityBinding>,
    pub restored: bool,
}
