//! 受管子进程的创建、退出通知与回收，不解释插件或其他业务协议

use std::{num::NonZeroUsize, path::PathBuf, process::Stdio, sync::Arc};

use tokio::{
    io::AsyncReadExt as _,
    process::{ChildStdin, ChildStdout, Command},
    sync::{Semaphore, watch},
};

pub struct ProcessSpec {
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub maximum_stderr_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessExit {
    Exited(Option<i32>),
    Stopped,
    StderrLimit,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProcessStartError {
    #[error("managed process capacity exhausted")]
    Capacity,
    #[error("managed process could not be started ({kind:?}, OS {os_code:?})")]
    Spawn {
        kind: std::io::ErrorKind,
        os_code: Option<i32>,
    },
    #[error("managed process pipes unavailable")]
    Pipes,
}

/// 组合根提供共享监督入口，容量直到子进程被回收后才归还
pub struct ProcessSupervisor {
    slots: Arc<Semaphore>,
}

impl Default for ProcessSupervisor {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(128)),
        }
    }
}

impl ProcessSupervisor {
    #[must_use]
    pub fn new(maximum_processes: NonZeroUsize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(maximum_processes.get())),
        }
    }

    pub fn spawn(&self, spec: ProcessSpec) -> Result<ProcessConnection, ProcessStartError> {
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProcessStartError::Capacity)?;
        spawn_process(spec, slot)
    }
}

/// 通信资源和监督控制分离；最后一个控制引用释放时也必须回收进程
pub struct ProcessConnection {
    pub input: ChildStdin,
    pub output: ChildStdout,
    pub control: ProcessControl,
}

#[derive(Clone)]
pub struct ProcessControl {
    stop: watch::Sender<bool>,
    exit: watch::Receiver<Option<ProcessExit>>,
}

impl ProcessControl {
    pub fn stop(&self) {
        self.stop.send_replace(true);
    }

    pub async fn exited(&self) -> ProcessExit {
        let mut exit = self.exit.clone();
        loop {
            if let Some(result) = *exit.borrow_and_update() {
                return result;
            }
            if exit.changed().await.is_err() {
                return ProcessExit::Failed;
            }
        }
    }
}

/// 可信原生程序仍拥有当前 OS 用户权限；清空环境和独立进程不等于安全沙箱
fn spawn_process(
    spec: ProcessSpec,
    slot: tokio::sync::OwnedSemaphorePermit,
) -> Result<ProcessConnection, ProcessStartError> {
    let mut child = Command::new(&spec.executable)
        .current_dir(&spec.directory)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        // 只保留错误类别与 OS 编号，不把进程路径、环境或输出带入诊断
        .map_err(|error| ProcessStartError::Spawn {
            kind: error.kind(),
            os_code: error.raw_os_error(),
        })?;
    let input = child.stdin.take().ok_or(ProcessStartError::Pipes)?;
    let output = child.stdout.take().ok_or(ProcessStartError::Pipes)?;
    let mut stderr = child.stderr.take().ok_or(ProcessStartError::Pipes)?;
    let (stop, mut stopped) = watch::channel(false);
    let (exit_sender, exit) = watch::channel(None);
    tokio::spawn(async move {
        let mut stderr_total = 0usize;
        let mut buffer = [0; 4096];
        let mut stderr_open = true;
        let reason = loop {
            tokio::select! {
                biased;
                _ = stopped.changed() => break ProcessExit::Stopped,
                result = child.wait() => break result.map_or(ProcessExit::Failed, |status| ProcessExit::Exited(status.code())),
                result = stderr.read(&mut buffer), if stderr_open => {
                    match result {
                        Ok(0) => stderr_open = false,
                        Ok(size) => {
                            stderr_total = stderr_total.saturating_add(size);
                            if stderr_total > spec.maximum_stderr_bytes { break ProcessExit::StderrLimit; }
                        }
                        Err(_) => break ProcessExit::Failed,
                    }
                }
            }
        };
        // stderr 是不可信诊断，不写宿主日志；业务日志必须经过有界脱敏接口
        if !matches!(reason, ProcessExit::Exited(_)) {
            let _ = child.kill().await;
        }
        let _ = child.wait().await;
        drop(slot);
        exit_sender.send_replace(Some(reason));
    });
    Ok(ProcessConnection {
        input,
        output,
        control: ProcessControl { stop, exit },
    })
}
