//! 配置加载与配置 schema。

#![deny(missing_docs)]

/// 配置加载器。
pub mod loader;
/// 配置文件 schema 与默认值。
pub mod schema;
/// 管理端可变设置与运行时写回服务。
pub mod settings;
