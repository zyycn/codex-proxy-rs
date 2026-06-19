//! 认证领域中的纯逻辑与端口。

/// OAuth token 刷新策略。
pub mod oauth;
/// 认证上游端口定义。
pub mod ports;
/// 管理员会话领域逻辑。
pub mod session;
