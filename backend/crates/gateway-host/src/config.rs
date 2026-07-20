//! 配置文件发现、反序列化与 Host-owned 配置校验。

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use tracing_subscriber::EnvFilter;

use crate::system_update::SystemUpdateConfig;

const CONFIG_RELATIVE_PATH: &str = "deploy/config.yaml";
const SERVER_HOST_ENV: &str = "CPR_SERVER_HOST";
const SERVER_PORT_ENV: &str = "CPR_SERVER_PORT";

/// 由组装根实现的顶层配置契约。
///
/// Host 只负责找到和解析文件；每个包的字段解释与相对路径解析
/// 由顶层配置委托给对应的包完成。
pub trait LoadableConfig: DeserializeOwned {
    fn resolve_and_validate(&mut self, source_dir: &Path) -> Result<(), ConfigError>;
}

/// 从当前目录或父目录中的 `deploy/config.yaml` 加载顶层配置。
pub fn load_config<T: LoadableConfig>() -> Result<T, ConfigError> {
    let current = env::current_dir().map_err(|_| ConfigError::CurrentDirectory)?;
    let path = discover_config_path(&current)?;
    let source_dir = path.parent().ok_or(ConfigError::InvalidConfigPath)?;
    let mut value: T = config::Config::builder()
        .add_source(config::File::from(path.as_path()).required(true))
        .build()
        .and_then(config::Config::try_deserialize)
        .map_err(|_| ConfigError::InvalidDocument { path: path.clone() })?;
    value.resolve_and_validate(source_dir)?;
    Ok(value)
}

/// Host 唯一拥有的进程配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub listen: ListenConfig,
    pub logging: LoggingConfig,
    #[serde(default)]
    pub system_update: SystemUpdateConfig,
    #[serde(default = "default_drain_timeout_seconds")]
    pub drain_timeout_seconds: u64,
    #[serde(default = "default_worker_shutdown_timeout_seconds")]
    pub worker_shutdown_timeout_seconds: u64,
}

impl HostConfig {
    pub fn resolve_and_validate(&mut self, source_dir: &Path) -> Result<(), ConfigError> {
        if let Some(host) = optional_environment_value(SERVER_HOST_ENV)? {
            self.listen.host = host;
        }
        if let Some(port) = optional_environment_value(SERVER_PORT_ENV)? {
            self.listen.port = port
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or(ConfigError::InvalidEnvironment(SERVER_PORT_ENV))?;
        }
        if self.listen.host.trim().is_empty() {
            return Err(ConfigError::InvalidField("host.listen.host"));
        }
        if self.listen.port == 0 {
            return Err(ConfigError::InvalidField("host.listen.port"));
        }
        if self.drain_timeout_seconds == 0 {
            return Err(ConfigError::InvalidField("host.drain_timeout_seconds"));
        }
        if self.worker_shutdown_timeout_seconds == 0 {
            return Err(ConfigError::InvalidField(
                "host.worker_shutdown_timeout_seconds",
            ));
        }
        self.logging.resolve_and_validate(source_dir)?;
        self.system_update.resolve_and_validate(source_dir)?;
        Ok(())
    }

    #[must_use]
    pub const fn drain_timeout(&self) -> Duration {
        Duration::from_secs(self.drain_timeout_seconds)
    }

    #[must_use]
    pub const fn worker_shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.worker_shutdown_timeout_seconds)
    }
}

/// HTTP 监听地址。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListenConfig {
    pub host: String,
    pub port: u16,
}

/// Host 结构化日志配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    pub level: String,
    pub stdout: bool,
    pub file: FileLoggingConfig,
}

impl LoggingConfig {
    fn resolve_and_validate(&mut self, source_dir: &Path) -> Result<(), ConfigError> {
        EnvFilter::try_new(&self.level)
            .map_err(|_| ConfigError::InvalidField("host.logging.level"))?;
        if !self.stdout && !self.file.enabled {
            return Err(ConfigError::InvalidField("host.logging"));
        }
        if self.file.directory.as_os_str().is_empty() {
            return Err(ConfigError::InvalidField("host.logging.file.directory"));
        }
        if self.file.retention_days == 0 {
            return Err(ConfigError::InvalidField(
                "host.logging.file.retention_days",
            ));
        }
        if self.file.max_file_size_mb == 0 {
            return Err(ConfigError::InvalidField(
                "host.logging.file.max_file_size_mb",
            ));
        }
        if self.file.max_files == 0 {
            return Err(ConfigError::InvalidField("host.logging.file.max_files"));
        }
        resolve_relative_path(source_dir, &mut self.file.directory);
        Ok(())
    }
}

/// Host 文件日志配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileLoggingConfig {
    pub enabled: bool,
    pub directory: PathBuf,
    pub retention_days: usize,
    pub max_file_size_mb: u64,
    pub max_files: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("current directory is unavailable")]
    CurrentDirectory,
    #[error("deploy/config.yaml was not found")]
    ConfigFileNotFound,
    #[error("configuration path is invalid")]
    InvalidConfigPath,
    #[error("configuration document is invalid: {path}")]
    InvalidDocument { path: PathBuf },
    #[error("configuration field is invalid: {0}")]
    InvalidField(&'static str),
    #[error("environment variable is invalid: {0}")]
    InvalidEnvironment(&'static str),
}

fn optional_environment_value(name: &'static str) -> Result<Option<String>, ConfigError> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => Err(ConfigError::InvalidEnvironment(name)),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(ConfigError::InvalidEnvironment(name)),
    }
}

fn discover_config_path(start: &Path) -> Result<PathBuf, ConfigError> {
    start
        .ancestors()
        .map(|directory| directory.join(CONFIG_RELATIVE_PATH))
        .find(|candidate| candidate.is_file())
        .ok_or(ConfigError::ConfigFileNotFound)
}

fn resolve_relative_path(base: &Path, path: &mut PathBuf) {
    if path.is_relative() {
        *path = base.join(&*path);
    }
}

const fn default_drain_timeout_seconds() -> u64 {
    30
}

const fn default_worker_shutdown_timeout_seconds() -> u64 {
    30
}
