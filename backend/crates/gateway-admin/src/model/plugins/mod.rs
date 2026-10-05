//! 插件制品元数据、宿主兼容性与管理领域模型的公共入口

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

use super::Revision;

pub mod distribution;
pub mod instances;
pub mod management;
pub mod official;
pub mod state;

/// 一个宿主发行版明确承诺支持的插件能力版本
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHostCapability {
    pub capability: String,
    pub versions: Vec<u32>,
}

/// 随宿主发行物封口的插件合同；Runtime 与发布清单共用这一份声明
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHostCompatibility {
    pub schema_version: u32,
    pub manifest_schema_versions: Vec<u32>,
    pub protocol_versions: Vec<u32>,
    pub capabilities: Vec<PluginHostCapability>,
}

impl PluginHostCompatibility {
    /// 拒绝空集合、重复项和不受控标识，避免发行清单把“未知”解释为通配
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.schema_version != 2
            || !valid_versions(&self.manifest_schema_versions)
            || !valid_versions(&self.protocol_versions)
            || self.capabilities.is_empty()
            || self.capabilities.len() > 64
        {
            return false;
        }
        let mut capabilities = BTreeSet::new();
        if self.capabilities.iter().any(|entry| {
            !valid_identifier(&entry.capability)
                || !valid_versions(&entry.versions)
                || !capabilities.insert(entry.capability.as_str())
        }) {
            return false;
        }
        true
    }

    #[must_use]
    pub fn supports_capability(&self, capability: &str, version: u32) -> bool {
        self.capabilities
            .iter()
            .any(|entry| entry.capability == capability && entry.versions.contains(&version))
    }
}

fn valid_versions(versions: &[u32]) -> bool {
    !versions.is_empty()
        && versions.len() <= 16
        && versions.iter().all(|version| *version > 0)
        && versions.iter().collect::<BTreeSet<_>>().len() == versions.len()
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

pub(crate) fn valid_plugin_id(value: &str) -> bool {
    if value.len() > 64 {
        return false;
    }
    let Some((publisher, name)) = value.split_once('.') else {
        return false;
    };
    valid_plugin_segment(publisher) && valid_plugin_segment(name)
}

fn valid_plugin_segment(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// 已安装包对目标宿主的静态要求；不包含包体、配置或 secret
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCompatibilityRequirements {
    pub host_version: String,
    pub manifest_schema_version: u32,
    pub protocol_version: u32,
    pub capabilities: Vec<(String, u32)>,
}

/// 一次下载使用的非敏感出站代理身份；地址与认证由受管代理记录持有
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginSourceEgress {
    pub id: String,
    pub revision: u64,
}

/// 安装来源只保存公开定位信息，下载凭据和出站代理通过独立记录引用
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginSource {
    Builtin {
        release: String,
    },
    Upload,
    Url {
        url: String,
        credential_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outbound_proxy: Option<PluginSourceEgress>,
    },
    Github {
        repository: String,
        tag: String,
        asset: String,
        credential_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outbound_proxy: Option<PluginSourceEgress>,
    },
}

impl PluginSource {
    #[must_use]
    pub fn credential_ids(&self) -> &[String] {
        match self {
            Self::Url { credential_ids, .. } | Self::Github { credential_ids, .. } => {
                credential_ids
            }
            Self::Builtin { .. } | Self::Upload => &[],
        }
    }

    #[must_use]
    pub const fn outbound_proxy(&self) -> Option<&PluginSourceEgress> {
        match self {
            Self::Url { outbound_proxy, .. } | Self::Github { outbound_proxy, .. } => {
                outbound_proxy.as_ref()
            }
            Self::Builtin { .. } | Self::Upload => None,
        }
    }
}

/// 管理端保留的单项贡献声明；map key 是能力标识，`id` 是实例绑定引用的扩展项身份
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginContribution {
    pub id: String,
    pub version: u32,
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub input_formats: Vec<String>,
    #[serde(default)]
    pub output_formats: Vec<String>,
}

/// 制品清单中的展示图标；路径只用于选择已校验的包资源
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginArtifactIcon {
    Path(String),
    Themed(PluginArtifactIconVariants),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginArtifactIconVariants {
    pub light: String,
    pub dark: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginIconTheme {
    Light,
    Dark,
}

/// 从不可变制品中读取的已校验图标字节
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginArtifactIconResource {
    pub content_type: String,
    pub body: Vec<u8>,
}

/// 已校验制品的持久化投影；读取时忽略未知字段，可选字段缺省沿用清单语义
/// 身份与执行所需事实仍为必填，包清单及协议版本由 Runtime 独立校验
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginArtifactMetadata {
    pub plugin_id: String,
    pub version: String,
    pub name: String,
    pub display_name: String,
    pub publisher: String,
    pub author: Option<String>,
    pub description: String,
    pub license: String,
    pub sha256: String,
    pub platforms: Vec<String>,
    #[serde(default)]
    pub icon: Option<PluginArtifactIcon>,
    pub contributes: BTreeMap<String, PluginContribution>,
    #[serde(default = "empty_configuration_schema")]
    pub configuration_schema: serde_json::Value,
    #[serde(default)]
    pub secret_fields: Vec<String>,
    #[serde(default)]
    pub state_namespaces: Vec<state::PluginStateSchema>,
}

fn empty_configuration_schema() -> serde_json::Value {
    serde_json::json!({})
}

/// 由包检查端口产生的不可变制品；不派生 Debug，包内可能含敏感用户数据
#[derive(Clone)]
pub struct InspectedPluginArtifact {
    pub metadata: PluginArtifactMetadata,
    pub archive: Arc<[u8]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstalledPluginArtifact {
    pub metadata: PluginArtifactMetadata,
    pub source: PluginSource,
    pub installed_at: chrono::DateTime<chrono::Utc>,
    pub accepted_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct PluginArtifactMutation {
    pub config_revision: Revision,
    pub artifact: InstalledPluginArtifact,
}

pub struct PluginInstallResult {
    pub mutation: PluginArtifactMutation,
    pub default_instance_id: Option<String>,
    pub configuration_required: bool,
}
