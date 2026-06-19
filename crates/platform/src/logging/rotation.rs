use std::{
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const LOG_PREFIX: &str = "codex-proxy-rs";
const LOG_SUFFIX: &str = "log";

/// 日志轮转配置。
#[derive(Debug, Clone)]
pub struct RotationConfig {
    /// 日志目录。
    pub directory: PathBuf,
    /// 保留周期。
    pub retention_days: usize,
}

impl RotationConfig {
    /// 创建轮转配置。
    pub fn new(directory: impl AsRef<Path>, retention_days: usize) -> Self {
        Self {
            directory: directory.as_ref().to_path_buf(),
            retention_days,
        }
    }
}

/// 日志初始化错误。
#[derive(Debug, Error)]
pub enum LogError {
    /// 日志 IO 错误。
    #[error("log io error: {0}")]
    Io(#[from] io::Error),
    /// appender 初始化错误。
    #[error("log appender initialization failed: {0}")]
    Appender(#[from] tracing_appender::rolling::InitError),
    /// tracing subscriber 已初始化。
    #[error("global tracing subscriber is already initialized")]
    SubscriberAlreadyInitialized,
}

/// 日志守卫。
pub struct LogGuard {
    _guard: WorkerGuard,
}

/// 构造文件 appender。
pub fn build_file_appender(config: &RotationConfig) -> Result<RollingFileAppender, LogError> {
    fs::create_dir_all(&config.directory)?;

    let builder = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix(LOG_SUFFIX);
    let builder = if config.retention_days == 0 {
        builder
    } else {
        builder.max_log_files(config.retention_days)
    };

    builder.build(&config.directory).map_err(LogError::Appender)
}

/// 初始化 tracing。
pub fn init_tracing(config: RotationConfig) -> Result<LogGuard, LogError> {
    let appender = build_file_appender(&config)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .json()
                .with_writer(writer)
                .with_target(true)
                .with_level(true)
                .with_file(true)
                .with_line_number(true)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_current_span(true)
                .with_span_list(true),
        )
        .try_init()
        .map_err(|_| LogError::SubscriberAlreadyInitialized)?;

    Ok(LogGuard { _guard: guard })
}
