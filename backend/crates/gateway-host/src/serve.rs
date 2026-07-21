//! HTTP 监听、OS signal、原子连接注册与优雅 drain。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use gateway_core::engine::CancellationToken;
use gateway_core::lifecycle::{ConnectionDraining, ConnectionGuard, ConnectionLifecycle};
use tokio::sync::Notify;

const DRAINING_BIT: usize = 1usize << (usize::BITS - 1);
const ACTIVE_MASK: usize = !DRAINING_BIT;

pub(crate) struct ConnectionTracker {
    state: Arc<ConnectionState>,
    cancellation: CancellationToken,
}

struct ConnectionState {
    value: AtomicUsize,
    idle: Notify,
}

impl ConnectionTracker {
    pub(crate) fn new(cancellation: CancellationToken) -> Self {
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

    pub(crate) async fn wait_until_idle(&self, timeout: Duration) {
        let wait = async {
            loop {
                let notified = self.state.idle.notified();
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

pub(crate) async fn serve_router(
    router: Router,
    host: &str,
    port: u16,
    cancellation: CancellationToken,
    connections: Arc<ConnectionTracker>,
    drain_timeout: Duration,
) -> Result<(), ServeError> {
    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}"))
        .await
        .map_err(ServeError::Bind)?;
    tracing::info!(target: "gateway_startup", host, port, "网关开始监听");

    let shutdown_cancellation = cancellation.clone();
    let shutdown_connections = Arc::clone(&connections);
    let shutdown = async move {
        wait_for_shutdown(&shutdown_cancellation).await;
        shutdown_connections.begin_draining();
    };
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(ServeError::Serve)?;

    connections.begin_draining();
    connections.wait_until_idle(drain_timeout).await;
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
