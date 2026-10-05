//! 进程结构化日志与按日期/大小轮转的文件 writer

use std::env;
use std::io;
use std::sync::Arc;

use gateway_core::health::HealthProbe;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::{Layer as _, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::config::LoggingConfig;

mod sink;
mod writer;

use sink::{FileLogGuard, FileLogSink, LogHealth};
use writer::RotatingLogWriter;

// 文件前缀统一为 codex-proxy-rs-<kebab-case 类别>；专用 tracing target 使用 snake_case
const APPLICATION_LOG_FILE_PREFIX: &str = "codex-proxy-rs-application";

/// 内部分类标识；由 host.logging.oauth_recovery 控制是否写入独立文件
const OAUTH_RECOVERY_LOG_TARGET: &str = "oauth_recovery";
const OAUTH_RECOVERY_LOG_FILE_PREFIX: &str = "codex-proxy-rs-oauth-recovery";

/// 请求转储含完整凭据与正文，只允许在显式开启时写入独立文件
const REQUEST_DUMP_LOG_TARGET: &str = "request_dump";
const REQUEST_DUMP_LOG_FILE_PREFIX: &str = "codex-proxy-rs-request-dump";

/// 文件日志在正常退出时排空队列并等待落盘；控制台日志单独持有守卫
pub struct LogGuard {
    _stdout: Vec<WorkerGuard>,
    _files: Vec<FileLogGuard>,
    health: Arc<LogHealth>,
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("log IO failed")]
    Io(#[from] io::Error),
    #[error("logging filter is invalid")]
    InvalidFilter,
    #[error("global tracing subscriber is already initialized")]
    AlreadyInitialized,
    #[error("logging size limit is too large")]
    SizeOverflow,
}

/// 初始化日志；文件按完整日期留存，不以分片数量淘汰窗口内记录
pub fn initialize_logging(
    config: &LoggingConfig,
    timezone: gateway_core::time::DeploymentTimeZone,
) -> Result<LogGuard, LogError> {
    let directive = env::var("RUST_LOG").unwrap_or_else(|_| config.level.clone());
    let file_filter = application_file_filter(&directive)?;
    let recovery_file_filter = oauth_recovery_file_filter();
    let request_dump_file_filter = request_dump_file_filter();
    let stdout_filter =
        EnvFilter::try_new(stdout_filter_directive(&directive, config.file.enabled))
            .map_err(|_| LogError::InvalidFilter)?;
    let mut guards = Vec::new();
    let mut file_guards = Vec::new();
    let health = Arc::new(LogHealth::default());

    let stdout_writer = config.stdout.then(|| {
        let (writer, guard) = NonBlockingBuilder::default()
            .thread_name("gateway-log-stdout")
            .finish(io::stdout());
        guards.push(guard);
        writer
    });
    let file_writer = if config.file.enabled {
        let (writer, guard) = create_file_writer(
            config,
            timezone,
            config.file.retention_days,
            APPLICATION_LOG_FILE_PREFIX,
            "gateway-log-application-file",
            Arc::clone(&health),
        )?;
        file_guards.push(guard);
        Some(writer)
    } else {
        None
    };
    let recovery_file_writer = if config.oauth_recovery {
        let (writer, guard) = create_file_writer(
            config,
            timezone,
            config.file.retention_days,
            OAUTH_RECOVERY_LOG_FILE_PREFIX,
            "gateway-log-oauth-recovery-file",
            Arc::clone(&health),
        )?;
        file_guards.push(guard);
        Some(writer)
    } else {
        None
    };
    let request_dump_file_writer = if config.request_dump {
        let (writer, guard) = create_file_writer(
            config,
            timezone,
            config.request_dump_retention_days,
            REQUEST_DUMP_LOG_FILE_PREFIX,
            "gateway-log-request-dump-file",
            Arc::clone(&health),
        )?;
        file_guards.push(guard);
        Some(writer)
    } else {
        None
    };

    let stdout_layer = stdout_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .compact()
            .with_timer(DeploymentClock(timezone))
            .with_writer(writer)
            .with_target(false)
            .with_ansi(false)
            .with_filter(stdout_filter)
    });
    let file_layer = file_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_timer(DeploymentClock(timezone))
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_filter(file_filter)
    });
    let recovery_file_layer = recovery_file_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_timer(DeploymentClock(timezone))
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_filter(recovery_file_filter)
    });
    let request_dump_file_layer = request_dump_file_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_timer(DeploymentClock(timezone))
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_filter(request_dump_file_filter)
    });
    tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer)
        .with(recovery_file_layer)
        .with(request_dump_file_layer)
        .try_init()
        .map_err(|_| LogError::AlreadyInitialized)?;
    Ok(LogGuard {
        _stdout: guards,
        _files: file_guards,
        health,
    })
}

fn application_file_filter(directive: &str) -> Result<EnvFilter, LogError> {
    let recovery_directive = format!("{OAUTH_RECOVERY_LOG_TARGET}=off")
        .parse()
        .expect("static OAuth recovery log directive is valid");
    let request_dump_directive = format!("{REQUEST_DUMP_LOG_TARGET}=off")
        .parse()
        .expect("static request dump log directive is valid");
    EnvFilter::try_new(directive)
        .map(|filter| filter.add_directive(recovery_directive))
        .map(|filter| filter.add_directive(request_dump_directive))
        .map_err(|_| LogError::InvalidFilter)
}

fn oauth_recovery_file_filter() -> EnvFilter {
    format!("off,{OAUTH_RECOVERY_LOG_TARGET}=info")
        .parse()
        .expect("static OAuth recovery log filter is valid")
}

fn request_dump_file_filter() -> EnvFilter {
    format!("off,{REQUEST_DUMP_LOG_TARGET}=info")
        .parse()
        .expect("static request dump log filter is valid")
}

fn stdout_filter_directive(directive: &str, persistent_log_enabled: bool) -> String {
    if !persistent_log_enabled {
        format!("{directive},{OAUTH_RECOVERY_LOG_TARGET}=off,{REQUEST_DUMP_LOG_TARGET}=off")
    } else {
        "off,gateway_startup=info,gateway_shutdown=info".to_owned()
    }
}

impl LogGuard {
    pub(crate) fn health_probe(&self) -> Arc<dyn HealthProbe> {
        self.health.clone()
    }
}

fn create_file_writer(
    config: &LoggingConfig,
    timezone: gateway_core::time::DeploymentTimeZone,
    retention_days: usize,
    prefix: &'static str,
    thread_name: &'static str,
    health: Arc<LogHealth>,
) -> Result<(FileLogSink, FileLogGuard), LogError> {
    let maximum_bytes = config
        .file
        .max_file_size_mb
        .checked_mul(1024 * 1024)
        .ok_or(LogError::SizeOverflow)?;
    let writer = RotatingLogWriter::open(
        config.file.directory.clone(),
        prefix,
        maximum_bytes,
        retention_days,
        Arc::clone(&health),
        timezone,
    )?;
    Ok(FileLogSink::spawn(writer, thread_name, health)?)
}

#[derive(Clone, Copy)]
struct DeploymentClock(gateway_core::time::DeploymentTimeZone);

impl tracing_subscriber::fmt::time::FormatTime for DeploymentClock {
    fn format_time(
        &self,
        writer: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        write!(writer, "{}", self.0.local(chrono::Utc::now()).to_rfc3339())
    }
}
