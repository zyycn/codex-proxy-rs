//! HTTP 与 WS 共用的建连资源上限，不保留出口健康或冷却状态

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};
use tokio::sync::{Semaphore, SemaphorePermit};
use tower_layer::Layer;
use tower_service::Service;

// 为单副本的文件描述符和 TLS 握手保留硬上限；不限制已经建立的长流
const ACTIVE_CONNECTIONS: usize = 128;
const WAITING_CONNECTIONS: usize = 1024;
static ACTIVE: OnceLock<Semaphore> = OnceLock::new();
static WAITING: OnceLock<Semaphore> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
#[error("local connection capacity unavailable")]
struct ConnectionAdmissionRejected;

/// 只根据自有类型识别本地容量拒绝，不将任意 IO 文案当作上游故障
pub(crate) fn is_admission_failure(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(cause) = source {
        if cause.is::<ConnectionAdmissionRejected>()
            || cause
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::get_ref)
                .is_some_and(|inner| inner.is::<ConnectionAdmissionRejected>())
        {
            return true;
        }
        source = cause.source();
    }
    false
}

fn admission_rejected() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::WouldBlock, ConnectionAdmissionRejected)
}

/// 可回退 HTTP 的 WS 快路径不排队，避免本地等待被当作上游 opening 变慢
pub(super) fn try_acquire() -> Result<SemaphorePermit<'static>, std::io::Error> {
    ACTIVE
        .get_or_init(|| Semaphore::new(ACTIVE_CONNECTIONS))
        .try_acquire()
        .map_err(|_| admission_rejected())
}

pub(super) async fn acquire() -> Result<SemaphorePermit<'static>, std::io::Error> {
    let active = ACTIVE.get_or_init(|| Semaphore::new(ACTIVE_CONNECTIONS));
    let waiting = WAITING.get_or_init(|| Semaphore::new(WAITING_CONNECTIONS));
    let queued = waiting.try_acquire().map_err(|_| admission_rejected())?;
    let permit = active
        .acquire()
        .await
        .map_err(|_| std::io::Error::other(ConnectionAdmissionRejected))?;
    drop(queued);
    Ok(permit)
}

#[derive(Clone)]
pub(super) struct ConnectionLayer;

impl<S> Layer<S> for ConnectionLayer {
    type Service = ConnectionService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        ConnectionService(inner)
    }
}

#[derive(Clone)]
pub(super) struct ConnectionService<S>(S);

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl<S, R> Service<R> for ConnectionService<S>
where
    S: Service<R, Error = BoxError>,
    S::Future: Send + 'static,
    S::Response: 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: R) -> Self::Future {
        let connection = self.0.call(request);
        Box::pin(async move {
            let _permit = acquire().await?;
            connection.await
        })
    }
}
