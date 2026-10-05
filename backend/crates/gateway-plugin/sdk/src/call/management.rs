//! 管理与 CLI 的跨进程数据；参数和命令结果只通过有界二进制载荷传输

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::host::AuthSaveRequest;

/// `management.register` 冻结的路由与页面；路径均相对插件实例命名空间
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementRegistration {
    #[serde(default)]
    pub routes: Vec<ManagementRoute>,
    #[serde(default)]
    pub resources: Vec<ManagementResource>,
    #[serde(default)]
    pub pages: Vec<ManagementPage>,
    #[serde(default)]
    pub callbacks: Vec<ManagementCallback>,
}

/// 公开登录回调；宿主签发的一次性 state 用于关联已发起的流程
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementCallback {
    pub path: String,
    pub response_content_types: Vec<String>,
}

/// 管理 handler 默认要求管理员身份；原始请求头完整传入插件
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementRoute {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub request_content_types: Vec<String>,
    pub response_content_types: Vec<String>,
}

/// 资源须在包清单 resources 中声明并校验摘要；`public` 决定是否允许未登录访问
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementResource {
    pub path: String,
    #[serde(default)]
    pub public: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementPage {
    pub id: String,
    pub title: String,
    /// 页面目录直接提供副标题，避免加载静态资源后再替换宿主标题区
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub entry: String,
    pub icon: Option<String>,
}

/// `management.handle` 元数据；原始请求体独立放在帧 payload，不进行 JSON 二次编码
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub content_type: Option<String>,
    #[serde(default)]
    pub headers: Vec<super::middleware::MiddlewareHeader>,
}

/// 原始响应体独立放在帧 payload；显式 headers 可覆盖宿主的默认响应头
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementResponse {
    pub status: u16,
    pub content_type: String,
    #[serde(default)]
    pub headers: Vec<super::middleware::MiddlewareHeader>,
}

/// `command_line.register` 的只读结果；注册和帮助查询不能执行命令或登录
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRegistration {
    pub commands: Vec<CommandDescriptor>,
}

/// 命令名只在插件实例的命名空间内生效，不注册宿主全局启动参数
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDescriptor {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<CommandParameter>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandParameter {
    pub name: String,
    pub description: String,
    pub value_type: CommandParameterType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub sensitive: bool,
    pub default: Option<CommandValue>,
}

/// `int` 固定为 32 位，duration 固定为有符号纳秒，避免随部署平台改变合同
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandParameterType {
    Bool,
    String,
    Int,
    Int64,
    Float64,
    Duration,
}

/// 参数可能含登录材料，故意不实现内容型 Debug
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CommandValue {
    Bool(bool),
    String(String),
    Int(i32),
    Int64(i64),
    Float64(f64),
    Duration(i64),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInvocation {
    pub name: String,
    pub arguments: BTreeMap<String, CommandValue>,
}

/// 输出由宿主原样交付 CLI 调用方，不进入普通诊断
/// 待保存账号仅在退出码为零时按顺序经 Admin 提交；每项独立 CAS，失败不自动重试
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
    #[serde(default)]
    pub accounts: Vec<AuthSaveRequest>,
}
