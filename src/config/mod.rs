//! 配置加载与配置类型。

#![deny(missing_docs)]

/// 配置加载器。
pub mod loader;
/// 管理端可变设置与运行时写回服务。
pub mod settings;
/// 配置类型定义。
pub mod types;
/// 配置写回。
pub mod writeback;

use thiserror::Error;

/// 配置加载结果类型。
pub type ConfigResult<T> = Result<T, ::config::ConfigError>;

/// 配置写入结果类型。
pub type ConfigWriteResult<T> = Result<T, ConfigWriteError>;

/// 配置写入错误。
#[derive(Debug, Error)]
pub enum ConfigWriteError {
    /// 序列化配置文件失败。
    #[error("serialize config file")]
    Serialize(#[source] serde_yml::Error),
    /// 创建配置目录失败。
    #[error("create config directory")]
    CreateDirectory(#[source] std::io::Error),
    /// 写入配置文件失败。
    #[error("write config file")]
    Write(#[source] std::io::Error),
}
