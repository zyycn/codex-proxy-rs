//! 日志初始化与文件轮转。

use std::{
    fmt, fs,
    fs::{File, OpenOptions},
    io,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use thiserror::Error;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    fmt::{self as tracing_fmt, format::Writer, time::FormatTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

use crate::infra::time::{china_date, china_rfc3339_millis};

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
    /// tracing subscriber 已初始化。
    #[error("global tracing subscriber is already initialized")]
    SubscriberAlreadyInitialized,
}

/// 日志守卫。
pub struct LogGuard {
    _guard: WorkerGuard,
}

/// 按中国自然日写入的日志 appender。
pub struct ChinaDailyFileAppender {
    directory: PathBuf,
    retention_days: usize,
    current_date: String,
    writer: File,
}

/// 构造文件 appender。
pub fn build_file_appender(config: &RotationConfig) -> Result<ChinaDailyFileAppender, LogError> {
    fs::create_dir_all(&config.directory)?;
    ChinaDailyFileAppender::open(&config.directory, config.retention_days, Utc::now())
}

/// 初始化 tracing。
pub fn init_tracing(config: &RotationConfig) -> Result<LogGuard, LogError> {
    let appender = build_file_appender(config)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_fmt::layer()
                .json()
                .with_timer(ChinaLogTimer)
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

impl ChinaDailyFileAppender {
    fn open(
        directory: impl AsRef<Path>,
        retention_days: usize,
        now: DateTime<Utc>,
    ) -> Result<Self, LogError> {
        let directory = directory.as_ref().to_path_buf();
        let current_date = china_date(&now);
        let writer = open_log_file(&directory, &current_date)?;
        cleanup_old_logs(&directory, retention_days)?;
        Ok(Self {
            directory,
            retention_days,
            current_date,
            writer,
        })
    }

    fn rollover_if_needed(&mut self) -> io::Result<()> {
        let current_date = china_date(&Utc::now());
        if current_date == self.current_date {
            return Ok(());
        }

        self.writer.flush()?;
        self.writer = open_log_file(&self.directory, &current_date)?;
        self.current_date = current_date;
        cleanup_old_logs(&self.directory, self.retention_days)?;
        Ok(())
    }
}

impl Write for ChinaDailyFileAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.rollover_if_needed()?;
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[derive(Debug, Clone, Copy)]
struct ChinaLogTimer;

impl FormatTime for ChinaLogTimer {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        write!(writer, "{}", china_log_timestamp(Utc::now()))
    }
}

fn china_log_timestamp(now: DateTime<Utc>) -> String {
    china_rfc3339_millis(&now)
}

fn open_log_file(directory: &Path, date: &str) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(log_file_name(date)))
}

fn log_file_name(date: &str) -> String {
    format!("{LOG_PREFIX}.{date}.{LOG_SUFFIX}")
}

fn cleanup_old_logs(directory: &Path, retention_days: usize) -> io::Result<()> {
    if retention_days == 0 {
        return Ok(());
    }

    let mut logs = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            is_managed_log_file(&file_name).then_some((file_name, entry.path()))
        })
        .collect::<Vec<_>>();
    logs.sort_by(|left, right| right.0.cmp(&left.0));

    for (_, path) in logs.into_iter().skip(retention_days) {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn is_managed_log_file(file_name: &str) -> bool {
    file_name.starts_with(&format!("{LOG_PREFIX}."))
        && file_name.ends_with(&format!(".{LOG_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn china_log_timestamp_should_use_china_offset() {
        let now = "2026-06-24T16:36:59.190910486Z"
            .parse::<DateTime<Utc>>()
            .unwrap();

        assert_eq!(china_log_timestamp(now), "2026-06-25T00:36:59.190+08:00");
    }

    #[test]
    fn log_file_name_should_use_china_date() {
        assert_eq!(log_file_name("2026-06-25"), "codex-proxy-rs.2026-06-25.log");
    }

    #[test]
    fn cleanup_old_logs_should_keep_recent_china_dates() {
        let dir = tempfile::tempdir().unwrap();
        for date in ["2026-06-23", "2026-06-24", "2026-06-25"] {
            fs::write(dir.path().join(log_file_name(date)), b"log").unwrap();
        }

        cleanup_old_logs(dir.path(), 2).unwrap();

        assert!(!dir.path().join(log_file_name("2026-06-23")).exists());
        assert!(dir.path().join(log_file_name("2026-06-24")).exists());
        assert!(dir.path().join(log_file_name("2026-06-25")).exists());
    }
}
