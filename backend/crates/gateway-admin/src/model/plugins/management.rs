//! 插件管理页面、静态资源、请求与授权回调的数据合同

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// 页面和业务调用绑定同一实例版本；制品摘要单独作为不可变资源身份
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManagementTarget {
    pub instance_id: String,
    pub artifact_sha256: String,
    pub revision: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManagementView {
    pub target: PluginManagementTarget,
    pub name: String,
    pub configuration_schema: serde_json::Value,
    pub pages: Vec<PluginManagementPage>,
    pub routes: Vec<PluginManagementRoute>,
    pub resources: Vec<PluginManagementResource>,
    pub callbacks: Vec<PluginManagementCallback>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManagementCallback {
    pub path: String,
    pub response_content_types: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartPluginManagementCallback {
    pub path: String,
    pub ttl_seconds: u32,
}

/// state 是短期一次性秘密，只交给发起登录的管理员，不进入 Debug 或日志
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManagementCallbackTicket {
    pub state: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManagementPage {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub entry: String,
    pub icon: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManagementRoute {
    pub method: String,
    pub path: String,
    pub request_content_types: Vec<String>,
    pub response_content_types: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManagementResource {
    pub path: String,
    pub content_type: String,
    pub public: bool,
}

/// 原始正文可能含业务数据，不能派生内容型 Debug 或写入诊断日志
pub struct PluginManagementRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub content_type: Option<String>,
    pub headers: Vec<gateway_core::engine::middleware::MiddlewareHeader>,
    pub body: Vec<u8>,
    pub request_id: String,
}

pub struct PluginManagementResponse {
    pub status: u16,
    pub content_type: String,
    pub headers: Vec<gateway_core::engine::middleware::MiddlewareHeader>,
    pub body: Arc<[u8]>,
}
