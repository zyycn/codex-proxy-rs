//! 监督本进程账号租约的释放任务，关闭时取消持有请求并有界等待 Redis 清理

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::lifecycle::CancellationToken;
use gateway_core::provider_ports::{
    ProviderLeaseGuard, ProviderStoreError, ProviderStoreErrorKind,
};
use gateway_core::task::{DaemonTask, WorkerDaemonContext, WorkerTaskError};
use tokio::sync::{RwLock, RwLockReadGuard, oneshot};
use tokio::task::JoinSet;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Default)]
pub(crate) struct SchedulingLeaseCleanup(Arc<State>);

#[derive(Default)]
struct State {
    stopping: CancellationToken,
    acquisitions: RwLock<()>,
    tasks: Mutex<JoinSet<()>>,
}

pub(crate) struct SchedulingLeaseGuard {
    release: oneshot::Sender<oneshot::Sender<Result<(), ProviderStoreError>>>,
    // 活跃请求可能比组合根活得久，不能因端口析构提前中止其续期与租约
    _cleanup: SchedulingLeaseCleanup,
}

impl ProviderLeaseGuard for SchedulingLeaseGuard {
    fn release(self: Box<Self>) -> BoxFuture<'static, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            let (ack, released) = oneshot::channel();
            self.release.send(ack).map_err(|_| {
                ProviderStoreError::new(
                    ProviderStoreErrorKind::Unavailable,
                    "request scheduling lease release",
                )
            })?;
            released.await.map_err(|source| {
                ProviderStoreError::caused_by(
                    ProviderStoreErrorKind::Unavailable,
                    "await scheduling lease release",
                    source,
                )
            })?
        })
    }
}

impl SchedulingLeaseCleanup {
    // 关闭必须等已发出的申请完成登记；不能漏掉 Redis 已创建、尚未返回的租约
    pub(crate) async fn acquire(&self) -> Option<RwLockReadGuard<'_, ()>> {
        let permit = self.0.acquisitions.read().await;
        (!self.0.stopping.is_cancelled()).then_some(permit)
    }

    pub(crate) fn track(
        &self,
        cancellation: CancellationToken,
        release: impl Future<Output = crate::StoreResult<bool>> + Send + 'static,
    ) -> SchedulingLeaseGuard {
        let (guard, dropped) =
            oneshot::channel::<oneshot::Sender<Result<(), ProviderStoreError>>>();
        let stopping = self.0.stopping.clone();
        let mut tasks = self
            .0
            .tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while let Some(result) = tasks.try_join_next() {
            report_join(result);
        }
        tasks.spawn(async move {
            let acknowledgement = tokio::select! {
                biased;
                () = stopping.cancelled() => { cancellation.cancel(); None },
                ack = dropped => ack.ok(),
            };
            let result = release.await;
            if let Err(error) = &result {
                tracing::warn!(%error, "账号租约释放失败，依赖 TTL 回收");
            }
            if let Some(ack) = acknowledgement {
                let _ = ack.send(result.map(|_| ()).map_err(|source| {
                    ProviderStoreError::caused_by(
                        ProviderStoreErrorKind::Unavailable,
                        "release scheduling lease",
                        source,
                    )
                }));
            }
        });
        SchedulingLeaseGuard {
            release: guard,
            _cleanup: self.clone(),
        }
    }

    async fn shutdown(&self) {
        self.0.stopping.cancel();
        let drain = async {
            let _closed = self.0.acquisitions.write().await;
            let mut tasks = std::mem::take(
                &mut *self
                    .0
                    .tasks
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            );
            while let Some(result) = tasks.join_next().await {
                report_join(result);
            }
        };
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, drain).await.is_err() {
            tracing::warn!("账号租约关闭清理超时，剩余租约依赖 TTL 回收");
        }
    }
}

impl DaemonTask for SchedulingLeaseCleanup {
    fn run(&self, context: WorkerDaemonContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            context.cancellation().cancelled().await;
            self.shutdown().await;
            Ok(())
        })
    }
}

fn report_join(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result {
        tracing::warn!(%error, "账号租约释放任务中断，依赖 TTL 回收");
    }
}
