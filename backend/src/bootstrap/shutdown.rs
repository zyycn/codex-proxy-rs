//! 进程关闭与重启信号处理。

use std::{path::PathBuf, sync::OnceLock};

use tokio::signal;
use tokio::sync::broadcast;

static SHUTDOWN_REQUESTS: OnceLock<broadcast::Sender<ShutdownAction>> = OnceLock::new();

#[derive(Clone)]
pub(crate) enum ShutdownAction {
    Graceful,
    Restart(PathBuf),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeProcessControl;

impl crate::api::router::ProcessControl for RuntimeProcessControl {
    fn request_shutdown(&self) {
        request_shutdown();
    }

    fn request_restart(&self, executable_path: PathBuf) {
        request_immediate_restart(executable_path);
    }
}

/// 等待进程关闭或重启信号。
pub(crate) async fn shutdown_signal() -> ShutdownAction {
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
        action = requested.recv() => match action {
            Ok(action) => action,
            Err(_) => ShutdownAction::Graceful,
        },
        () = ctrl_c => ShutdownAction::Graceful,
        () = terminate => ShutdownAction::Graceful,
    }
}

/// 请求进程按优雅关闭路径退出。
pub fn request_shutdown() {
    let _ = shutdown_sender().send(ShutdownAction::Graceful);
}

fn request_immediate_restart(executable_path: PathBuf) {
    let _ = shutdown_sender().send(ShutdownAction::Restart(executable_path));
}

fn shutdown_sender() -> &'static broadcast::Sender<ShutdownAction> {
    SHUTDOWN_REQUESTS.get_or_init(|| {
        let (sender, _receiver) = broadcast::channel(16);
        sender
    })
}
