//! 优雅关闭信号处理。

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};

use tokio::signal;
use tokio::sync::broadcast;

static SHUTDOWN_REQUESTS: OnceLock<broadcast::Sender<()>> = OnceLock::new();
static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);
static RESTART_EXECUTABLE_PATH: OnceLock<PathBuf> = OnceLock::new();

/// 等待进程关闭信号（Ctrl+C 或 SIGTERM）。
pub async fn shutdown_signal() {
    let mut requested = shutdown_sender().subscribe();
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = requested.recv() => {},
        () = ctrl_c => {},
        () = terminate => {},
    }
}

/// 请求进程按优雅关闭路径退出。
pub fn request_shutdown() {
    let _ = shutdown_sender().send(());
}

/// 请求进程完成优雅关闭后以新二进制替换当前进程。
pub fn request_process_restart(executable_path: PathBuf) {
    let _ = RESTART_EXECUTABLE_PATH.set(executable_path);
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
    request_shutdown();
}

/// 获取待 exec 的重启目标。
pub fn restart_executable_path() -> Option<PathBuf> {
    RESTART_REQUESTED
        .load(Ordering::SeqCst)
        .then(|| RESTART_EXECUTABLE_PATH.get().cloned())
        .flatten()
}

fn shutdown_sender() -> &'static broadcast::Sender<()> {
    SHUTDOWN_REQUESTS.get_or_init(|| {
        let (sender, _receiver) = broadcast::channel(16);
        sender
    })
}
