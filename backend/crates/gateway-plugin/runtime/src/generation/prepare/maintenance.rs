//! 对账只从已发布集合开始；通知合并，实例串行，失败和停用不阻塞其他实例

use super::{
    PluginRuntime,
    set::{PreparedInstance, PreparedSet},
};
use crate::RpcSession;
use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use gateway_core::{
    lifecycle::CancellationToken,
    runtime::RuntimeSnapshotHandle,
    task::{
        DaemonRestartPolicy, DaemonTask, WorkerContribution, WorkerDaemonContext,
        WorkerDefinitionError, WorkerId, WorkerKind, WorkerRegistration, WorkerRunnable,
        WorkerTaskError,
    },
};
use gateway_plugin_sdk::Stage;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{sync::Notify, task::JoinHandle};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
const RETRY_DELAY: Duration = Duration::from_secs(5);

struct InstanceTask {
    instance: Arc<PreparedInstance>,
    dirty: Arc<Notify>,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

impl InstanceTask {
    fn start(instance: Arc<PreparedInstance>) -> Self {
        let dirty = Arc::new(Notify::new());
        dirty.notify_one();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(reconcile(
            Arc::clone(&instance.session),
            Arc::clone(&dirty),
            cancellation.clone(),
        ));
        Self {
            instance,
            dirty,
            cancellation,
            task,
        }
    }

    async fn stop(mut self) {
        self.cancellation.cancel();
        let _ = (&mut self.task).await;
        // 宿主资源在取消时同步撤销，替换任务还须等对端确认或进程退出
        self.instance.session.settle_cancellations().await;
    }
}

impl Drop for InstanceTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
    }
}

struct MaintenanceTask {
    runtime: Arc<PluginRuntime>,
    snapshots: RuntimeSnapshotHandle,
}

impl PluginRuntime {
    /// 由 Host 监督维护任务；CLI 和只读准备不会启动后台业务
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
    fn run(&self, context: WorkerDaemonContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let cancellation = context.cancellation();
            let mut publications = self.snapshots.subscribe_publications();
            let mut active: Option<Arc<PreparedSet>> = None;
            let mut revision = None;
            let mut tasks = BTreeMap::<String, InstanceTask>::new();
            loop {
                let snapshot = self.snapshots.acquire().ok();
                let reference = snapshot.as_ref().and_then(|s| s.extensions());
                let same_set = active.as_ref().map(|s| &s.id) == reference.map(|s| s.id());
                if !same_set {
                    active = match reference {
                        Some(reference) => {
                            Some(self.runtime.prepared_set(reference).await.map_err(|_| {
                                WorkerTaskError::safe("published plugin set is unavailable")
                            })?)
                        }
                        None => None,
                    };
                    let desired = active
                        .iter()
                        .flat_map(|set| &set.sessions)
                        .filter(|instance| instance.maintenance)
                        .map(|instance| (instance.instance_id.clone(), Arc::clone(instance)))
                        .collect::<BTreeMap<_, _>>();
                    let retired = tasks
                        .extract_if(.., |id, task| {
                            // 发布层只复用完全相同且就绪的实例，Arc 身份也包含会话 incarnation
                            !desired
                                .get(id)
                                .is_some_and(|instance| Arc::ptr_eq(instance, &task.instance))
                        })
                        .map(|(_, task)| task)
                        .collect::<Vec<_>>();
                    for task in &retired {
                        task.cancellation.cancel();
                    }
                    futures::future::join_all(retired.into_iter().map(InstanceTask::stop)).await;
                    for task in tasks.values() {
                        task.dirty.notify_one();
                    }
                    for (id, instance) in desired {
                        tasks
                            .entry(id)
                            .or_insert_with(|| InstanceTask::start(instance));
                    }
                }
                if same_set && revision != snapshot.as_ref().map(|s| s.revision()) {
                    for task in tasks.values() {
                        task.dirty.notify_one();
                    }
                }
                revision = snapshot.as_ref().map(|s| s.revision());
                if snapshot.is_some() {
                    context.succeeded();
                }
                // 发布变更优先于下一轮维护；不在快照发布锁中等待插件或数据库
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => break,
                    changed = publications.next() => if changed.is_none() { break; },
                    result = next_finished(&mut tasks), if !tasks.is_empty() => {
                        if let Some(instance_id) = result {
                            tasks.remove(&instance_id);
                            stop_all(tasks).await;
                            return Err(WorkerTaskError::safe("plugin maintenance task stopped unexpectedly"));
                        }
                    }
                }
            }
            stop_all(tasks).await;
            Ok(())
        })
    }
}

async fn next_finished(tasks: &mut BTreeMap<String, InstanceTask>) -> Option<String> {
    let mut pending = tasks
        .iter_mut()
        .map(|(id, task)| async move {
            let _ = (&mut task.task).await;
            id.clone()
        })
        .collect::<FuturesUnordered<_>>();
    pending.next().await
}

async fn stop_all(tasks: BTreeMap<String, InstanceTask>) {
    for task in tasks.values() {
        task.cancellation.cancel();
    }
    futures::future::join_all(tasks.into_values().map(InstanceTask::stop)).await;
}

async fn reconcile(session: Arc<RpcSession>, dirty: Arc<Notify>, cancellation: CancellationToken) {
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            () = dirty.notified() => {},
            () = tokio::time::sleep(RECONCILE_INTERVAL) => {},
        }
        let context = session.context(Stage::Maintenance, session.budget(Stage::Maintenance));
        let instance_id = context.instance_id.clone();
        let resource_scope_id = context.resource_scope_id.clone();
        let budget_ms = context.timeout_ms;
        let started = tokio::time::Instant::now();
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                tracing::info!(target: "plugin.maintenance", %instance_id, %resource_scope_id, budget_ms,
                    budget_source = ?session.budget_source(Stage::Maintenance),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    reason = "cancelled", "plugin maintenance ended");
                return;
            },
            result = session.call("plugin.reconcile", context, serde_json::json!({}), Vec::new()) => result,
        };
        if let Err(error) = result {
            tracing::warn!(target: "plugin.maintenance", %instance_id, %resource_scope_id, budget_ms,
                budget_source = ?session.budget_source(Stage::Maintenance),
                elapsed_ms = started.elapsed().as_millis() as u64,
                reason = error.diagnostic().0, "plugin maintenance failed; retrying after bounded delay");
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                () = tokio::time::sleep(RETRY_DELAY) => {},
            }
            dirty.notify_one();
        } else {
            tracing::info!(target: "plugin.maintenance", %instance_id, %resource_scope_id, budget_ms,
                budget_source = ?session.budget_source(Stage::Maintenance),
                elapsed_ms = started.elapsed().as_millis() as u64,
                reason = "completed", "plugin maintenance ended");
        }
    }
}
