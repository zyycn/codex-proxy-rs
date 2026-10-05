//! 按大小轮转、压缩已关闭分片、按部署时区自然日期组清理

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use flate2::{Compression, write::GzEncoder};

use super::sink::LogHealth;

pub(super) struct RotatingLogWriter {
    timezone: gateway_core::time::DeploymentTimeZone,
    directory: PathBuf,
    prefix: &'static str,
    maximum_bytes: u64,
    retention_days: usize,
    date: NaiveDate,
    segment: usize,
    bytes_written: u64,
    file: File,
    health: Arc<LogHealth>,
}

impl RotatingLogWriter {
    pub(super) fn open(
        directory: PathBuf,
        prefix: &'static str,
        maximum_bytes: u64,
        retention_days: usize,
        health: Arc<LogHealth>,
        timezone: gateway_core::time::DeploymentTimeZone,
    ) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        let date = timezone.local(Utc::now()).date_naive();
        cleanup_log_files(&directory, prefix, date, retention_days, timezone)?;
        let latest = managed_log_files(&directory, prefix)?
            .into_iter()
            .filter(|entry| entry.date == date)
            .max_by_key(|entry| (entry.segment, entry.compressed));
        let (segment, bytes_written) = match latest {
            Some(entry) if !entry.compressed && entry.path.metadata()?.len() < maximum_bytes => {
                (entry.segment, entry.path.metadata()?.len())
            }
            Some(entry) => (next_segment(entry.segment)?, 0),
            None => (0, 0),
        };
        let path = directory.join(log_file_name(prefix, date, segment));
        let file = open_log_segment(&path)?;
        let writer = Self {
            timezone,
            directory,
            prefix,
            maximum_bytes,
            retention_days,
            date,
            segment,
            bytes_written,
            file,
            health,
        };
        // 重启后恢复未完成归档，正在写入的文件不参与压缩
        for entry in managed_log_files(&writer.directory, prefix)? {
            if !entry.compressed
                && entry.path != path
                && let Err(error) = compress_log_file(&entry.path)
            {
                writer.health.maintenance_failed(error.kind());
            }
        }
        Ok(writer)
    }

    fn rotate_if_required(&mut self, incoming_bytes: usize) -> io::Result<()> {
        let date = self.timezone.local(Utc::now()).date_naive();
        let day_changed = date != self.date;
        let size_exceeded = self.bytes_written > 0
            && self.bytes_written.saturating_add(incoming_bytes as u64) > self.maximum_bytes;
        if !day_changed && !size_exceeded {
            return Ok(());
        }
        let previous = self
            .directory
            .join(log_file_name(self.prefix, self.date, self.segment));
        self.file.sync_all()?;
        let segment = if day_changed {
            managed_log_files(&self.directory, self.prefix)?
                .into_iter()
                .filter(|entry| entry.date == date)
                .map(|entry| entry.segment)
                .max()
                .map_or(Ok(0), next_segment)?
        } else {
            next_segment(self.segment)?
        };
        let path = self
            .directory
            .join(log_file_name(self.prefix, date, segment));
        let file = open_log_segment(&path)?;
        // 新文件打开成功后才能提交轮转状态
        self.file = file;
        self.date = date;
        self.segment = segment;
        self.bytes_written = self.file.metadata()?.len();
        if let Err(error) = compress_log_file(&previous) {
            self.health.maintenance_failed(error.kind());
        }
        // 维护失败不能丢弃触发轮转的日志记录
        if let Err(error) = cleanup_log_files(
            &self.directory,
            self.prefix,
            date,
            self.retention_days,
            self.timezone,
        ) {
            self.health.maintenance_failed(error.kind());
        }
        Ok(())
    }

    pub(super) fn sync(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}

impl Write for RotatingLogWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.rotate_if_required(buffer.len())?;
        if let Err(error) = self.file.write_all(buffer) {
            self.bytes_written = self.file.metadata()?.len();
            return Err(error);
        }
        self.bytes_written = self.bytes_written.saturating_add(buffer.len() as u64);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn next_segment(segment: usize) -> io::Result<usize> {
    segment
        .checked_add(1)
        .ok_or_else(|| io::Error::other("log segment sequence exhausted"))
}

fn open_log_segment(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn log_file_name(prefix: &str, date: NaiveDate, segment: usize) -> String {
    if segment == 0 {
        format!("{prefix}.{date}.log")
    } else {
        format!("{prefix}.{date}.{segment}.log")
    }
}

struct ManagedLogFile {
    date: NaiveDate,
    segment: usize,
    compressed: bool,
    path: PathBuf,
}

fn managed_log_files(directory: &Path, prefix: &str) -> io::Result<Vec<ManagedLogFile>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(body) = name.strip_prefix(&format!("{prefix}.")) else {
            continue;
        };
        let (body, compressed) = match body.strip_suffix(".log.gz") {
            Some(body) => (body, true),
            None => match body.strip_suffix(".log") {
                Some(body) => (body, false),
                None => continue,
            },
        };
        let (date, segment) = body
            .split_once('.')
            .map_or((body, Some(0)), |(date, segment)| {
                (date, segment.parse::<usize>().ok())
            });
        if let (Ok(date), Some(segment)) = (NaiveDate::parse_from_str(date, "%Y-%m-%d"), segment) {
            files.push(ManagedLogFile {
                date,
                segment,
                compressed,
                path: entry.path(),
            });
        }
    }
    Ok(files)
}

fn cleanup_log_files(
    directory: &Path,
    prefix: &str,
    today: NaiveDate,
    retention_days: usize,
    timezone: gateway_core::time::DeploymentTimeZone,
) -> io::Result<()> {
    let days =
        u64::try_from(retention_days).map_err(|_| io::Error::other("log retention overflow"))?;
    let cutoff = today
        .checked_sub_days(chrono::Days::new(days))
        .unwrap_or(NaiveDate::MIN);
    let mut dates = BTreeMap::<NaiveDate, Vec<PathBuf>>::new();
    for entry in managed_log_files(directory, prefix)? {
        dates.entry(entry.date).or_default().push(entry.path);
    }
    for (date, paths) in dates {
        if date >= cutoff {
            continue;
        }
        // 用较近的文件名日期或实际写入日期保护整组，避免时区切换后提前清理
        let mut latest = date;
        for path in &paths {
            let modified: chrono::DateTime<Utc> = path.metadata()?.modified()?.into();
            latest = latest.max(timezone.local(modified).date_naive());
        }
        if latest >= cutoff {
            continue;
        }
        for path in paths {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn compress_log_file(path: &Path) -> io::Result<()> {
    let mut source = File::open(path)?;
    let modified = source.metadata()?.modified()?;
    let archive = path.with_extension("log.gz");
    let temporary = path.with_extension("log.gz.tmp");
    // 只有已关闭且由当前写入器管理的分段才能拥有此临时归档
    let output = File::create(&temporary)?;
    let mut encoder = GzEncoder::new(output, Compression::fast());
    io::copy(&mut source, &mut encoder)?;
    let output = encoder.finish()?;
    output.set_modified(modified)?;
    output.sync_all()?;
    fs::rename(&temporary, &archive)?;
    // 完整归档同步并发布后才能删除原文件
    fs::remove_file(path)
}
