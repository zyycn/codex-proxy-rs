//! 有界文件日志队列：拥堵时背压，正常退出时排空；写入失败显式影响健康状态

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

use futures::future::BoxFuture;
use gateway_core::health::{HealthProbe, HealthState};
use tracing_subscriber::fmt::MakeWriter;

use super::writer::RotatingLogWriter;

const QUEUE_CAPACITY: usize = 64;

enum Command {
    Write(Vec<u8>),
    Shutdown,
}

#[derive(Default)]
pub(super) struct LogHealth {
    write_failures: AtomicU64,
    maintenance_failures: AtomicU64,
}

impl LogHealth {
    pub(super) fn write_failed(&self, kind: io::ErrorKind) {
        if self.write_failures.fetch_add(1, Ordering::Relaxed) == 0 {
            eprintln!("file_logging: log completeness compromised; write failure ({kind:?})");
        }
    }

    pub(super) fn maintenance_failed(&self, kind: io::ErrorKind) {
        if self.maintenance_failures.fetch_add(1, Ordering::Relaxed) == 0 {
            eprintln!("file_logging: archive/retention maintenance failed ({kind:?})");
        }
    }
}

impl HealthProbe for LogHealth {
    fn name(&self) -> &'static str {
        "file_logging"
    }

    fn check(&self) -> BoxFuture<'_, HealthState> {
        Box::pin(async move {
            let failures = self.write_failures.load(Ordering::Relaxed);
            if failures > 0 {
                return HealthState::Unhealthy(format!(
                    "log completeness compromised: {failures} write/flush failures since startup"
                ));
            }
            let failures = self.maintenance_failures.load(Ordering::Relaxed);
            if failures > 0 {
                return HealthState::Degraded(format!(
                    "log archive/retention maintenance failed {failures} times since startup"
                ));
            }
            HealthState::Healthy
        })
    }
}

#[derive(Clone)]
pub(super) struct FileLogSink {
    sender: mpsc::SyncSender<Command>,
    health: Arc<LogHealth>,
}

pub(super) struct FileLogGuard {
    sender: mpsc::SyncSender<Command>,
    worker: Option<JoinHandle<()>>,
    health: Arc<LogHealth>,
}

impl FileLogSink {
    pub(super) fn spawn(
        mut writer: RotatingLogWriter,
        name: &'static str,
        health: Arc<LogHealth>,
    ) -> io::Result<(Self, FileLogGuard)> {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let worker_health = Arc::clone(&health);
        let worker = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Write(bytes) => {
                            if let Err(error) = writer.write_all(&bytes) {
                                worker_health.write_failed(error.kind());
                            }
                        }
                        Command::Shutdown => break,
                    }
                }
                if let Err(error) = writer.sync() {
                    worker_health.write_failed(error.kind());
                }
            })?;
        Ok((
            Self {
                sender: sender.clone(),
                health: Arc::clone(&health),
            },
            FileLogGuard {
                sender,
                worker: Some(worker),
                health,
            },
        ))
    }
}

impl<'a> MakeWriter<'a> for FileLogSink {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Write for FileLogSink {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.sender
            .send(Command::Write(buffer.to_vec()))
            .map_err(|_| {
                self.health.write_failed(io::ErrorKind::BrokenPipe);
                io::Error::new(io::ErrorKind::BrokenPipe, "file log worker unavailable")
            })?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // 文件不额外缓冲，由守卫负责队列排空与同步的生命周期
        Ok(())
    }
}

impl Drop for FileLogGuard {
    fn drop(&mut self) {
        // 关闭指令排在所有已接收记录之后
        // 等待线程退出，确保正常关闭时不会因超时而丢弃队列中的日志
        let _ = self.sender.send(Command::Shutdown);
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            self.health.write_failed(io::ErrorKind::BrokenPipe);
        }
    }
}
