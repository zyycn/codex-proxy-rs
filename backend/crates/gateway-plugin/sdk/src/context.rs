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

/// 调用身份与入口快照；上下文不构成宿主资源的访问凭证
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallContext {
    /// 入口请求不可变快照；无入口的定时、注册和命令行调用为空
    // 帧编解码器将快照放入载荷前缀，避免大请求头占满控制元数据
    #[serde(skip)]
    pub request: Option<std::sync::Arc<RequestContext>>,
    pub call_id: u64,
    pub instance_id: String,
    pub generation: u64,
    pub incarnation: String,
    pub stage: Stage,
    /// 插件处理预算；等待宿主 middleware next 时暂停，请求总期限由宿主单独执行
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

/// 所有请求相关回调共用入口快照，保留重复请求头与非 UTF-8 字节
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestContext {
    pub method: String,
    pub uri: String,
    pub http_version: String,
    pub headers: Vec<crate::call::middleware::MiddlewareHeader>,
    pub peer_addr: Option<String>,
    pub client_ip: Option<String>,
}

impl std::fmt::Debug for RequestContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestContext")
            .field("method", &self.method)
            .field("http_version", &self.http_version)
            .finish_non_exhaustive()
    }
}
