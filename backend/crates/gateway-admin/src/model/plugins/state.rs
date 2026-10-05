//! 插件私有状态的归属、代次隔离与迁移数据合同

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

use crate::model::Revision;

/// 已由 Runtime 校验的命名空间 schema 与配额事实
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginStateSchema {
    pub namespace: String,
    pub schema_version: u32,
    pub schema_sha256: String,
    pub schema: serde_json::Value,
    pub maximum_records: u32,
    pub maximum_bytes: u64,
    pub maximum_value_bytes: u32,
    #[serde(default)]
    pub migrates_from: Vec<u32>,
}

impl fmt::Debug for PluginStateSchema {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginStateSchema")
            .field("namespace", &self.namespace)
            .field("schema_version", &self.schema_version)
            .field("schema_sha256", &self.schema_sha256)
            .field("schema", &"[REDACTED]")
            .field("maximum_records", &self.maximum_records)
            .field("maximum_bytes", &self.maximum_bytes)
            .field("maximum_value_bytes", &self.maximum_value_bytes)
            .field("migrates_from", &self.migrates_from)
            .finish()
    }
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginStateConfiguration {
    #[serde(default)]
    pub namespaces: Vec<PluginStateSchema>,
}

/// 与实例保存同事务提交的状态配置；transition 只引用已完成的 staging 数据
#[derive(Clone, Default)]
pub struct PluginStateCommit {
    pub configuration: PluginStateConfiguration,
    pub transition_id: Option<String>,
}

#[derive(Clone)]
pub struct PluginStateOwnerRequest {
    pub instance_id: String,
    pub artifact_sha256: String,
    pub instance_revision: Revision,
    pub configuration: PluginStateConfiguration,
}

/// Store 签发的实例私有状态 fence
/// 插件协议中从不传输这些字段
#[derive(Clone)]
pub struct PluginStateOwner {
    instance_id: String,
    artifact_sha256: String,
    instance_revision: Revision,
    namespaces: BTreeMap<String, PluginStateNamespaceOwner>,
}

#[derive(Clone)]
pub struct PluginStateNamespaceOwner {
    generation_id: String,
    fence: String,
    schema_version: u32,
}

impl PluginStateOwner {
    #[must_use]
    pub fn from_store(
        instance_id: String,
        artifact_sha256: String,
        instance_revision: Revision,
        namespaces: BTreeMap<String, PluginStateNamespaceOwner>,
    ) -> Self {
        Self {
            instance_id,
            artifact_sha256,
            instance_revision,
            namespaces,
        }
    }

    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    #[must_use]
    pub fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    #[must_use]
    pub const fn instance_revision(&self) -> Revision {
        self.instance_revision
    }

    #[must_use]
    pub fn namespace(&self, namespace: &str) -> Option<&PluginStateNamespaceOwner> {
        self.namespaces.get(namespace)
    }
}

impl PluginStateNamespaceOwner {
    #[must_use]
    pub fn from_store(generation_id: String, fence: String, schema_version: u32) -> Self {
        Self {
            generation_id,
            fence,
            schema_version,
        }
    }

    #[must_use]
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    #[must_use]
    pub fn fence(&self) -> &str {
        &self.fence
    }

    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }
}

/// 私有值可能含插件自己的敏感状态，故意不实现 `Debug`
#[derive(Clone)]
pub struct PluginStateRecord {
    pub key: String,
    pub value: serde_json::Value,
    pub version: u64,
    pub schema_version: u32,
}

pub struct PutPluginState {
    pub namespace: String,
    pub key: String,
    pub value: serde_json::Value,
    /// `None` 是 create-only；已有记录必须提交精确版本
    pub expected_version: Option<u64>,
}

pub struct DeletePluginState {
    pub namespace: String,
    pub key: String,
    pub expected_version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginStateWrite {
    pub version: u64,
}

#[derive(Clone)]
pub struct PluginStateTransition {
    pub id: String,
    pub instance_id: String,
    pub artifact_sha256: String,
    pub namespaces: Vec<PluginStateMigrationNamespace>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStateMigrationNamespace {
    pub namespace: String,
    pub from_schema_version: u32,
    pub to_schema_version: u32,
}

pub struct PluginStateMigrationBatch {
    pub cursor: Option<String>,
    pub records: Vec<PluginStateRecord>,
}

pub struct ApplyPluginStateMigration {
    pub transition_id: String,
    pub namespace: String,
    pub cursor: Option<String>,
    /// Runtime 刚读取的有序源键；Store 会在写入前重新读取并逐项比对
    pub expected_keys: Vec<String>,
    pub changes: Vec<PluginStateMigrationChange>,
}

pub struct PluginStateMigrationChange {
    pub key: String,
    pub action: PluginStateMigrationAction,
}

pub enum PluginStateMigrationAction {
    Keep,
    Replace(serde_json::Value),
    Delete,
}
