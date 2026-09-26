//! 对账只从已发布集合开始；通知合并，实例串行，失败和停用不阻塞其他实例。

use super::{PluginRuntime, PreparedSet, RpcSession};
use futures::{StreamExt as _, future::BoxFuture};
use gateway_core::{
    lifecycle::CancellationToken,
    runtime::RuntimeSnapshotHandle,
    task::{
        DaemonRestartPolicy, DaemonTask, WorkerContribution, WorkerDefinitionError, WorkerId,
        WorkerKind, WorkerRegistration, WorkerRunnable, WorkerTaskError,
    },
};
use gateway_plugin_sdk::Stage;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Notify, task::JoinSet};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
const RETRY_DELAY: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

struct MaintenanceTask {
    runtime: Arc<PluginRuntime>,
    snapshots: RuntimeSnapshotHandle,
}

impl PluginRuntime {
    /// 由 Host 监督维护任务；CLI 和只读准备不会启动后台业务。
    pub fn maintenance_worker(
        self: &Arc<Self>,
        snapshots: RuntimeSnapshotHandle,
    ) -> Result<WorkerContribution, WorkerDefinitionError> {
        Ok(WorkerContribution::Registration(
            WorkerRegistration::try_new(
                WorkerId::try_new(
                    WorkerKind::RuntimeSnapshotReconciliation,
                    "plugin-maintenance",
                )?,
                WorkerRunnable::Daemon {
                    restart: DaemonRestartPolicy::try_new(
                        Duration::from_secs(1),
                        Duration::from_secs(30),
                    )?,
                    task: Box::new(MaintenanceTask {
                        runtime: self.clone(),
                        snapshots,
                    }),
                },
            )?,
        ))
    }
}

impl DaemonTask for MaintenanceTask {
    fn run(&self, cancellation: CancellationToken) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let mut publications = self.snapshots.subscribe_publications();
            let mut active: Option<Arc<PreparedSet>> = None;
            let mut revision = None;
            let mut stop = CancellationToken::new();
            let mut tasks = JoinSet::new();
            let mut notifications = Vec::new();
            loop {
                let snapshot = self.snapshots.acquire().ok();
                let reference = snapshot.as_ref().and_then(|s| s.extensions());
                let same_set = active.as_ref().map(|s| &s.id) == reference.map(|s| s.id());
                if !same_set {
                    stop.cancel();
                    while tasks.join_next().await.is_some() {}
                    notifications.clear();
                    stop = CancellationToken::new();
                    active = match reference {
                        Some(reference) => {
                            Some(self.runtime.prepared_set(reference).await.map_err(|_| {
                                WorkerTaskError::safe("published plugin set is unavailable")
                            })?)
                        }
                        None => None,
                    };
                    if let Some(set) = &active {
                        for instance in set.sessions.iter().filter(|i| i.maintenance) {
                            let dirty = Arc::new(Notify::new());
                            dirty.notify_one();
                            notifications.push(dirty.clone());
                            tasks.spawn(reconcile(instance.session.clone(), dirty, stop.clone()));
                        }
                    }
                } else if revision != snapshot.as_ref().map(|s| s.revision()) {
                    for dirty in &notifications {
                        dirty.notify_one();
                    }
                }
                revision = snapshot.as_ref().map(|s| s.revision());
                // 发布变更优先于下一轮维护；不在快照发布锁中等待插件或数据库。
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => break,
                    changed = publications.next() => if changed.is_none() { break; },
                    result = tasks.join_next(), if !tasks.is_empty() => {
                        if result.is_some() {
                            stop.cancel();
                            while tasks.join_next().await.is_some() {}
                            return Err(WorkerTaskError::safe("plugin maintenance task stopped unexpectedly"));
                        }
                    }
                }
            }
            stop.cancel();
            while tasks.join_next().await.is_some() {}
            Ok(())
        })
    }
}

async fn reconcile(session: Arc<RpcSession>, dirty: Arc<Notify>, cancellation: CancellationToken) {
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            () = dirty.notified() => {},
            () = tokio::time::sleep(RECONCILE_INTERVAL) => {},
        }
        let context = session.context(Stage::Maintenance, CALL_TIMEOUT);
        let instance_id = context.instance_id.clone();
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            result = session.call("plugin.reconcile", context, serde_json::json!({}), Vec::new()) => result,
        };
        if !result
            .is_ok_and(|reply| reply.result == serde_json::json!({}) && reply.payload.is_empty())
        {
            tracing::warn!(%instance_id, "plugin maintenance failed; retrying after bounded delay");
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                () = tokio::time::sleep(RETRY_DELAY) => {},
            }
            dirty.notify_one();
        }
    }
}
