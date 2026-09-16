//! 导入队列由 Host 管理生命周期，HTTP 只负责接受输入和读取快照。

use std::{
    collections::VecDeque,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use chrono::Utc;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use gateway_core::{
    lifecycle::CancellationToken,
    task::{DaemonTask, WorkerTaskError},
};
use tokio::{sync::Notify, time::Instant};
use uuid::Uuid;

use super::{openai::OpenAiService, xai::XaiService};
use crate::model::{
    AdminError, AdminErrorKind, MutationActor, MutationContext,
    import_tasks::{
        ImportItemStatus, ImportTaskCounts, ImportTaskDetail, ImportTaskInput, ImportTaskItem,
        ImportTaskSummary, MAX_IMPORT_TASK_ITEMS, SubmitImportTask,
    },
};

const CONCURRENCY: usize = 3;
const MAX_ACTIVE_TASKS: usize = 8;
const MAX_RETAINED_TASKS: usize = 100;
const RETENTION: Duration = Duration::from_secs(3600);

pub trait ImportTasksService: Send + Sync {
    fn submit(&self, command: SubmitImportTask) -> Result<ImportTaskSummary, AdminError>;
    fn list(&self, context: &MutationContext) -> Vec<ImportTaskSummary>;
    fn detail(
        &self,
        context: &MutationContext,
        task_id: Uuid,
    ) -> Result<ImportTaskDetail, AdminError>;
    fn stop(
        &self,
        context: &MutationContext,
        task_id: Uuid,
    ) -> Result<ImportTaskDetail, AdminError>;
}

struct Entry {
    task_id: Uuid,
    submission_id: Uuid,
    fingerprint: [u8; 32],
    owner: MutationActor,
    created_at: chrono::DateTime<Utc>,
    finished_at: Option<chrono::DateTime<Utc>>,
    finished_instant: Option<Instant>,
    stop_requested: bool,
    items: Vec<ImportTaskItem>,
    inputs: Vec<Option<ImportTaskInput>>,
}

impl Entry {
    fn summary(&self) -> ImportTaskSummary {
        let mut counts = ImportTaskCounts::default();
        for item in &self.items {
            match item.status {
                ImportItemStatus::Pending => counts.pending += 1,
                ImportItemStatus::Running => counts.running += 1,
                ImportItemStatus::Succeeded => counts.succeeded += 1,
                ImportItemStatus::Failed => counts.failed += 1,
                ImportItemStatus::Unknown => counts.unknown += 1,
                ImportItemStatus::Skipped => counts.skipped += 1,
            }
            counts.imported_accounts += item.account_ids.len();
        }
        ImportTaskSummary {
            task_id: self.task_id,
            created_at: self.created_at,
            finished_at: self.finished_at,
            stop_requested: self.stop_requested,
            total: self.items.len(),
            counts,
        }
    }

    fn detail(&self) -> ImportTaskDetail {
        ImportTaskDetail {
            summary: self.summary(),
            items: self.items.clone(),
        }
    }

    fn finish_if_done(&mut self) {
        if self.finished_at.is_none()
            && self.items.iter().all(|item| {
                !matches!(
                    item.status,
                    ImportItemStatus::Pending | ImportItemStatus::Running
                )
            })
        {
            self.finished_at = Some(Utc::now());
            self.finished_instant = Some(Instant::now());
            // 终态仅保留结果，连已清空的输入槽位分配也一并释放。
            self.inputs = Vec::new();
        }
    }

    fn stop(&mut self) {
        if self.finished_at.is_some() {
            return;
        }
        self.stop_requested = true;
        for (item, input) in self.items.iter_mut().zip(&mut self.inputs) {
            if item.status == ImportItemStatus::Pending {
                item.status = ImportItemStatus::Skipped;
                *input = None;
            }
        }
        self.finish_if_done();
    }
}

#[derive(Default)]
struct Registry {
    tasks: VecDeque<Entry>,
    shutting_down: bool,
}

pub(crate) struct DefaultImportTasksService {
    registry: Mutex<Registry>,
    notify: Notify,
    openai: Arc<dyn OpenAiService>,
    xai: Arc<dyn XaiService>,
}

impl DefaultImportTasksService {
    pub(crate) fn new(openai: Arc<dyn OpenAiService>, xai: Arc<dyn XaiService>) -> Arc<Self> {
        Arc::new(Self {
            registry: Mutex::default(),
            notify: Notify::new(),
            openai,
            xai,
        })
    }

    fn registry(&self) -> MutexGuard<'_, Registry> {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.tasks.retain(|entry| {
            entry
                .finished_instant
                .is_none_or(|at| at.elapsed() < RETENTION)
        });
        registry
    }

    fn take_next(&self) -> Option<(Uuid, usize, ImportTaskInput)> {
        let mut registry = self.registry();
        if registry.shutting_down {
            return None;
        }
        let task_index = registry
            .tasks
            .iter()
            .position(|entry| entry.inputs.iter().any(Option::is_some))?;
        // 轮转任务，避免大批次独占后续全部执行机会。
        let mut entry = registry.tasks.remove(task_index)?;
        let index = entry.inputs.iter().position(Option::is_some)?;
        let input = entry.inputs[index].take()?;
        entry.items[index].status = ImportItemStatus::Running;
        let id = entry.task_id;
        registry.tasks.push_back(entry);
        Some((id, index, input))
    }

    async fn execute(&self, id: Uuid, index: usize, input: ImportTaskInput) {
        let operation = async {
            match input.provider.as_str() {
                "openai" => self.openai.import_document(input.command).await,
                "xai" => self.xai.import_document(input.command).await,
                _ => Err(AdminError::invalid("不支持的导入平台")),
            }
        };
        let result = AssertUnwindSafe(operation).catch_unwind().await;
        let mut registry = self.registry();
        let Some(entry) = registry.tasks.iter_mut().find(|entry| entry.task_id == id) else {
            return;
        };
        let item = &mut entry.items[index];
        match result {
            Ok(Ok(result)) => {
                item.status = ImportItemStatus::Succeeded;
                item.account_ids = result.credential_ids;
            }
            Ok(Err(error)) => {
                // 存储或发布异常也可能发生在提交之后；不自动重放可能轮换 RT 的操作。
                item.status = if matches!(
                    error.kind(),
                    AdminErrorKind::UpstreamResultUnknown
                        | AdminErrorKind::Internal
                        | AdminErrorKind::Unavailable
                ) {
                    ImportItemStatus::Unknown
                } else {
                    ImportItemStatus::Failed
                };
                item.message = Some(error.message().to_owned());
            }
            Err(_) => {
                item.status = ImportItemStatus::Unknown;
                item.message = Some("执行中断，请先核对账号列表再决定是否重新导入".to_owned());
            }
        }
        entry.finish_if_done();
    }
}

impl ImportTasksService for DefaultImportTasksService {
    fn submit(&self, command: SubmitImportTask) -> Result<ImportTaskSummary, AdminError> {
        if command.items.is_empty()
            || command.items.len() > MAX_IMPORT_TASK_ITEMS
            || command.items.iter().any(|item| {
                !matches!(item.provider.as_str(), "openai" | "xai")
                    || item.command.context.actor != command.context.actor
            })
        {
            return Err(AdminError::invalid("导入任务需包含 1–200 条有效输入"));
        }
        let mut registry = self.registry();
        if let Some(existing) = registry.tasks.iter().find(|entry| {
            entry.owner == command.context.actor && entry.submission_id == command.submission_id
        }) {
            return if existing.fingerprint == command.fingerprint {
                Ok(existing.summary())
            } else {
                Err(AdminError::new(
                    AdminErrorKind::Conflict,
                    "提交标识已用于其他导入内容",
                ))
            };
        }
        if registry.shutting_down {
            return Err(AdminError::new(
                AdminErrorKind::Unavailable,
                "服务正在关闭，暂不接受导入任务",
            ));
        }
        if registry
            .tasks
            .iter()
            .filter(|entry| entry.finished_at.is_none())
            .count()
            >= MAX_ACTIVE_TASKS
            || registry.tasks.len() >= MAX_RETAINED_TASKS
        {
            return Err(AdminError::new(
                AdminErrorKind::RateLimited,
                "导入任务已满，请稍后再试",
            ));
        }
        let items = command
            .items
            .iter()
            .enumerate()
            .map(|(index, input)| ImportTaskItem {
                index: index + 1,
                provider: input.provider.clone(),
                status: ImportItemStatus::Pending,
                account_ids: Vec::new(),
                message: None,
            })
            .collect();
        let entry = Entry {
            task_id: Uuid::now_v7(),
            submission_id: command.submission_id,
            fingerprint: command.fingerprint,
            owner: command.context.actor,
            created_at: Utc::now(),
            finished_at: None,
            finished_instant: None,
            stop_requested: false,
            items,
            inputs: command.items.into_iter().map(Some).collect(),
        };
        let summary = entry.summary();
        registry.tasks.push_back(entry);
        drop(registry);
        self.notify.notify_one();
        Ok(summary)
    }

    fn list(&self, context: &MutationContext) -> Vec<ImportTaskSummary> {
        let mut tasks: Vec<_> = self
            .registry()
            .tasks
            .iter()
            .filter(|entry| entry.owner == context.actor)
            .map(Entry::summary)
            .collect();
        tasks.sort_by_key(|entry| std::cmp::Reverse(entry.task_id));
        tasks
    }

    fn detail(
        &self,
        context: &MutationContext,
        task_id: Uuid,
    ) -> Result<ImportTaskDetail, AdminError> {
        self.registry()
            .tasks
            .iter()
            .find(|entry| entry.task_id == task_id && entry.owner == context.actor)
            .map(Entry::detail)
            .ok_or_else(|| AdminError::not_found("导入任务不存在或已过期"))
    }

    fn stop(
        &self,
        context: &MutationContext,
        task_id: Uuid,
    ) -> Result<ImportTaskDetail, AdminError> {
        let mut registry = self.registry();
        let entry = registry
            .tasks
            .iter_mut()
            .find(|entry| entry.task_id == task_id && entry.owner == context.actor)
            .ok_or_else(|| AdminError::not_found("导入任务不存在或已过期"))?;
        entry.stop();
        Ok(entry.detail())
    }
}

pub(crate) struct ImportTaskWorker(pub Arc<DefaultImportTasksService>);

impl DaemonTask for ImportTaskWorker {
    fn run(&self, cancellation: CancellationToken) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let mut active = FuturesUnordered::new();
            // 固定一个取消 future，避免循环反复注册中立 CancellationToken 的 waiter。
            let cancelled = cancellation.cancelled();
            tokio::pin!(cancelled);
            let mut cleanup = tokio::time::interval(Duration::from_secs(60));
            loop {
                if cancellation.is_cancelled() {
                    let mut registry = self.0.registry();
                    registry.shutting_down = true;
                    for entry in &mut registry.tasks {
                        entry.stop();
                    }
                    break;
                }
                while active.len() < CONCURRENCY {
                    let Some((id, index, input)) = self.0.take_next() else {
                        break;
                    };
                    active.push(self.0.execute(id, index, input));
                }
                tokio::select! {
                    () = &mut cancelled => {},
                    _ = active.next(), if !active.is_empty() => {},
                    () = self.0.notify.notified() => {},
                    _ = cleanup.tick() => { drop(self.0.registry()); },
                }
            }
            // Host 提供总关闭预算；此处让已发出的交换与入库尽量完成。
            while active.next().await.is_some() {}
            Ok(())
        })
    }
}
