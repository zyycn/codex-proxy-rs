//! 监督本进程账号租约的释放任务，关闭时取消持有请求并有界等待 Redis 清理

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::lifecycle::CancellationToken;
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
    _release: oneshot::Sender<()>,
    // 活跃请求可能比组合根活得久，不能因端口析构提前中止其续期与租约
    _cleanup: SchedulingLeaseCleanup,
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
        let (guard, dropped) = oneshot::channel::<()>();
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
            tokio::select! {
                biased;
                () = stopping.cancelled() => cancellation.cancel(),
                _ = dropped => {}
            }
            if let Err(error) = release.await {
                tracing::warn!(%error, "账号租约释放失败，依赖 TTL 回收");
            }
        });
        SchedulingLeaseGuard {
            _release: guard,
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
