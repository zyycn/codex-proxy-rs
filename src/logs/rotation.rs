use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime},
};

use chrono::Utc;
use thiserror::Error;
use tracing_subscriber::{fmt::MakeWriter, EnvFilter};

const LOG_PREFIX: &str = "codex-proxy-rs";

#[derive(Debug, Clone)]
pub struct RotationConfig {
    pub directory: PathBuf,
    pub max_file_bytes: u64,
    pub retention_days: u64,
}

impl RotationConfig {
    pub fn new(directory: impl AsRef<Path>, max_file_bytes: u64, retention_days: u64) -> Self {
        Self {
            directory: directory.as_ref().to_path_buf(),
            max_file_bytes,
            retention_days,
        }
    }
}

#[derive(Debug, Error)]
pub enum LogError {
    #[error("log io error: {0}")]
    Io(#[from] io::Error),
    #[error("global tracing subscriber is already initialized")]
    SubscriberAlreadyInitialized,
}

#[derive(Clone)]
pub struct RotatingLogWriter {
    inner: Arc<Mutex<RotatingLogInner>>,
}

impl RotatingLogWriter {
    pub fn new(config: RotationConfig) -> Result<Self, LogError> {
        fs::create_dir_all(&config.directory)?;
        let current_date = current_date();
        let active_path = active_path(&config.directory, &current_date);
        if active_path.exists()
            && fs::metadata(&active_path)?.len() >= config.max_file_bytes
            && config.max_file_bytes > 0
        {
            rotate_existing(&active_path, &current_date)?;
        }
        cleanup_retention(&config.directory, config.retention_days)?;
        let file = open_append(&active_path)?;
        let current_size = file.metadata()?.len();
        Ok(Self {
            inner: Arc::new(Mutex::new(RotatingLogInner {
                config,
                current_date,
                current_size,
                file,
            })),
        })
    }

    pub fn active_path(&self) -> PathBuf {
        let inner = self.lock_inner();
        active_path(&inner.config.directory, &inner.current_date)
    }

    fn lock_inner(&self) -> MutexGuard<'_, RotatingLogInner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl<'a> MakeWriter<'a> for RotatingLogWriter {
    type Writer = RotatingLogFile;

    fn make_writer(&'a self) -> Self::Writer {
        RotatingLogFile {
            inner: Arc::clone(&self.inner),
        }
    }
}

pub struct RotatingLogFile {
    inner: Arc<Mutex<RotatingLogInner>>,
}

impl Write for RotatingLogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.roll_if_needed(buf.len() as u64)?;
        let written = inner.file.write(buf)?;
        inner.current_size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.file.flush()
    }
}

struct RotatingLogInner {
    config: RotationConfig,
    current_date: String,
    current_size: u64,
    file: File,
}

impl RotatingLogInner {
    fn roll_if_needed(&mut self, incoming_bytes: u64) -> io::Result<()> {
        let today = current_date();
        let date_changed = today != self.current_date;
        let size_exceeded = self.config.max_file_bytes > 0
            && self.current_size > 0
            && self.current_size.saturating_add(incoming_bytes) > self.config.max_file_bytes;
        if !date_changed && !size_exceeded {
            return Ok(());
        }

        self.file.flush()?;
        let old_active_path = active_path(&self.config.directory, &self.current_date);
        if old_active_path.exists() {
            rotate_existing(&old_active_path, &self.current_date)?;
        }
        cleanup_retention(&self.config.directory, self.config.retention_days)?;
        self.current_date = today;
        let new_active_path = active_path(&self.config.directory, &self.current_date);
        self.file = open_append(&new_active_path)?;
        self.current_size = self.file.metadata()?.len();
        Ok(())
    }
}

pub fn init_tracing(config: RotationConfig) -> Result<RotatingLogWriter, LogError> {
    let writer = RotatingLogWriter::new(config)?;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // 中文注释：日志必须写入可轮转文件，不能只依赖 stdout，否则线上问题无法回溯。
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(writer.clone())
        .try_init()
        .map_err(|_| LogError::SubscriberAlreadyInitialized)?;
    Ok(writer)
}

fn current_date() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

fn active_path(directory: &Path, date: &str) -> PathBuf {
    directory.join(format!("{LOG_PREFIX}.{date}.log"))
}

fn archive_path(active_path: &Path, date: &str) -> PathBuf {
    let stamp = Utc::now().format("%H%M%S%.3f");
    let suffix = uuid::Uuid::new_v4();
    active_path.with_file_name(format!("{LOG_PREFIX}.{date}.{stamp}.{suffix}.log"))
}

fn rotate_existing(active_path: &Path, date: &str) -> io::Result<()> {
    if fs::metadata(active_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
        == 0
    {
        return Ok(());
    }
    fs::rename(active_path, archive_path(active_path, date))
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn cleanup_retention(directory: &Path, retention_days: u64) -> io::Result<()> {
    if retention_days == 0 {
        return Ok(());
    }
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(retention_days.saturating_mul(86_400)))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !file_name.starts_with(LOG_PREFIX) || !file_name.ends_with(".log") {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.modified().is_ok_and(|modified| modified < cutoff) {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}
