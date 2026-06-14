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

#[derive(Debug, Clone)]
pub struct RotationConfig {
    pub directory: PathBuf,
    pub retention_days: usize,
}

impl RotationConfig {
    pub fn new(directory: impl AsRef<Path>, retention_days: usize) -> Self {
        Self {
            directory: directory.as_ref().to_path_buf(),
            retention_days,
        }
    }
}

#[derive(Debug, Error)]
pub enum LogError {
    #[error("log io error: {0}")]
    Io(#[from] io::Error),
    #[error("log appender initialization failed: {0}")]
    Appender(#[from] tracing_appender::rolling::InitError),
    #[error("global tracing subscriber is already initialized")]
    SubscriberAlreadyInitialized,
}

pub struct LogGuard {
    _guard: WorkerGuard,
}

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
