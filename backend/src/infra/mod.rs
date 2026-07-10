#![deny(missing_docs)]

//! 平台基础设施层，承载身份、存储、日志和 JSON 原语。

/// PostgreSQL 连接与迁移管理。
pub mod database;
/// 展示格式化辅助。
pub mod format;
/// 身份验证原语（管理员密码、凭据生成与哈希）。
pub mod identity;
/// JSON 序列化和分页原语。
pub mod json;
/// 日志初始化和轮换。
pub mod logging;
/// 路径和安装 ID 辅助。
pub mod paths;
pub mod redis;
/// 时间格式化辅助。
pub mod time;
