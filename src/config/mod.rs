//! 配置加载与配置类型。

#![deny(missing_docs)]

/// 配置加载器。
pub mod loader;
/// 配置类型定义。
pub mod types;
/// 配置写回。
pub mod writeback;

pub use types::*;

use thiserror::Error;

/// 配置加载结果类型。
pub type ConfigResult<T> = Result<T, ::config::ConfigError>;

/// 配置写入结果类型。
pub type ConfigWriteResult<T> = Result<T, ConfigWriteError>;

/// 配置写入错误。
#[derive(Debug, Error)]
pub enum ConfigWriteError {
    /// 序列化配置覆盖失败。
    #[error("serialize config overlay")]
    Serialize(#[source] serde_yml::Error),
    /// 创建配置目录失败。
    #[error("create config directory")]
    CreateDirectory(#[source] std::io::Error),
    /// 写入配置覆盖文件失败。
    #[error("write config overlay")]
    Write(#[source] std::io::Error),
}
