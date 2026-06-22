//! 管理端 HTTP API。

/// 账号 CRUD 处理器。
pub mod accounts;
/// 客户端 API Key 处理器。
pub mod api_keys;
/// OAuth 认证处理器。
pub mod auth;
/// 诊断处理器。
pub mod diagnostics;
/// 日志处理器。
pub mod logs;
/// 模型处理器。
pub mod models;
/// 响应封装。
pub mod response;
/// 管理端路由。
pub mod router;
/// 会话处理器。
pub mod session;
/// 设置处理器。
pub mod settings;
/// 用量处理器。
pub mod usage;
