//! 插件会话握手与单次调用上下文，绑定制品、配置及调用身份

use serde::{Deserialize, Serialize};

use crate::{Contributions, Stage};

/// 一次会话绑定不可变制品与配置；重启必须使用新的 incarnation
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    pub protocol_version: u32,
    pub artifact_sha256: String,
    pub plugin_id: String,
    pub instance_id: String,
    pub generation: u64,
    pub incarnation: String,
    pub configuration: serde_json::Value,
    #[serde(deserialize_with = "crate::capability::deserialize_contributions")]
    pub contributes: Contributions,
}

impl std::fmt::Debug for Handshake {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Handshake")
            .field("protocol_version", &self.protocol_version)
            .field("plugin_id", &self.plugin_id)
            .field("instance_id", &self.instance_id)
            .field("generation", &self.generation)
            .field("incarnation", &self.incarnation)
            .finish_non_exhaustive()
    }
}

/// 仅携带安全关联信息；这些标识本身不是宿主资源的访问凭证
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallContext {
    pub call_id: u64,
    pub instance_id: String,
    pub generation: u64,
    pub incarnation: String,
    pub stage: Stage,
    pub timeout_ms: u64,
    /// 首个流式结果返回后由资源 owner 控制取消，不再沿用初始调用期限
    pub resource_stream: bool,
    pub resource_scope_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_revision: Option<u64>,
}
