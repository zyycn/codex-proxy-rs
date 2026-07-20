//! 进程结构化日志与按日期/大小轮转的文件 writer。

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::config::LoggingConfig;

const LOG_FILE_PREFIX: &str = "codex-proxy-rs";

/// non-blocking 日志 writer 的进程级守卫。
pub struct LogGuard {
    _writers: Vec<WorkerGuard>,
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

/// 按自然日、单文件大小、保留天数和文件总数初始化结构化日志。
pub fn initialize_logging(config: &LoggingConfig) -> Result<LogGuard, LogError> {
    let directive = env::var("RUST_LOG").unwrap_or_else(|_| config.level.clone());
    let filter = EnvFilter::try_new(directive).map_err(|_| LogError::InvalidFilter)?;
    let mut guards = Vec::new();

    let stdout_writer = config.stdout.then(|| {
        let (writer, guard) = NonBlockingBuilder::default()
            .thread_name("gateway-log-stdout")
            .finish(io::stdout());
        guards.push(guard);
        writer
    });
    let file_writer = if config.file.enabled {
        let maximum_bytes = config
            .file
            .max_file_size_mb
            .checked_mul(1024 * 1024)
            .ok_or(LogError::SizeOverflow)?;
        let writer = RotatingLogWriter::open(
            config.file.directory.clone(),
            maximum_bytes,
            config.file.retention_days,
            config.file.max_files,
        )?;
        let (writer, guard) = NonBlockingBuilder::default()
            .thread_name("gateway-log-file")
            .finish(writer);
        guards.push(guard);
        Some(writer)
    } else {
        None
    };

    let stdout_layer = stdout_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .compact()
            .with_writer(writer)
            .with_target(true)
            .with_ansi(false)
    });
    let file_layer = file_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_current_span(true)
            .with_span_list(true)
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .try_init()
        .map_err(|_| LogError::AlreadyInitialized)?;
    Ok(LogGuard { _writers: guards })
}

struct RotatingLogWriter {
    directory: PathBuf,
    maximum_bytes: u64,
    retention_days: usize,
    maximum_files: usize,
    date: chrono::NaiveDate,
    segment: usize,
    bytes_written: u64,
    file: File,
}

impl RotatingLogWriter {
    fn open(
        directory: PathBuf,
        maximum_bytes: u64,
        retention_days: usize,
        maximum_files: usize,
    ) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        let date = Utc::now().date_naive();
        cleanup_log_files(&directory, date, retention_days, maximum_files)?;
        let (segment, bytes_written) = writable_log_segment(&directory, date, maximum_bytes)?;
        let file = open_log_segment(&directory, date, segment)?;
        Ok(Self {
            directory,
            maximum_bytes,
            retention_days,
            maximum_files,
            date,
            segment,
            bytes_written,
            file,
        })
    }

    fn rotate_if_required(&mut self, incoming_bytes: usize) -> io::Result<()> {
        let date = Utc::now().date_naive();
        let incoming_bytes = u64::try_from(incoming_bytes).unwrap_or(u64::MAX);
        let day_changed = date != self.date;
        let size_exceeded = !day_changed
            && self.bytes_written > 0
            && self.bytes_written.saturating_add(incoming_bytes) > self.maximum_bytes;
        if !day_changed && !size_exceeded {
            return Ok(());
        }
        self.file.flush()?;
        if day_changed {
            self.date = date;
            self.segment = 0;
        } else {
            self.segment = self.segment.saturating_add(1);
        }
        self.file = open_log_segment(&self.directory, self.date, self.segment)?;
        self.bytes_written = self.file.metadata()?.len();
        cleanup_log_files(
            &self.directory,
            self.date,
            self.retention_days,
            self.maximum_files,
        )
    }
}

impl Write for RotatingLogWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.rotate_if_required(buffer.len())?;
        let written = self.file.write(buffer)?;
        self.bytes_written = self
            .bytes_written
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[derive(Debug)]
struct ManagedLogFile {
    date: chrono::NaiveDate,
    segment: usize,
    path: PathBuf,
}

fn writable_log_segment(
    directory: &Path,
    date: chrono::NaiveDate,
    maximum_bytes: u64,
) -> io::Result<(usize, u64)> {
    let latest = managed_log_files(directory)?
        .into_iter()
        .filter(|entry| entry.date == date)
        .max_by_key(|entry| entry.segment);
    let Some(latest) = latest else {
        return Ok((0, 0));
    };
    let length = latest.path.metadata()?.len();
    if length >= maximum_bytes {
        Ok((latest.segment.saturating_add(1), 0))
    } else {
        Ok((latest.segment, length))
    }
}

fn open_log_segment(directory: &Path, date: chrono::NaiveDate, segment: usize) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(log_file_name(date, segment)))
}

fn log_file_name(date: chrono::NaiveDate, segment: usize) -> String {
    if segment == 0 {
        format!("{LOG_FILE_PREFIX}.{date}.log")
    } else {
        format!("{LOG_FILE_PREFIX}.{date}.{segment}.log")
    }
}

fn managed_log_files(directory: &Path) -> io::Result<Vec<ManagedLogFile>> {
    Ok(fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            parse_log_file_name(entry.file_name().to_string_lossy().as_ref()).map(
                |(date, segment)| ManagedLogFile {
                    date,
                    segment,
                    path: entry.path(),
                },
            )
        })
        .collect())
}

fn parse_log_file_name(name: &str) -> Option<(chrono::NaiveDate, usize)> {
    let body = name
        .strip_prefix(&format!("{LOG_FILE_PREFIX}."))?
        .strip_suffix(".log")?;
    let (date, segment) = body.split_once('.').map_or((body, 0), |(date, segment)| {
        (date, segment.parse().ok().unwrap_or(usize::MAX))
    });
    if segment == usize::MAX {
        return None;
    }
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .map(|date| (date, segment))
}

fn cleanup_log_files(
    directory: &Path,
    today: chrono::NaiveDate,
    retention_days: usize,
    maximum_files: usize,
) -> io::Result<()> {
    let retention_offset = i64::try_from(retention_days.saturating_sub(1)).unwrap_or(i64::MAX);
    let cutoff = today
        .checked_sub_signed(chrono::Duration::days(retention_offset))
        .unwrap_or(chrono::NaiveDate::MIN);
    let mut retained = Vec::new();
    for entry in managed_log_files(directory)? {
        if entry.date < cutoff {
            fs::remove_file(entry.path)?;
        } else {
            retained.push(entry);
        }
    }
    retained.sort_by(|left, right| {
        right
            .date
            .cmp(&left.date)
            .then_with(|| right.segment.cmp(&left.segment))
    });
    for entry in retained.into_iter().skip(maximum_files) {
        fs::remove_file(entry.path)?;
    }
    Ok(())
}
