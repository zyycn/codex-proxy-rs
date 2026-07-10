//! 管理员认证领域类型。

use chrono::{DateTime, Utc};
use thiserror::Error;

/// 管理员登录成功后的会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginSession {
    /// 会话 ID。
    pub session_id: String,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
}

/// 管理员会话错误。
#[derive(Debug, Error)]
pub enum SessionError {
    /// 校验管理员会话失败。
    #[error("failed to validate admin session")]
    Validate,
    /// 计算管理员密码哈希失败。
    #[error("failed to hash admin password")]
    HashPassword,
    /// 创建默认管理员失败。
    #[error("failed to create default admin user")]
    CreateAdmin,
    /// 读取管理员用户失败。
    #[error("failed to load admin user")]
    LoadAdmin,
    /// 校验管理员密码失败。
    #[error("failed to verify admin password")]
    VerifyPassword,
    /// 创建管理员会话失败。
    #[error("failed to create admin session")]
    CreateSession,
    /// 删除管理员会话失败。
    #[error("failed to delete admin session")]
    DeleteSession,
    /// 管理员登录尝试过于频繁。
    #[error("too many admin login attempts")]
    LoginThrottled,
}
