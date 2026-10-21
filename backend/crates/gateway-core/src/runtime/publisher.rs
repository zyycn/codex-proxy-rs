//! 运行时快照的原子发布、健康状态与跨进程版本收敛

use super::publications::RuntimeSnapshotHandle;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::lock::Mutex;
use futures::{FutureExt as _, Stream, StreamExt as _, pin_mut, select_biased};
use futures_timer::Delay;

use crate::lifecycle::CancellationToken;
use crate::routing::ConfigRevision;
use crate::routing::snapshot::{RuntimeSnapshotCompileError, RuntimeSnapshotCompiler};
use crate::task::{
    DaemonRestartPolicy, DaemonTask, ScheduledTask, WorkerContribution, WorkerCycleContext,
    WorkerDefinitionError, WorkerId, WorkerKind, WorkerRegistration, WorkerRunnable,
    WorkerSchedule, WorkerTaskError,
};

const RECONCILIATION_INTERVAL: Duration = Duration::from_secs(5);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAXIMUM_BACKOFF: Duration = Duration::from_secs(30);
const UNUSED_LEASE_TTL: Duration = Duration::from_secs(30);
const UNUSED_LEASE_RENEWAL: Duration = Duration::from_secs(10);

/// 不泄漏订阅基础设施细节的通知错误
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("runtime snapshot notification is unavailable")]
pub struct SnapshotSubscriptionError;

impl SnapshotSubscriptionError {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self
    }
}

/// 可丢失的配置 revision 通知流；权威 revision 始终由 Store 端口读取
pub type SnapshotRevisionStream =
    Pin<Box<dyn Stream<Item = Result<ConfigRevision, SnapshotSubscriptionError>> + Send + 'static>>;

/// 跨进程 revision 通知的基础设施中立端口
pub trait SnapshotSubscriptionPort: Send + Sync {
    fn publish_snapshot_revision(
        &self,
        revision: ConfigRevision,
    ) -> BoxFuture<'_, Result<(), SnapshotSubscriptionError>>;

    fn subscribe_snapshot_revisions(
        &self,
    ) -> BoxFuture<'_, Result<SnapshotRevisionStream, SnapshotSubscriptionError>>;
}

/// Admin 提交配置后触发本进程刷新与跨进程通知的对象安全端口
pub trait SnapshotControl: Send + Sync {
    fn publish_committed(&self, committed_revision: ConfigRevision) -> BoxFuture<'_, ()>;
}

#[derive(Default)]
struct RefreshPriorityState {
    pending_commits: usize,
    active_background: Option<CancellationToken>,
}

struct CommittedRefreshGuard {
    priority: Arc<SyncMutex<RefreshPriorityState>>,
}

impl CommittedRefreshGuard {
    fn begin(priority: Arc<SyncMutex<RefreshPriorityState>>) -> Self {
        let active = {
            let mut state = lock_unpoisoned(&priority);
            state.pending_commits += 1;
            state.active_background.clone()
        };
        // 先登记待发布提交，再取消正在等待目录的后台刷新，避免后者抢先重入
        if let Some(active) = active {
            active.cancel();
        }
        Self { priority }
    }
}

impl Drop for CommittedRefreshGuard {
    fn drop(&mut self) {
        lock_unpoisoned(&self.priority).pending_commits -= 1;
    }
}

struct BackgroundRefreshGuard {
    priority: Arc<SyncMutex<RefreshPriorityState>>,
    cancellation: CancellationToken,
}

impl BackgroundRefreshGuard {
    fn begin(priority: Arc<SyncMutex<RefreshPriorityState>>) -> Option<Self> {
        let mut state = lock_unpoisoned(&priority);
        if state.pending_commits != 0 {
            return None;
        }
        let cancellation = CancellationToken::new();
        state.active_background = Some(cancellation.clone());
        drop(state);
        Some(Self {
            priority,
            cancellation,
        })
    }
}

impl Drop for BackgroundRefreshGuard {
    fn drop(&mut self) {
        lock_unpoisoned(&self.priority).active_background = None;
    }
}

// 仅中断未完成的后台读取或编译；配置提交负责发布最新事实并发送通知
async fn preempt_for_committed_refresh<T>(
    cancellation: Option<&CancellationToken>,
    operation: impl Future<Output = T>,
) -> Result<T, RuntimeSnapshotCompileError> {
    let Some(cancellation) = cancellation else {
        return Ok(operation.await);
    };
    let cancelled = cancellation.cancelled().fuse();
    let operation = operation.fuse();
    pin_mut!(cancelled, operation);
    select_biased! {
        _ = cancelled => Err(RuntimeSnapshotCompileError::RevisionChanged),
        result = operation => Ok(result),
    }
}

/// 配置提交后的本进程快照发布与跨进程失效通知
#[derive(Clone)]
pub struct RuntimeSnapshotPublisher {
    compiler: Arc<RuntimeSnapshotCompiler>,
    snapshots: RuntimeSnapshotHandle,
    subscriptions: Arc<dyn SnapshotSubscriptionPort>,
    refresh_lock: Arc<Mutex<()>>,
    refresh_priority: Arc<SyncMutex<RefreshPriorityState>>,
}

enum RefreshMode {
    Required,
    Reconcile,
    Committed,
}

impl RuntimeSnapshotPublisher {
    #[must_use]
    pub fn new(
        compiler: Arc<RuntimeSnapshotCompiler>,
        snapshots: RuntimeSnapshotHandle,
        subscriptions: Arc<dyn SnapshotSubscriptionPort>,
    ) -> Self {
        Self {
            compiler,
            snapshots,
            subscriptions,
            refresh_lock: Arc::new(Mutex::new(())),
            refresh_priority: Arc::new(SyncMutex::new(RefreshPriorityState::default())),
        }
    }

    /// 串行重编译并替换本进程快照；无法确认配置时暂停新请求
    pub async fn refresh(&self) -> Result<ConfigRevision, RuntimeSnapshotCompileError> {
        self.refresh_with_mode(RefreshMode::Required).await
    }

    async fn refresh_with_mode(
        &self,
        mode: RefreshMode,
    ) -> Result<ConfigRevision, RuntimeSnapshotCompileError> {
        // 提交先登记并中断慢后台读取；发布/暂停仍在完整临界区内串行执行
        let _committed = matches!(mode, RefreshMode::Committed)
            .then(|| CommittedRefreshGuard::begin(Arc::clone(&self.refresh_priority)));
        let _refresh = self.refresh_lock.lock().await;
        self.refresh_locked_with_priority(mode).await
    }

    async fn refresh_locked_with_priority(
        &self,
        mode: RefreshMode,
    ) -> Result<ConfigRevision, RuntimeSnapshotCompileError> {
        if matches!(mode, RefreshMode::Committed) {
            return self.refresh_locked(mode, None).await;
        }
        let background = BackgroundRefreshGuard::begin(Arc::clone(&self.refresh_priority))
            .ok_or(RuntimeSnapshotCompileError::RevisionChanged)?;
        self.refresh_locked(mode, Some(&background.cancellation))
            .await
    }

    async fn refresh_locked(
        &self,
        mode: RefreshMode,
        cancellation: Option<&CancellationToken>,
    ) -> Result<ConfigRevision, RuntimeSnapshotCompileError> {
        let configuration_changed = if matches!(mode, RefreshMode::Reconcile) {
            let persisted_revision = preempt_for_committed_refresh(
                cancellation,
                self.compiler.store().current_config_revision(),
            )
            .await?
            .map_err(|_| {
                self.snapshots.suspend();
                RuntimeSnapshotCompileError::StoreUnavailable
            })?;
            let changed = runtime_revision_needs_refresh(
                self.published_revision().map(ConfigRevision::get),
                persisted_revision.get(),
            );
            if !changed
                && !self.provider_catalogs_need_refresh()
                && self
                    .snapshots
                    .acquire()
                    .is_ok_and(|snapshot| snapshot.extensions().is_none_or(|set| set.is_ready()))
            {
                return Ok(persisted_revision);
            }
            changed
        } else {
            true
        };
        let compiled = if matches!(mode, RefreshMode::Committed) {
            match self.snapshots.snapshot_for_diagnostics() {
                Some(previous) => self.compiler.compile_with_cached_catalog(&previous).await,
                None => self.compiler.compile().await,
            }
        } else {
            let previous = self.snapshots.snapshot_for_diagnostics();
            preempt_for_committed_refresh(cancellation, async {
                match previous {
                    Some(previous) => self.compiler.compile_with_catalog_fallback(&previous).await,
                    None => self.compiler.compile().await,
                }
            })
            .await?
        };
        let snapshot = match compiled {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // 目录对账失败可继续服务旧快照；配置或权限变化无法确认时须暂停
                if configuration_changed {
                    self.snapshots.suspend();
                }
                return Err(error);
            }
        };
        let revision = snapshot.revision();
        self.snapshots.publish(snapshot);
        Ok(revision)
    }

    #[must_use]
    pub fn published_revision(&self) -> Option<ConfigRevision> {
        self.snapshots.revision()
    }

    #[must_use]
    fn provider_catalogs_need_refresh(&self) -> bool {
        let Ok(snapshot) = self.snapshots.acquire() else {
            return true;
        };
        self.compiler.provider_catalog_generations() != *snapshot.provider_catalog_generations()
    }

    /// 数据库提交不能被目录或通知基础设施的暂时故障伪装成回滚
    async fn publish_committed_inner(&self, committed_revision: ConfigRevision) {
        if let Err(error) = self.refresh_with_mode(RefreshMode::Committed).await {
            tracing::warn!(?error, "committed runtime snapshot refresh failed");
        }
        self.notify_committed_revision(committed_revision).await;
    }

    async fn notify_committed_revision(&self, committed_revision: ConfigRevision) {
        if let Err(error) = self
            .subscriptions
            .publish_snapshot_revision(committed_revision)
            .await
        {
            tracing::warn!(
                ?error,
                revision = committed_revision.get(),
                "runtime snapshot revision notification failed"
            );
        }
    }

    /// 交给 Host 的周期对账与长驻订阅任务
    pub fn worker_contributions(&self) -> Result<Vec<WorkerContribution>, WorkerDefinitionError> {
        let reconciliation_id = WorkerId::try_new(
            WorkerKind::RuntimeSnapshotReconciliation,
            "runtime_snapshot",
        )?;
        let schedule = WorkerSchedule::try_new(
            RECONCILIATION_INTERVAL,
            INITIAL_BACKOFF,
            MAXIMUM_BACKOFF,
            UNUSED_LEASE_TTL,
            UNUSED_LEASE_RENEWAL,
        )?;
        let reconciliation = WorkerRegistration::try_new(
            reconciliation_id,
            WorkerRunnable::Scheduled {
                schedule,
                lease: None,
                task: Box::new(RuntimeSnapshotReconciliationTask {
                    publisher: self.clone(),
                }),
            },
        )?;
        let subscription_id =
            WorkerId::try_new(WorkerKind::RuntimeChangeSubscription, "runtime_snapshot")?;
        let restart = DaemonRestartPolicy::try_new(INITIAL_BACKOFF, MAXIMUM_BACKOFF)?;
        let subscription = WorkerRegistration::try_new(
            subscription_id,
            WorkerRunnable::Daemon {
                restart,
                task: Box::new(RuntimeSnapshotSubscriptionTask {
                    subscriptions: Arc::clone(&self.subscriptions),
                    publisher: self.clone(),
                }),
            },
        )?;
        Ok(vec![
            WorkerContribution::Registration(reconciliation),
            WorkerContribution::Registration(subscription),
        ])
    }
}

impl SnapshotControl for RuntimeSnapshotPublisher {
    fn publish_committed(&self, committed_revision: ConfigRevision) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.publish_committed_inner(committed_revision).await;
        })
    }
}

struct RuntimeSnapshotReconciliationTask {
    publisher: RuntimeSnapshotPublisher,
}

impl ScheduledTask for RuntimeSnapshotReconciliationTask {
    fn run_cycle(
        &self,
        _context: WorkerCycleContext,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            self.publisher
                .refresh_with_mode(RefreshMode::Reconcile)
                .await
                .map(|_| ())
                .map_err(|source| {
                    WorkerTaskError::safe("runtime snapshot reconciliation failed")
                        .with_source(source)
                })
        })
    }
}

struct RuntimeSnapshotSubscriptionTask {
    subscriptions: Arc<dyn SnapshotSubscriptionPort>,
    publisher: RuntimeSnapshotPublisher,
}

impl DaemonTask for RuntimeSnapshotSubscriptionTask {
    fn run(
        &self,
        context: crate::task::WorkerDaemonContext,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let cancellation = context.cancellation();
            let mut retry_delay = INITIAL_BACKOFF;
            loop {
                if cancellation.is_cancelled() {
                    return Ok(());
                }
                let subscription = self.subscriptions.subscribe_snapshot_revisions().await;
                let mut subscription = match subscription {
                    Ok(subscription) => {
                        retry_delay = INITIAL_BACKOFF;
                        context.succeeded();
                        subscription
                    }
                    Err(_) => {
                        wait_or_cancel(cancellation, retry_delay).await;
                        retry_delay = (retry_delay * 2).min(MAXIMUM_BACKOFF);
                        continue;
                    }
                };
                loop {
                    let cancelled = cancellation.cancelled().fuse();
                    let next = subscription.next().fuse();
                    pin_mut!(cancelled, next);
                    let notified = select_biased! {
                        _ = cancelled => return Ok(()),
                        next = next => next,
                    };
                    match notified {
                        Some(Ok(_)) => {
                            let _ = self.publisher.refresh().await;
                        }
                        Some(Err(_)) | None => break,
                    }
                }
            }
        })
    }
}

async fn wait_or_cancel(cancellation: &CancellationToken, duration: Duration) {
    let cancelled = cancellation.cancelled().fuse();
    let delay = Delay::new(duration).fuse();
    pin_mut!(cancelled, delay);
    select_biased! {
        _ = cancelled => {},
        _ = delay => {},
    }
}

/// 当前发布版本与持久版本不一致时必须重载；缺失和回退同样 fail closed
#[must_use]
pub fn runtime_revision_needs_refresh(
    published_revision: Option<u64>,
    persisted_revision: u64,
) -> bool {
    published_revision != Some(persisted_revision)
}

fn lock_unpoisoned<T>(lock: &SyncMutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
