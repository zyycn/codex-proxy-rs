//! CLI 不进入 HTTP serve，也必须接收终止信号；资源退出时停止监听任务

use gateway_core::lifecycle::CancellationToken;

pub(crate) struct SignalGuard(tokio::task::JoinHandle<()>);

impl SignalGuard {
    pub(crate) fn start(cancellation: CancellationToken) -> Self {
        Self(tokio::spawn(async move {
            crate::serve::wait_for_shutdown(&cancellation).await;
        }))
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
