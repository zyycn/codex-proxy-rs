//! 插件分发来源、下载凭据、发行查询与远程安装的数据合同

use std::{fmt, sync::Arc};

use chrono::{DateTime, Utc};
use gateway_core::account::OutboundProxy;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use super::{PluginArtifactMetadata, PluginSource, PluginSourceEgress};
use crate::model::Revision;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginUpdateSource {
    Builtin,
    Upload,
    Url { url: String },
    Github { repository: String },
}

/// 检查策略只决定查询目标，任何策略都不会自动安装或切换运行实例
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginUpdatePolicy {
    Manual {},
    Stable {},
    Pinned {
        tag: String,
        #[serde(default)]
        allow_prerelease: bool,
    },
}

impl Default for PluginUpdatePolicy {
    fn default() -> Self {
        Self::Manual {}
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginSourceBinding {
    pub plugin_id: String,
    pub source: PluginUpdateSource,
    #[serde(default)]
    pub policy: PluginUpdatePolicy,
    #[serde(default)]
    pub outbound_proxy_id: Option<String>,
}

#[derive(Clone)]
pub struct PluginDistributionEgress {
    pub source: PluginSourceEgress,
    pub proxy: OutboundProxy,
}

impl PluginDistributionEgress {
    #[must_use]
    pub fn new(id: String, revision: Revision, proxy: OutboundProxy) -> Self {
        Self {
            source: PluginSourceEgress {
                id,
                revision: revision.get(),
            },
            proxy,
        }
    }

    #[must_use]
    pub fn cache_key(&self) -> String {
        format!("{}:{}", self.source.id, self.source.revision)
    }
}

impl fmt::Debug for PluginDistributionEgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginDistributionEgress")
            .field("source", &self.source)
            .field("proxy", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateCheck {
    pub binding: PluginSourceBinding,
    /// Release 元数据不是已校验的插件包，不据此声明平台或 SDK 兼容
    pub release: PluginRelease,
}

impl From<&PluginSource> for PluginUpdateSource {
    fn from(source: &PluginSource) -> Self {
        match source {
            PluginSource::Builtin { .. } => Self::Builtin,
            PluginSource::Upload => Self::Upload,
            PluginSource::Url { url, .. } => Self::Url { url: url.clone() },
            PluginSource::Github { repository, .. } => Self::Github {
                repository: repository.to_ascii_lowercase(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadPurpose {
    Metadata,
    Artifact,
}

/// 授权目标以完整 origin 和路径段边界匹配；摘要、名称和列表均不包含 secret
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceCredentialInfo {
    pub id: String,
    pub name: String,
    pub origin: String,
    pub path_prefix: String,
    pub purposes: Vec<DownloadPurpose>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceAuthentication {
    Github {
        token: SecretString,
    },
    Bearer {
        token: SecretString,
    },
    Basic {
        username: String,
        password: SecretString,
    },
    Header {
        name: String,
        value: SecretString,
    },
}

#[derive(Clone)]
pub struct SourceCredential {
    pub info: SourceCredentialInfo,
    pub authentication: SourceAuthentication,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubReleaseQuery {
    pub repository: String,
    pub tag: Option<String>,
    #[serde(default)]
    pub allow_prerelease: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAsset {
    pub name: String,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRelease {
    pub repository: String,
    pub tag: String,
    pub name: String,
    pub prerelease: bool,
    pub assets: Vec<ReleaseAsset>,
    pub queried_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// 下载意图不承载凭据；引用在 Admin 中解析后只交给 Host 的本次调用
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemotePluginLocation {
    Url {
        url: String,
        sha256: Option<String>,
    },
    Github {
        repository: String,
        tag: String,
        asset: String,
        #[serde(default)]
        allow_prerelease: bool,
        sha256: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemotePluginVerify {
    pub location: RemotePluginLocation,
    #[serde(default)]
    pub credential_ids: Vec<String>,
    #[serde(default)]
    pub outbound_proxy_id: Option<String>,
    /// 更新已有插件时锁定身份；首次安装从包内清单读取
    pub expected_plugin_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemotePluginInstall {
    pub plugin_id: String,
    pub version: String,
    pub location: RemotePluginLocation,
    #[serde(default)]
    pub credential_ids: Vec<String>,
    #[serde(default)]
    pub outbound_proxy_id: Option<String>,
}

pub struct DownloadedPlugin {
    pub archive: Arc<[u8]>,
    pub sha256: String,
    pub source: PluginSource,
}

/// 只读包校验结果，不代表实例配置、授权或私有状态已完成运行时准备
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedPluginArtifact {
    pub metadata: PluginArtifactMetadata,
    pub source: PluginSource,
}
