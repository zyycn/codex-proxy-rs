//! 配置加载与配置类型。

mod loader;
mod types;

use thiserror::Error;

pub use types::*;

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
