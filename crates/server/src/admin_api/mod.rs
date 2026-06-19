//! 管理端 HTTP API。

/// 账号处理器。
pub mod accounts;
/// 客户端 key 处理器。
pub mod client_keys;
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

pub use response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse, PageMeta};
pub use router::router;
pub use session::require_admin_session;
