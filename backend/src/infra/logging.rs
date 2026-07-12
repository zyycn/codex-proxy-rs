//! 结构化日志初始化、输出与文件轮转。

use std::{
    backtrace::Backtrace,
    env, fmt, fs,
    fs::{File, OpenOptions},
    io::{self, Write},
    panic,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use thiserror::Error;
use tracing_appender::non_blocking::{ErrorCounter, NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::{
    EnvFilter,
    fmt::{self as tracing_fmt, format::Writer, time::FormatTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use crate::infra::time::{china_date, china_rfc3339_millis};

const LOG_PREFIX: &str = "codex-proxy-rs";
const LOG_SUFFIX: &str = "log";
const DROP_MONITOR_INTERVAL: StdDuration = StdDuration::from_secs(10);

/// 文件日志轮转配置。
#[derive(Debug, Clone)]
pub struct RotationConfig {
    /// 日志目录。
    pub directory: PathBuf,
    /// 按中国自然日计算的保留天数。
    pub retention_days: usize,
    /// 单文件大小上限。
    pub max_file_size_bytes: u64,
    /// 文件总数上限。
    pub max_files: usize,
}

impl RotationConfig {
    /// 创建轮转配置。
    pub fn new(
        directory: impl AsRef<Path>,
        retention_days: usize,
        max_file_size_bytes: u64,
        max_files: usize,
    ) -> Self {
        Self {
            directory: directory.as_ref().to_path_buf(),
            retention_days,
            max_file_size_bytes,
            max_files,
        }
    }

    fn validate(&self) -> Result<(), LogError> {
        if self.retention_days == 0 {
            return Err(LogError::InvalidConfiguration(
                "logging.file.retention_days must be greater than zero",
            ));
        }
        if self.max_file_size_bytes == 0 {
            return Err(LogError::InvalidConfiguration(
                "logging.file.max_file_size_mb must be greater than zero",
            ));
        }
        if self.max_files == 0 {
            return Err(LogError::InvalidConfiguration(
                "logging.file.max_files must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// tracing 输出配置。
#[derive(Debug, Clone)]
pub struct TracingConfig {
    /// 默认过滤级别或指令。
    pub level: String,
    /// 是否写入标准输出。
    pub stdout: bool,
    /// 可选文件输出。
    pub file: Option<RotationConfig>,
}

impl TracingConfig {
    /// 创建 tracing 输出配置。
    pub fn new(level: impl Into<String>, stdout: bool, file: Option<RotationConfig>) -> Self {
        Self {
            level: level.into(),
            stdout,
            file,
        }
    }

    fn validate(&self) -> Result<(), LogError> {
        if !self.stdout && self.file.is_none() {
            return Err(LogError::InvalidConfiguration(
                "at least one of logging.stdout or logging.file.enabled must be true",
            ));
        }
        if let Some(file) = &self.file {
            file.validate()?;
        }
        Ok(())
    }
}

/// 日志初始化错误。
#[derive(Debug, Error)]
pub enum LogError {
    /// 日志 IO 错误。
    #[error("log io error: {0}")]
    Io(#[from] io::Error),
    /// 日志配置非法。
    #[error("invalid logging configuration: {0}")]
    InvalidConfiguration(&'static str),
    /// 日志过滤指令非法。
    #[error("invalid logging filter `{filter}`: {message}")]
    InvalidFilter {
        /// 原始过滤指令。
        filter: String,
        /// 解析错误。
        message: String,
    },
    /// tracing subscriber 已初始化。
    #[error("global tracing subscriber is already initialized")]
    SubscriberAlreadyInitialized,
}

/// 日志守卫；drop 时先停止丢弃监测，再刷出两个 non-blocking writer。
pub struct LogGuard {
    _drop_monitor: DroppedLogMonitor,
    _writer_guards: Vec<WorkerGuard>,
}

/// 按中国自然日和大小写入的日志 appender。
pub struct ChinaDailyFileAppender {
    config: RotationConfig,
    current_date: String,
    current_segment: usize,
    bytes_written: u64,
    writer: File,
}

/// 构造文件 appender。
pub fn build_file_appender(config: &RotationConfig) -> Result<ChinaDailyFileAppender, LogError> {
    config.validate()?;
    fs::create_dir_all(&config.directory)?;
    ChinaDailyFileAppender::open(config.clone(), Utc::now())
}

/// 初始化 tracing。
pub fn init_tracing(config: &TracingConfig) -> Result<LogGuard, LogError> {
    config.validate()?;

    let filter_directive = env::var("RUST_LOG").unwrap_or_else(|_| config.level.clone());
    let filter =
        EnvFilter::try_new(&filter_directive).map_err(|error| LogError::InvalidFilter {
            filter: filter_directive,
            message: error.to_string(),
        })?;

    let mut writer_guards = Vec::new();
    let mut counters = Vec::new();

    let stdout_writer = config.stdout.then(|| {
        let (writer, guard) = NonBlockingBuilder::default()
            .lossy(true)
            .thread_name("tracing-stdout")
            .finish(io::stdout());
        counters.push(NamedErrorCounter::new("stdout", writer.error_counter()));
        writer_guards.push(guard);
        writer
    });

    let file_writer = config
        .file
        .as_ref()
        .map(build_file_appender)
        .transpose()?
        .map(|appender| {
            let (writer, guard) = NonBlockingBuilder::default()
                .lossy(true)
                .thread_name("tracing-file")
                .finish(appender);
            counters.push(NamedErrorCounter::new("file", writer.error_counter()));
            writer_guards.push(guard);
            writer
        });

    let stdout_layer = stdout_writer.map(json_layer);
    let file_layer = file_writer.map(json_layer);
    let drop_monitor = DroppedLogMonitor::start(counters)?;

    let init_result = tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .try_init()
        .map_err(|_| LogError::SubscriberAlreadyInitialized);
    if let Err(error) = init_result {
        drop(drop_monitor);
        drop(writer_guards);
        return Err(error);
    }
    install_panic_hook();

    Ok(LogGuard {
        _drop_monitor: drop_monitor,
        _writer_guards: writer_guards,
    })
}

fn install_panic_hook() {
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        tracing::error!(
            panic = %panic_info,
            backtrace = %Backtrace::force_capture(),
            "process panicked"
        );
        previous_hook(panic_info);
    }));
}

fn json_layer<S>(
    writer: tracing_appender::non_blocking::NonBlocking,
) -> tracing_subscriber::fmt::Layer<
    S,
    tracing_subscriber::fmt::format::JsonFields,
    tracing_subscriber::fmt::format::Format<tracing_subscriber::fmt::format::Json, ChinaLogTimer>,
    tracing_appender::non_blocking::NonBlocking,
>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
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
        .with_span_list(true)
}

impl ChinaDailyFileAppender {
    fn open(config: RotationConfig, now: DateTime<Utc>) -> Result<Self, LogError> {
        cleanup_managed_logs(&config, now)?;
        let current_date = china_date(&now);
        let (current_segment, bytes_written) = writable_segment(&config, &current_date)?;
        let writer = open_log_file(&config.directory, &current_date, current_segment)?;
        cleanup_managed_logs(&config, now)?;
        Ok(Self {
            config,
            current_date,
            current_segment,
            bytes_written,
            writer,
        })
    }

    fn rollover_if_needed(&mut self, incoming_bytes: usize) -> io::Result<()> {
        let now = Utc::now();
        let current_date = china_date(&now);
        let date_changed = current_date != self.current_date;
        let size_exceeded = !date_changed
            && self.bytes_written > 0
            && self
                .bytes_written
                .saturating_add(u64::try_from(incoming_bytes).unwrap_or(u64::MAX))
                > self.config.max_file_size_bytes;
        if !date_changed && !size_exceeded {
            return Ok(());
        }

        self.writer.flush()?;
        if date_changed {
            self.current_date = current_date;
            self.current_segment = 0;
        } else {
            self.current_segment = self.current_segment.saturating_add(1);
        }
        self.writer = open_log_file(
            &self.config.directory,
            &self.current_date,
            self.current_segment,
        )?;
        self.bytes_written = self.writer.metadata()?.len();
        cleanup_managed_logs(&self.config, now).map_err(log_error_into_io)
    }
}

impl Write for ChinaDailyFileAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.rollover_if_needed(buf.len())?;
        let written = self.writer.write(buf)?;
        self.bytes_written = self
            .bytes_written
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
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

fn writable_segment(config: &RotationConfig, date: &str) -> io::Result<(usize, u64)> {
    let latest = managed_log_files(&config.directory)?
        .into_iter()
        .filter(|file| file.date.to_string() == date)
        .max_by_key(|file| file.segment);
    let Some(latest) = latest else {
        return Ok((0, 0));
    };
    let size = latest.path.metadata()?.len();
    if size >= config.max_file_size_bytes {
        Ok((latest.segment.saturating_add(1), 0))
    } else {
        Ok((latest.segment, size))
    }
}

fn open_log_file(directory: &Path, date: &str, segment: usize) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(log_file_name(date, segment)))
}

fn log_file_name(date: &str, segment: usize) -> String {
    if segment == 0 {
        format!("{LOG_PREFIX}.{date}.{LOG_SUFFIX}")
    } else {
        format!("{LOG_PREFIX}.{date}.{segment}.{LOG_SUFFIX}")
    }
}

#[derive(Debug)]
struct ManagedLogFile {
    date: NaiveDate,
    segment: usize,
    path: PathBuf,
}

fn managed_log_files(directory: &Path) -> io::Result<Vec<ManagedLogFile>> {
    Ok(fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            parse_managed_log_name(&file_name).map(|(date, segment)| ManagedLogFile {
                date,
                segment,
                path: entry.path(),
            })
        })
        .collect())
}

fn parse_managed_log_name(file_name: &str) -> Option<(NaiveDate, usize)> {
    let body = file_name
        .strip_prefix(&format!("{LOG_PREFIX}."))?
        .strip_suffix(&format!(".{LOG_SUFFIX}"))?;
    let (date, segment) = match body.split_once('.') {
        Some((date, segment)) => (date, segment.parse().ok()?),
        None => (body, 0),
    };
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .map(|date| (date, segment))
}

fn cleanup_managed_logs(config: &RotationConfig, now: DateTime<Utc>) -> Result<(), LogError> {
    let today = NaiveDate::parse_from_str(&china_date(&now), "%Y-%m-%d")
        .map_err(|_| LogError::InvalidConfiguration("failed to derive current China date"))?;
    let retention_offset = i64::try_from(config.retention_days.saturating_sub(1))
        .map_err(|_| LogError::InvalidConfiguration("logging.file.retention_days is too large"))?;
    let cutoff = today
        .checked_sub_signed(Duration::days(retention_offset))
        .ok_or(LogError::InvalidConfiguration(
            "logging.file.retention_days is too large",
        ))?;

    let mut retained = Vec::new();
    for file in managed_log_files(&config.directory)? {
        if file.date < cutoff {
            fs::remove_file(file.path)?;
        } else {
            retained.push(file);
        }
    }
    retained.sort_by(|left, right| {
        right
            .date
            .cmp(&left.date)
            .then_with(|| right.segment.cmp(&left.segment))
    });
    for file in retained.into_iter().skip(config.max_files) {
        fs::remove_file(file.path)?;
    }
    Ok(())
}

fn log_error_into_io(error: LogError) -> io::Error {
    match error {
        LogError::Io(error) => error,
        other => io::Error::other(other),
    }
}

#[derive(Debug)]
struct NamedErrorCounter {
    output: &'static str,
    counter: ErrorCounter,
    last_reported: usize,
}

impl NamedErrorCounter {
    fn new(output: &'static str, counter: ErrorCounter) -> Self {
        Self {
            output,
            counter,
            last_reported: 0,
        }
    }
}

struct DroppedLogMonitor {
    shutdown_tx: mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}

impl DroppedLogMonitor {
    fn start(mut counters: Vec<NamedErrorCounter>) -> io::Result<Self> {
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("tracing-drop-monitor".to_string())
            .spawn(move || {
                loop {
                    match shutdown_rx.recv_timeout(DROP_MONITOR_INTERVAL) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            report_dropped_logs(&mut counters, &mut io::stderr().lock());
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            report_dropped_logs(&mut counters, &mut io::stderr().lock());
                        }
                    }
                }
            })?;
        Ok(Self {
            shutdown_tx,
            handle: Some(handle),
        })
    }
}

impl Drop for DroppedLogMonitor {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn report_dropped_logs(counters: &mut [NamedErrorCounter], writer: &mut impl Write) {
    for counter in counters {
        let total = counter.counter.dropped_lines();
        if total <= counter.last_reported {
            continue;
        }
        let dropped = total - counter.last_reported;
        counter.last_reported = total;
        let warning = serde_json::json!({
            "timestamp": china_log_timestamp(Utc::now()),
            "level": "ERROR",
            "target": "codex_proxy_rs::infra::logging",
            "fields": {
                "message": "non-blocking logging output dropped events",
                "output": counter.output,
                "dropped": dropped,
                "dropped_total": total,
            }
        });
        let _ = writeln!(writer, "{warning}");
    }
}
