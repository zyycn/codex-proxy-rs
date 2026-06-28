//! 管理端 HTTP API。

/// 账号 CRUD 处理器。
pub mod accounts;
/// 管理端账号认证状态与本地管理员会话处理器。
pub mod auth;
/// v1 接口访问 Key 处理器。
pub mod keys;
/// 管理面日志、用量与诊断统计。
pub mod monitoring;
/// 响应封装。
pub mod response;
/// 管理端路由。
pub mod router;
/// 设置处理器。
pub mod settings;
