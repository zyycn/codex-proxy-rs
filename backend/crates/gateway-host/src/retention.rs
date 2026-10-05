//! 保留期任务的调度、单轮预算、取消与日志；删除事务由 Store 端口负责

use std::{num::NonZeroU32, sync::Arc, time::Duration};

use chrono::Utc;
use gateway_admin::{model::retention::RetentionTarget, ports::retention::RetentionStore};
use gateway_core::task::{
    ScheduledTask, WorkerContribution, WorkerCycleContext, WorkerDefinitionError, WorkerId,
    WorkerKind, WorkerLeaseRequest, WorkerRegistration, WorkerRunnable, WorkerSchedule,
    WorkerTaskError,
};

#[derive(Debug, Clone, Copy)]
pub struct RetentionCycleBudget {
    pub batch_rows: NonZeroU32,
    pub max_batches: NonZeroU32,
    pub max_duration: Duration,
    pub batch_pause: Duration,
}

impl Default for RetentionCycleBudget {
    fn default() -> Self {
        Self {
            batch_rows: NonZeroU32::new(5_000).unwrap_or(NonZeroU32::MIN),
            max_batches: NonZeroU32::new(12).unwrap_or(NonZeroU32::MIN),
            max_duration: Duration::from_secs(30),
            batch_pause: Duration::from_millis(50),
        }
    }
}

pub fn worker(store: Arc<dyn RetentionStore>) -> Result<WorkerContribution, WorkerDefinitionError> {
    // 保持稳定的租约身份，滚动升级期间仍与已有进程互斥
    let id = WorkerId::try_new(WorkerKind::Retention, "postgres")?;
    let schedule = WorkerSchedule::try_new(
        Duration::from_secs(60 * 60),
        Duration::from_secs(1),
        Duration::from_secs(60),
        Duration::from_secs(15 * 60),
        Duration::from_secs(5 * 60),
    )?;
    let lease = WorkerLeaseRequest::try_new(id.clone(), schedule.leader_lease_ttl())?;
    Ok(WorkerContribution::Registration(
        WorkerRegistration::try_new(
            id,
            WorkerRunnable::Scheduled {
                schedule,
                lease: Some(lease),
                task: Box::new(RetentionTask::new(store, RetentionCycleBudget::default())),
            },
        )?,
    ))
}

pub struct RetentionTask {
    store: Arc<dyn RetentionStore>,
    budget: RetentionCycleBudget,
}

impl RetentionTask {
    #[must_use]
    pub fn new(store: Arc<dyn RetentionStore>, budget: RetentionCycleBudget) -> Self {
        Self { store, budget }
    }

    async fn cleanup(&self, context: &WorkerCycleContext) -> Result<(), WorkerTaskError> {
        let policy = self
            .store
            .load_policy()
            .await
            .map_err(|_| WorkerTaskError::safe("retention settings read failed"))?;
        let now = Utc::now();
        let started_at = tokio::time::Instant::now();
        let mut targets = [
            (RetentionTarget::ModelRequests, 0_u64, false),
            (RetentionTarget::OpsEvents, 0, false),
            (RetentionTarget::AdminAuditEvents, 0, false),
        ];
        let mut batches = 0;
        'cycles: loop {
            for (target, deleted, complete) in &mut targets {
                if *complete {
                    continue;
                }
                if batches >= self.budget.max_batches.get()
                    || started_at.elapsed() >= self.budget.max_duration
                    || context.cancellation().is_cancelled()
                {
                    break 'cycles;
                }
                let count = self
                    .store
                    .purge_batch(*target, now, policy, self.budget.batch_rows)
                    .await
                    .map_err(|_| WorkerTaskError::safe("retention cleanup failed"))?;
                batches += 1;
                *deleted = deleted.saturating_add(count);
                *complete = count < u64::from(self.budget.batch_rows.get());
                if !*complete
                    && batches < self.budget.max_batches.get()
                    && !self.budget.batch_pause.is_zero()
                {
                    let remaining = self
                        .budget
                        .max_duration
                        .saturating_sub(started_at.elapsed());
                    tokio::time::sleep(self.budget.batch_pause.min(remaining)).await;
                }
            }
            if targets.iter().all(|(_, _, complete)| *complete) {
                break;
            }
        }
        tracing::info!(
            model_requests = targets[0].1,
            ops_events = targets[1].1,
            admin_audit_events = targets[2].1,
            batches,
            budget_exhausted = targets.iter().any(|(_, _, complete)| !complete),
            elapsed_milliseconds =
                u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            "retention cycle completed"
        );
        Ok(())
    }
}

impl ScheduledTask for RetentionTask {
    fn run_cycle(
        &self,
        context: WorkerCycleContext,
    ) -> futures::future::BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            tokio::select! {
                biased;
                () = context.cancellation().cancelled() => Ok(()),
                result = self.cleanup(&context) => result,
            }
        })
    }
}
