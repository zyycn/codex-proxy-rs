//! HTTP 监听、关闭信号与停止时的原子连接注册

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use gateway_core::lifecycle::CancellationToken;
use gateway_core::lifecycle::{ConnectionDraining, ConnectionGuard, ConnectionLifecycle};

const DRAINING_BIT: usize = 1usize << (usize::BITS - 1);
const ACTIVE_MASK: usize = !DRAINING_BIT;

/// 从使用固定延迟交接的版本升级时，旧进程可能尚未释放监听端口，
/// 因此对 AddrInUse 保留有限重试窗口
const BIND_RETRY_WINDOW: Duration = Duration::from_secs(10);
const BIND_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);

pub struct ConnectionTracker {
    state: Arc<ConnectionState>,
    cancellation: CancellationToken,
}

struct ConnectionState {
    value: AtomicUsize,
}

impl ConnectionTracker {
    #[must_use]
    pub fn new(cancellation: CancellationToken) -> Self {
        Self {
            state: Arc::new(ConnectionState {
                value: AtomicUsize::new(0),
            }),
            cancellation,
        }
    }

    pub(crate) fn begin_draining(&self) {
        self.state.value.fetch_or(DRAINING_BIT, Ordering::AcqRel);
        self.cancellation.cancel();
    }
}

impl ConnectionLifecycle for ConnectionTracker {
    fn try_register(&self) -> Result<Box<dyn ConnectionGuard>, ConnectionDraining> {
        let mut observed = self.state.value.load(Ordering::Acquire);
        loop {
            if observed & DRAINING_BIT != 0 || observed & ACTIVE_MASK == ACTIVE_MASK {
                return Err(ConnectionDraining);
            }
            match self.state.value.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(Box::new(ActiveConnection {
                        state: Arc::clone(&self.state),
                    }));
                }
                Err(actual) => observed = actual,
            }
        }
    }

    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    fn is_draining(&self) -> bool {
        self.state.value.load(Ordering::Acquire) & DRAINING_BIT != 0
    }
}

struct ActiveConnection {
    state: Arc<ConnectionState>,
}

impl ConnectionGuard for ActiveConnection {}

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.state.value.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 绑定失败即进程退出、服务彻底离线，因此 AddrInUse（典型为自重启交接
/// 时旧进程尚未关闭 listener）在窗口内指数退避重试；其他错误立即上抛
pub async fn bind_listener(address: &str) -> std::io::Result<tokio::net::TcpListener> {
    let deadline = tokio::time::Instant::now() + BIND_RETRY_WINDOW;
    let mut delay = Duration::from_millis(100);
    loop {
        match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => return Ok(listener),
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse
                    && tokio::time::Instant::now() < deadline =>
            {
                tracing::warn!(target: "gateway_startup", address, "监听地址仍被占用，退避后重试绑定");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(BIND_RETRY_MAX_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) async fn serve_router(
    router: Router,
    host: &str,
    port: u16,
    cancellation: CancellationToken,
    connections: Arc<ConnectionTracker>,
) -> Result<(), ServeError> {
    let listener = bind_listener(&format!("{host}:{port}"))
        .await
        .map_err(ServeError::Bind)?;
    tracing::info!(target: "gateway_startup", pid = std::process::id(), host, port, "网关开始监听");

    // 所有关闭入口都停止等待长请求；丢弃 serve 关闭监听，存量连接随进程退出终止
    // 后台落盘由 Host 的 Worker 关闭阶段独立收尾
    let serve = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    );
    let result = tokio::select! {
        biased;
        () = wait_for_shutdown(&cancellation) => Ok(()),
        result = serve => result.map_err(ServeError::Serve),
    };
    connections.begin_draining();
    tracing::info!(target: "gateway_shutdown", "停止等待存量连接，后台落盘完成后退出进程");
    result
}

pub(crate) async fn wait_for_shutdown(cancellation: &CancellationToken) {
    let interrupt = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => "sigint",
            Err(error) => {
                tracing::warn!(target: "gateway_shutdown", %error, "中断信号监听失败");
                "interrupt_listener_error"
            }
        }
    };
    #[cfg(unix)]
    let reason = {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        match terminate {
            Ok(mut terminate) => {
                tokio::select! {
                    reason = interrupt => reason,
                    signal = terminate.recv() => {
                        if signal.is_some() { "sigterm" } else { "terminate_listener_closed" }
                    }
                    () = cancellation.cancelled() => "internal_cancellation",
                }
            }
            Err(error) => {
                tracing::warn!(target: "gateway_shutdown", %error, "终止信号监听注册失败，仅监听中断与内部关闭请求");
                tokio::select! {
                    reason = interrupt => reason,
                    () = cancellation.cancelled() => "internal_cancellation",
                }
            }
        }
    };
    #[cfg(not(unix))]
    let reason = tokio::select! {
        reason = interrupt => reason,
        () = cancellation.cancelled() => "internal_cancellation",
    };
    tracing::info!(target: "gateway_shutdown", pid = std::process::id(), reason, "收到关闭请求");
    cancellation.cancel();
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("failed to bind HTTP listener")]
    Bind(std::io::Error),
    #[error("HTTP server failed")]
    Serve(std::io::Error),
}
