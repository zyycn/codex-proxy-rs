//! HTTP 监听、OS signal、原子连接注册与优雅 drain。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Router;
use gateway_core::engine::CancellationToken;
use gateway_core::lifecycle::{ConnectionDraining, ConnectionGuard, ConnectionLifecycle};
use tokio::sync::Notify;

const DRAINING_BIT: usize = 1usize << (usize::BITS - 1);
const ACTIVE_MASK: usize = !DRAINING_BIT;

/// 自重启交接没有显式握手：替换进程按 CPR_RESTART_DELAY_MS 估算等待，
/// 可能在旧进程释放监听端口前尝试绑定，因此对 AddrInUse 保留有限重试窗口。
const BIND_RETRY_WINDOW: Duration = Duration::from_secs(10);
const BIND_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);

pub struct ConnectionTracker {
    state: Arc<ConnectionState>,
    cancellation: CancellationToken,
}

struct ConnectionState {
    value: AtomicUsize,
    idle: Notify,
}

impl ConnectionTracker {
    #[must_use]
    pub fn new(cancellation: CancellationToken) -> Self {
        Self {
            state: Arc::new(ConnectionState {
                value: AtomicUsize::new(0),
                idle: Notify::new(),
            }),
            cancellation,
        }
    }

    pub(crate) fn begin_draining(&self) {
        let previous = self.state.value.fetch_or(DRAINING_BIT, Ordering::AcqRel);
        self.cancellation.cancel();
        if previous & ACTIVE_MASK == 0 {
            self.state.idle.notify_waiters();
        }
    }

    pub async fn wait_until_idle(&self, timeout: Duration) {
        let wait = async {
            loop {
                let notified = self.state.idle.notified();
                tokio::pin!(notified);
                // notify_waiters 不保存许可：必须先 enable 注册等待者，再读
                // 活跃计数；否则最后一个 guard 在读数与首次 poll 之间 drop
                // 时唤醒丢失，只能等满整个超时。
                notified.as_mut().enable();
                if self.state.value.load(Ordering::Acquire) & ACTIVE_MASK == 0 {
                    return;
                }
                notified.await;
            }
        };
        let _ = tokio::time::timeout(timeout, wait).await;
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
        let previous = self.state.value.fetch_sub(1, Ordering::AcqRel);
        if previous & ACTIVE_MASK == 1 {
            self.state.idle.notify_waiters();
        }
    }
}

/// 绑定失败即进程退出、服务彻底离线，因此 AddrInUse（典型为自重启交接
/// 时旧进程尚未关闭 listener）在窗口内指数退避重试；其他错误立即上抛。
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
    drain_timeout: Duration,
) -> Result<(), ServeError> {
    let listener = bind_listener(&format!("{host}:{port}"))
        .await
        .map_err(ServeError::Bind)?;
    tracing::info!(target: "gateway_startup", host, port, "网关开始监听");

    let shutdown_cancellation = cancellation.clone();
    let shutdown_connections = Arc::clone(&connections);
    let shutdown = async move {
        wait_for_shutdown(&shutdown_cancellation).await;
        shutdown_connections.begin_draining();
    };
    // axum 的优雅关闭会无限等待存量连接结束，而 SSE 等长响应体不观察
    // 取消信号：慢客户端可以把关闭拖住直到被 SIGKILL。整个 drain（axum
    // 优雅关闭 + 游离 guard 等待）共享同一个从取消信号起算的绝对截止点，
    // 逾期即放弃等待，存量连接随进程退出终止。
    let drain_deadline_at = Arc::new(OnceLock::new());
    let drain_deadline = {
        let cancellation = cancellation.clone();
        let deadline_at = Arc::clone(&drain_deadline_at);
        async move {
            cancellation.cancelled().await;
            let deadline = tokio::time::Instant::now() + drain_timeout;
            let _ = deadline_at.set(deadline);
            tokio::time::sleep_until(deadline).await;
        }
    };
    let serve = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown);
    tokio::select! {
        biased;
        result = serve => {
            result.map_err(ServeError::Serve)?;
            connections.begin_draining();
            let remaining = drain_deadline_at.get().map_or(drain_timeout, |deadline| {
                deadline.saturating_duration_since(tokio::time::Instant::now())
            });
            connections.wait_until_idle(remaining).await;
        }
        () = drain_deadline => {
            tracing::warn!(
                target: "gateway_shutdown",
                timeout_secs = drain_timeout.as_secs(),
                "优雅 drain 超时，放弃等待存量连接"
            );
        }
    }
    Ok(())
}

async fn wait_for_shutdown(cancellation: &CancellationToken) {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        match terminate {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                    () = cancellation.cancelled() => return,
                }
            }
            Err(_) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    () = cancellation.cancelled() => return,
                }
            }
        }
    }
    #[cfg(not(unix))]
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = cancellation.cancelled() => return,
    }
    cancellation.cancel();
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("failed to bind HTTP listener")]
    Bind(std::io::Error),
    #[error("HTTP server failed")]
    Serve(std::io::Error),
}
