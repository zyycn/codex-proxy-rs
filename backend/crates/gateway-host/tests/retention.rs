//! 验证历史清理的轮转、批次预算、时长限制与取消行为

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::{
    model::retention::{RetentionPolicy, RetentionTarget},
    ports::{
        retention::RetentionStore,
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use gateway_core::{
    lifecycle::CancellationToken,
    task::{ScheduledTask, WorkerCycleContext, WorkerId, WorkerKind},
};
use gateway_host::retention::{RetentionCycleBudget, RetentionTask};
use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Store {
    calls: Mutex<Vec<RetentionTarget>>,
    fail: bool,
    full: bool,
    block_load: bool,
}

#[async_trait]
impl RetentionStore for Store {
    async fn load_policy(&self) -> AdminStoreResult<RetentionPolicy> {
        if self.block_load {
            std::future::pending::<()>().await;
        }
        Ok(RetentionPolicy::try_new(31, 7, 90).unwrap())
    }
    async fn purge_batch(
        &self,
        target: RetentionTarget,
        _: DateTime<Utc>,
        _: RetentionPolicy,
        limit: NonZeroU32,
    ) -> AdminStoreResult<u64> {
        self.calls.lock().unwrap().push(target);
        if self.fail {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                "retention",
                "fixture",
            ));
        }
        Ok(if self.full { u64::from(limit.get()) } else { 0 })
    }
}

fn context(cancellation: CancellationToken) -> WorkerCycleContext {
    WorkerCycleContext::new(
        WorkerId::try_new(WorkerKind::Retention, "postgres").unwrap(),
        None,
        cancellation,
    )
}

fn budget() -> RetentionCycleBudget {
    RetentionCycleBudget {
        batch_rows: NonZeroU32::new(2).unwrap(),
        max_batches: NonZeroU32::new(4).unwrap(),
        max_duration: Duration::from_secs(1),
        batch_pause: Duration::ZERO,
    }
}

#[tokio::test]
async fn full_tables_rotate_until_batch_budget_is_exhausted() {
    let store = Arc::new(Store {
        full: true,
        ..Store::default()
    });
    RetentionTask::new(store.clone(), budget())
        .run_cycle(context(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(
        *store.calls.lock().unwrap(),
        [
            RetentionTarget::ModelRequests,
            RetentionTarget::OpsEvents,
            RetentionTarget::AdminAuditEvents,
            RetentionTarget::ModelRequests
        ]
    );
}

#[tokio::test]
async fn empty_tables_are_not_revisited_and_errors_stop_the_cycle() {
    let store = Arc::new(Store::default());
    RetentionTask::new(store.clone(), budget())
        .run_cycle(context(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(store.calls.lock().unwrap().len(), 3);
    let store = Arc::new(Store {
        fail: true,
        ..Store::default()
    });
    assert!(
        RetentionTask::new(store.clone(), budget())
            .run_cycle(context(CancellationToken::new()))
            .await
            .is_err()
    );
    assert_eq!(store.calls.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn duration_budget_stops_cleanup_during_batch_pause() {
    let store = Arc::new(Store {
        full: true,
        ..Store::default()
    });
    let mut budget = budget();
    budget.batch_pause = Duration::from_secs(2);
    RetentionTask::new(store.clone(), budget)
        .run_cycle(context(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(store.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_interrupts_settings_read_before_any_delete() {
    let store = Arc::new(Store {
        block_load: true,
        ..Store::default()
    });
    let token = CancellationToken::new();
    let task = RetentionTask::new(store.clone(), budget());
    let run = task.run_cycle(context(token.clone()));
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => panic!("unexpected completion: {result:?}"),
        () = tokio::task::yield_now() => token.cancel(),
    }
    run.await.unwrap();
    assert!(store.calls.lock().unwrap().is_empty());
}
