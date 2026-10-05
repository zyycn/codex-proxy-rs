//! 验证执行写入队列的容量、请求内顺序与排空行为

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use gateway_core::account::ProviderAccountId;
use gateway_core::engine::{
    AttemptRecord, ExecutionStore, IntermediateFailure, ModelRequestFinalization, ModelRequestId,
    NewModelRequest, ProbeFailure, RecoveryReport,
};
use gateway_core::error::{
    OpaqueUpstreamValue, ProviderError, ProviderErrorKind, StoreError, StoreErrorKind,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::routing::{ProviderKind, UpstreamModelId};
use gateway_core::task::DaemonTask as _;
use gateway_core::upstream::UpstreamSendState;
use gateway_store::postgres::{
    BufferedExecutionStore, ObservabilityRepository as _, PgExecutionStore,
};
use tokio::sync::{Notify, Semaphore};

use super::{
    TestDatabase,
    execution::{accepted_request, early_failure},
    observability_repository,
};

#[derive(Default)]
struct RecordingStore {
    operations: Mutex<Vec<&'static str>>,
    fail_once: Mutex<Option<&'static str>>,
}

impl RecordingStore {
    fn failing_once(operation: &'static str) -> Self {
        Self {
            operations: Mutex::new(Vec::new()),
            fail_once: Mutex::new(Some(operation)),
        }
    }

    fn record(&self, operation: &'static str) -> Result<(), StoreError> {
        self.operations
            .lock()
            .expect("operations lock")
            .push(operation);
        let mut fail_once = self.fail_once.lock().expect("failure lock");
        if fail_once.as_ref().is_some_and(|value| *value == operation) {
            *fail_once = None;
            return Err(StoreError::new(StoreErrorKind::Unavailable));
        }
        Ok(())
    }
}

#[async_trait]
impl ExecutionStore for RecordingStore {
    async fn create_model_request(&self, _: NewModelRequest) -> Result<(), StoreError> {
        self.record("create")
    }

    async fn record_attempt(&self, _: AttemptRecord) -> Result<(), StoreError> {
        self.record("attempt")
    }

    async fn mark_send_state(
        &self,
        _: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        self.record("send")
    }

    async fn mark_downstream_committed(
        &self,
        _: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        self.record("commit")
    }

    async fn record_client_status(&self, _: &ModelRequestId, _: u16) -> Result<(), StoreError> {
        self.record("status")
    }

    async fn record_intermediate_failure(&self, _: IntermediateFailure) -> Result<(), StoreError> {
        self.record("intermediate_failure")
    }

    async fn record_probe_failure(&self, _: ProbeFailure) -> Result<(), StoreError> {
        self.record("probe_failure")
    }

    async fn finalize_model_request(&self, _: ModelRequestFinalization) -> Result<(), StoreError> {
        self.record("finalize")
    }

    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        self.record("recover")?;
        Ok(RecoveryReport::default())
    }
}

struct BlockingStore {
    active: AtomicUsize,
    maximum_active: AtomicUsize,
    started: AtomicUsize,
    started_changed: Notify,
    releases: Semaphore,
    operations: Mutex<Vec<(String, &'static str)>>,
}

impl Default for BlockingStore {
    fn default() -> Self {
        Self {
            active: AtomicUsize::new(0),
            maximum_active: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            started_changed: Notify::new(),
            releases: Semaphore::new(0),
            operations: Mutex::new(Vec::new()),
        }
    }
}

struct ActiveWrite<'a>(&'a AtomicUsize);

impl Drop for ActiveWrite<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl BlockingStore {
    async fn record(&self, request_id: &str, operation: &'static str) -> Result<(), StoreError> {
        self.operations
            .lock()
            .expect("operations lock")
            .push((request_id.to_owned(), operation));
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.maximum_active.fetch_max(active, Ordering::AcqRel);
        let _active = ActiveWrite(&self.active);
        self.started.fetch_add(1, Ordering::AcqRel);
        self.started_changed.notify_waiters();
        self.releases
            .acquire()
            .await
            .expect("release semaphore")
            .forget();
        Ok(())
    }

    async fn wait_for_started(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let changed = self.started_changed.notified();
                if self.started.load(Ordering::Acquire) >= expected {
                    break;
                }
                changed.await;
            }
        })
        .await
        .expect("expected writes should start");
    }

    fn operations_for(&self, request_id: &str) -> Vec<&'static str> {
        self.operations
            .lock()
            .expect("operations lock")
            .iter()
            .filter_map(|(candidate, operation)| (candidate == request_id).then_some(*operation))
            .collect()
    }
}

#[async_trait]
impl ExecutionStore for BlockingStore {
    async fn create_model_request(&self, request: NewModelRequest) -> Result<(), StoreError> {
        self.record(request.id.as_str(), "create").await
    }

    async fn record_attempt(&self, attempt: AttemptRecord) -> Result<(), StoreError> {
        self.record(attempt.request_id.as_str(), "attempt").await
    }

    async fn mark_send_state(
        &self,
        request_id: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        self.record(request_id.as_str(), "send").await
    }

    async fn mark_downstream_committed(
        &self,
        request_id: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        self.record(request_id.as_str(), "commit").await
    }

    async fn record_client_status(
        &self,
        request_id: &ModelRequestId,
        _: u16,
    ) -> Result<(), StoreError> {
        self.record(request_id.as_str(), "status").await
    }

    async fn record_intermediate_failure(
        &self,
        failure: IntermediateFailure,
    ) -> Result<(), StoreError> {
        self.record(failure.request_id.as_str(), "intermediate_failure")
            .await
    }

    async fn record_probe_failure(&self, _: ProbeFailure) -> Result<(), StoreError> {
        self.record("probe", "probe_failure").await
    }

    async fn finalize_model_request(
        &self,
        finalization: ModelRequestFinalization,
    ) -> Result<(), StoreError> {
        self.record(finalization.request_id.as_str(), "finalize")
            .await
    }

    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        Ok(RecoveryReport::default())
    }
}

#[tokio::test]
async fn full_observation_queue_never_waits_for_the_database() {
    let inner = Arc::new(RecordingStore::default());
    let (store, _writer) = BufferedExecutionStore::with_capacity(
        Arc::clone(&inner),
        NonZeroUsize::new(1).expect("capacity"),
    );
    let request_id = ModelRequestId::new("req_queue_full").expect("request id");

    tokio::time::timeout(
        Duration::from_millis(50),
        store.mark_send_state(&request_id, UpstreamSendState::Sent),
    )
    .await
    .expect("first enqueue must not wait")
    .expect("first enqueue is fail-open");
    tokio::time::timeout(
        Duration::from_millis(50),
        store.record_client_status(&request_id, 200),
    )
    .await
    .expect("full queue must not wait")
    .expect("full queue is fail-open");

    assert!(inner.operations.lock().expect("operations lock").is_empty());
    assert_eq!(store.stats().queued_items, 1);
    assert_eq!(store.stats().dropped_total, 1);
}

#[tokio::test]
async fn observation_writer_persists_commands_in_enqueue_order() {
    let inner = Arc::new(RecordingStore::default());
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::clone(&inner),
        NonZeroUsize::new(8).expect("capacity"),
    );
    let request_id = ModelRequestId::new("req_queue_order").expect("request id");
    store
        .mark_send_state(&request_id, UpstreamSendState::Sent)
        .await
        .expect("enqueue send state");
    store
        .record_client_status(&request_id, 200)
        .await
        .expect("enqueue client status");

    let cancellation = CancellationToken::new();
    let writer = Arc::new(writer);
    let task = tokio::spawn({
        let writer = Arc::clone(&writer);
        let cancellation = cancellation.clone();
        async move { writer.run(cancellation).await }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if inner.operations.lock().expect("operations lock").len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("writer should drain queued observations");
    cancellation.cancel();
    task.await
        .expect("writer task")
        .expect("writer cancellation");

    assert_eq!(
        *inner.operations.lock().expect("operations lock"),
        ["send", "status"]
    );
    assert_eq!(store.stats().persisted_total, 2);
    assert_eq!(store.stats().queued_items, 0);
}

#[tokio::test]
async fn request_affine_lanes_run_independently_and_preserve_each_request_order() {
    let inner = Arc::new(BlockingStore::default());
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::clone(&inner),
        NonZeroUsize::new(4).expect("capacity"),
    );
    let first = ModelRequestId::new("req_lane_0").expect("first request id");
    let second = ModelRequestId::new("req_lane_1").expect("second request id");
    for request_id in [&first, &second] {
        store
            .mark_send_state(request_id, UpstreamSendState::Sent)
            .await
            .expect("enqueue send state");
        store
            .record_client_status(request_id, 200)
            .await
            .expect("enqueue status");
    }

    let cancellation = CancellationToken::new();
    let writer = Arc::new(writer);
    let task = tokio::spawn({
        let writer = Arc::clone(&writer);
        let cancellation = cancellation.clone();
        async move { writer.run(cancellation).await }
    });
    inner.wait_for_started(2).await;
    assert_eq!(inner.maximum_active.load(Ordering::Acquire), 2);
    assert_eq!(inner.operations_for(first.as_str()), ["send"]);
    assert_eq!(inner.operations_for(second.as_str()), ["send"]);

    inner.releases.add_permits(2);
    inner.wait_for_started(4).await;
    inner.releases.add_permits(2);
    tokio::time::timeout(Duration::from_secs(1), async {
        while store.stats().persisted_total != 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all writes should persist");
    cancellation.cancel();
    task.await
        .expect("writer task")
        .expect("writer cancellation");

    assert_eq!(inner.operations_for(first.as_str()), ["send", "status"]);
    assert_eq!(inner.operations_for(second.as_str()), ["send", "status"]);
    assert_eq!(store.stats().dropped_total, 0);
    assert_eq!(store.stats().queued_items, 0);
}

#[tokio::test]
async fn idle_wait_includes_a_dequeued_write_until_persistence_finishes() {
    let inner = Arc::new(BlockingStore::default());
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::clone(&inner),
        NonZeroUsize::new(1).expect("capacity"),
    );
    let request_id = ModelRequestId::new("req_idle_inflight").expect("request id");
    store
        .mark_send_state(&request_id, UpstreamSendState::Sent)
        .await
        .expect("enqueue write");
    let cancellation = CancellationToken::new();
    let writer = Arc::new(writer);
    let task = tokio::spawn({
        let writer = Arc::clone(&writer);
        let cancellation = cancellation.clone();
        async move { writer.run(cancellation).await }
    });
    inner.wait_for_started(1).await;

    let idle = writer.wait_until_idle(Instant::now() + Duration::from_secs(1));
    tokio::pin!(idle);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut idle)
            .await
            .is_err(),
        "从队列取出的写入仍应计入 drain"
    );
    inner.releases.add_permits(1);
    assert!(idle.await);

    cancellation.cancel();
    task.await
        .expect("writer task")
        .expect("writer cancellation");
    assert_eq!(store.stats().persisted_total, 1);
    assert_eq!(store.stats().queued_items, 0);
}

#[tokio::test]
async fn global_item_budget_includes_writes_running_in_every_lane() {
    let inner = Arc::new(BlockingStore::default());
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::clone(&inner),
        NonZeroUsize::new(2).expect("capacity"),
    );
    let first = ModelRequestId::new("req_lane_0").expect("first request id");
    let second = ModelRequestId::new("req_lane_1").expect("second request id");
    let rejected = ModelRequestId::new("req_lane_2").expect("rejected request id");
    store
        .mark_send_state(&first, UpstreamSendState::Sent)
        .await
        .expect("enqueue first lane");
    store
        .mark_send_state(&second, UpstreamSendState::Sent)
        .await
        .expect("enqueue second lane");

    let cancellation = CancellationToken::new();
    let writer = Arc::new(writer);
    let task = tokio::spawn({
        let writer = Arc::clone(&writer);
        let cancellation = cancellation.clone();
        async move { writer.run(cancellation).await }
    });
    inner.wait_for_started(2).await;
    assert_eq!(store.stats().queued_items, 2);
    store
        .mark_send_state(&rejected, UpstreamSendState::Sent)
        .await
        .expect("full global budget remains fail open");
    assert_eq!(store.stats().dropped_total, 1);
    assert_eq!(store.stats().queued_items, 2);

    inner.releases.add_permits(2);
    tokio::time::timeout(Duration::from_secs(1), async {
        while store.stats().persisted_total != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("accepted writes should persist");
    cancellation.cancel();
    task.await
        .expect("writer task")
        .expect("writer cancellation");
    assert_eq!(store.stats().queued_items, 0);
}

#[tokio::test]
async fn all_lanes_share_one_shutdown_deadline_for_inflight_writes() {
    let inner = Arc::new(BlockingStore::default());
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::clone(&inner),
        NonZeroUsize::new(4).expect("capacity"),
    );
    for index in 0..4 {
        let request_id = ModelRequestId::new(format!("req_lane_{index}"))
            .expect("request id assigned to a distinct lane");
        store
            .mark_send_state(&request_id, UpstreamSendState::Sent)
            .await
            .expect("enqueue lane write");
    }

    let cancellation = CancellationToken::new();
    let writer = Arc::new(writer);
    let task = tokio::spawn({
        let writer = Arc::clone(&writer);
        let cancellation = cancellation.clone();
        async move { writer.run(cancellation).await }
    });
    inner.wait_for_started(4).await;
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("four blocked lanes must share the two-second shutdown deadline")
        .expect("writer task")
        .expect("writer cancellation");

    assert_eq!(store.stats().persisted_total, 0);
    assert_eq!(store.stats().dropped_total, 4);
    assert_eq!(store.stats().queued_items, 0);
}

#[tokio::test]
async fn observation_writer_continues_after_a_database_write_failure() {
    let inner = Arc::new(RecordingStore::failing_once("send"));
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::clone(&inner),
        NonZeroUsize::new(8).expect("capacity"),
    );
    let request_id = ModelRequestId::new("req_queue_failure").expect("request id");
    store
        .mark_send_state(&request_id, UpstreamSendState::Sent)
        .await
        .expect("enqueue failed write");
    store
        .record_client_status(&request_id, 200)
        .await
        .expect("enqueue following write");

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    writer
        .run(cancellation)
        .await
        .expect("shutdown drains observations");

    assert_eq!(
        *inner.operations.lock().expect("operations lock"),
        ["send", "status"]
    );
    assert_eq!(store.stats().write_failure_total, 1);
    assert_eq!(store.stats().persisted_total, 1);
    assert_eq!(store.stats().queued_items, 0);
}

#[tokio::test]
async fn shutdown_drains_observations_already_accepted_by_the_queue() {
    let inner = Arc::new(RecordingStore::default());
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::clone(&inner),
        NonZeroUsize::new(8).expect("capacity"),
    );
    let request_id = ModelRequestId::new("req_queue_shutdown").expect("request id");
    store
        .mark_send_state(&request_id, UpstreamSendState::Sent)
        .await
        .expect("enqueue send state");
    store
        .record_client_status(&request_id, 200)
        .await
        .expect("enqueue client status");

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    writer
        .run(cancellation)
        .await
        .expect("shutdown drains observations");

    assert_eq!(
        *inner.operations.lock().expect("operations lock"),
        ["send", "status"]
    );
    assert_eq!(store.stats().persisted_total, 2);
    assert_eq!(store.stats().dropped_total, 0);
}

#[tokio::test]
async fn observation_byte_budget_drops_payload_without_waiting_for_the_database() {
    let inner = Arc::new(RecordingStore::default());
    let (store, _writer) = BufferedExecutionStore::with_limits(
        Arc::clone(&inner),
        NonZeroUsize::new(8).expect("capacity"),
        NonZeroUsize::new(1_024).expect("byte capacity"),
    );
    let provider_error =
        ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::NotSent)
            .with_upstream_code(OpaqueUpstreamValue::new("x".repeat(2_048)));
    let failure = ProbeFailure {
        provider_kind: ProviderKind::new("openai").expect("provider"),
        account_id: ProviderAccountId::new("acct_byte_budget").expect("account"),
        upstream_model_id: UpstreamModelId::new("gpt-byte-budget").expect("model"),
        error: provider_error,
        latency: Duration::from_millis(1),
    };

    tokio::time::timeout(
        Duration::from_millis(50),
        store.record_probe_failure(failure),
    )
    .await
    .expect("byte budget must not wait")
    .expect("byte budget is fail-open");

    assert!(inner.operations.lock().expect("operations lock").is_empty());
    assert_eq!(store.stats().queued_items, 0);
    assert_eq!(store.stats().dropped_total, 1);
}

#[tokio::test]
async fn zero_attempt_create_and_finalize_drain_in_order_with_trace() {
    let Some(database) = TestDatabase::create("zero_attempt_queue_order").await else {
        return;
    };
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::new(PgExecutionStore::new(database.pool.clone())),
        NonZeroUsize::new(2).expect("capacity"),
    );
    let request = accepted_request("req_zero_attempt_queue_order");
    let finalization = early_failure(&request);
    let expected_trace: serde_json::Value = serde_json::from_str(
        finalization
            .diagnostic_trace_json
            .as_deref()
            .expect("trace"),
    )
    .expect("trace JSON");
    store
        .create_model_request(request.clone())
        .await
        .expect("enqueue create");
    store
        .finalize_model_request(finalization)
        .await
        .expect("enqueue finalization");
    let before: i64 = sqlx::query_scalar("select count(*) from model_requests")
        .fetch_one(&database.pool)
        .await
        .expect("not yet persisted");
    assert_eq!(before, 0);
    assert_eq!(store.stats().queued_items, 2);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    writer
        .run(cancellation)
        .await
        .expect("shutdown drains accepted commands");
    let stats = store.stats();
    assert_eq!(stats.persisted_total, 2);
    assert_eq!(stats.write_failure_total, 0);
    assert_eq!(stats.dropped_total, 0);
    assert_eq!(stats.queued_items, 0);
    assert_eq!(stats.queued_bytes, 0);
    let detail = observability_repository(&database.pool)
        .usage_record_detail(request.id.as_str())
        .await
        .expect("queued failure detail");
    assert_eq!(detail.request.outcome, "failed");
    assert_eq!(detail.request.attempt_count, 0);
    assert_eq!(detail.request.upstream_send_state, "not_sent");
    assert_eq!(detail.trace, Some(expected_trace));
    assert!(detail.attempts.is_empty());
    database.close().await;
}

#[tokio::test]
async fn request_affine_lanes_persist_a_postgres_lifecycle_burst_without_orphans() {
    const REQUESTS: usize = 256;
    const WRITES: u64 = (REQUESTS * 2) as u64;

    let Some(database) = TestDatabase::create("execution_lane_burst").await else {
        return;
    };
    let (store, writer) =
        BufferedExecutionStore::new(Arc::new(PgExecutionStore::new(database.pool.clone())));
    for index in 0..REQUESTS {
        let request = accepted_request(&format!("req_execution_lane_burst_{index}"));
        store
            .create_model_request(request.clone())
            .await
            .expect("enqueue create");
        store
            .finalize_model_request(early_failure(&request))
            .await
            .expect("enqueue finalization");
    }
    assert_eq!(store.stats().enqueued_total, WRITES);
    assert_eq!(store.stats().dropped_total, 0);

    let cancellation = CancellationToken::new();
    let writer = Arc::new(writer);
    let task = tokio::spawn({
        let writer = Arc::clone(&writer);
        let cancellation = cancellation.clone();
        async move { writer.run(cancellation).await }
    });
    tokio::time::timeout(Duration::from_secs(15), async {
        while store.stats().persisted_total != WRITES {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("PostgreSQL burst should drain");
    cancellation.cancel();
    task.await
        .expect("writer task")
        .expect("writer cancellation");

    let (total, failed): (i64, i64) = sqlx::query_as(
        "select count(*), count(*) filter (where outcome = 'failed')
         from model_requests where id like 'req_execution_lane_burst_%'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load burst rows");
    assert_eq!((total, failed), (REQUESTS as i64, REQUESTS as i64));
    let stats = store.stats();
    assert_eq!(stats.persisted_total, WRITES);
    assert_eq!(stats.write_failure_total, 0);
    assert_eq!(stats.dropped_total, 0);
    assert_eq!(stats.queued_items, 0);
    assert_eq!(stats.queued_bytes, 0);
    database.close().await;
}

#[tokio::test]
async fn zero_attempt_dropped_finalize_is_fail_open_and_recoverable_at_deadline() {
    let Some(database) = TestDatabase::create("zero_attempt_queue_full").await else {
        return;
    };
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::new(PgExecutionStore::new(database.pool.clone())),
        NonZeroUsize::new(1).expect("capacity"),
    );
    let request = accepted_request("req_zero_attempt_queue_full");
    tokio::time::timeout(Duration::from_secs(1), async {
        store
            .create_model_request(request.clone())
            .await
            .expect("enqueue create");
        store
            .finalize_model_request(early_failure(&request))
            .await
            .expect("full queue is fail open");
    })
    .await
    .expect("no database wait");
    assert_eq!(store.stats().enqueued_total, 1);
    assert_eq!(store.stats().dropped_total, 1);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    writer.run(cancellation).await.expect("drain create only");
    let repository = observability_repository(&database.pool);
    let pending = repository
        .usage_record_detail(request.id.as_str())
        .await
        .expect("pending record");
    assert_eq!(pending.request.outcome, "running");
    assert_eq!(pending.trace, None);
    assert!(pending.attempts.is_empty());
    assert_eq!(
        store
            .recover_expired(request.deadline_at.at().unwrap())
            .await
            .expect("recover without queue")
            .requests,
        1
    );
    let recovered = repository
        .usage_record_detail(request.id.as_str())
        .await
        .expect("recovered record");
    assert_eq!(recovered.request.outcome, "incomplete");
    assert_eq!(
        recovered.request.error_kind.as_deref(),
        Some("process_interrupted")
    );
    assert_eq!(recovered.request.attempt_count, 0);
    assert_eq!(recovered.trace, None);
    assert!(recovered.attempts.is_empty());
    assert_eq!(
        store
            .recover_expired(request.deadline_at.at().unwrap())
            .await
            .expect("repeat recovery")
            .requests,
        0
    );

    // 已关闭队列同样不把观测失败返回客户端，也不能覆盖已经恢复的终态
    store
        .finalize_model_request(early_failure(&request))
        .await
        .expect("closed queue is fail open");
    assert_eq!(store.stats().dropped_total, 2);
    assert_eq!(store.stats().persisted_total, 1);
    assert_eq!(store.stats().write_failure_total, 0);
    assert_eq!(
        repository
            .usage_record_detail(request.id.as_str())
            .await
            .expect("unchanged recovery"),
        recovered
    );
    database.close().await;
}

#[tokio::test]
async fn zero_attempt_dropped_create_does_not_make_finalize_an_upsert() {
    let Some(database) = TestDatabase::create("zero_attempt_create_dropped").await else {
        return;
    };
    let (store, writer) = BufferedExecutionStore::with_limits(
        Arc::new(PgExecutionStore::new(database.pool.clone())),
        NonZeroUsize::new(2).expect("capacity"),
        NonZeroUsize::new(8_192).expect("byte budget"),
    );
    let mut request = accepted_request("req_zero_attempt_create_dropped");
    // 合成大 header 只触发 Create 字节预算；Finalize 不携带 user_agent，仍可入队
    request.user_agent = Some("x".repeat(16_384));
    store
        .create_model_request(request.clone())
        .await
        .expect("oversized create is fail open");
    assert_eq!(store.stats().dropped_total, 1);
    store
        .finalize_model_request(early_failure(&request))
        .await
        .expect("enqueue finalization without create");
    assert_eq!(store.stats().enqueued_total, 1);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    writer
        .run(cancellation)
        .await
        .expect("drain orphan finalization");
    assert_eq!(store.stats().persisted_total, 0);
    assert_eq!(store.stats().write_failure_total, 1);
    assert_eq!(store.stats().queued_bytes, 0);
    let count: i64 = sqlx::query_scalar("select count(*) from model_requests")
        .fetch_one(&database.pool)
        .await
        .expect("no fabricated row");
    assert_eq!(count, 0);
    assert_eq!(
        store
            .recover_expired(request.deadline_at.at().unwrap())
            .await
            .expect("nothing to recover")
            .requests,
        0
    );
    database.close().await;
}

#[tokio::test]
async fn zero_attempt_postgres_write_failures_are_not_retried_and_do_not_stop_the_queue() {
    let Some(database) = TestDatabase::create("zero_attempt_write_failure").await else {
        return;
    };
    // 只在随机测试 schema 注入语句失败；序列不随语句回滚，用于核对实际写入次数
    sqlx::raw_sql(
        "create sequence zero_attempt_rejected_writes;
         create function reject_zero_attempt_observation() returns trigger language plpgsql as $$
         begin
           if (TG_OP = 'INSERT' and NEW.id = 'req_zero_attempt_create_rejected')
              or (TG_OP = 'UPDATE' and NEW.id = 'req_zero_attempt_finalize_rejected'
                  and NEW.outcome = 'failed') then
             perform nextval('zero_attempt_rejected_writes');
             raise exception 'synthetic observation write failure';
           end if;
           return NEW;
         end $$;
         create trigger reject_zero_attempt_observation
         before insert or update on model_requests
         for each row execute function reject_zero_attempt_observation();",
    )
    .execute(&database.pool)
    .await
    .expect("install isolated write failure");
    let (store, writer) = BufferedExecutionStore::with_capacity(
        Arc::new(PgExecutionStore::new(database.pool.clone())),
        NonZeroUsize::new(6).expect("capacity"),
    );
    let request = accepted_request("req_zero_attempt_create_rejected");
    for id in [
        "req_zero_attempt_create_rejected",
        "req_zero_attempt_finalize_rejected",
        "req_zero_attempt_after_rejection",
    ] {
        let mut request = request.clone();
        request.id = ModelRequestId::new(id).expect("request id");
        store
            .create_model_request(request.clone())
            .await
            .expect("enqueue create");
        store
            .finalize_model_request(early_failure(&request))
            .await
            .expect("enqueue finalization");
    }
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    writer
        .run(cancellation)
        .await
        .expect("continue draining after PG errors");
    let stats = store.stats();
    assert_eq!(stats.enqueued_total, 6);
    assert_eq!(stats.persisted_total, 3);
    assert_eq!(stats.write_failure_total, 3);
    assert_eq!(stats.dropped_total, 0);
    assert_eq!(stats.queued_items, 0);
    assert_eq!(stats.queued_bytes, 0);
    let rejected_writes: i64 =
        sqlx::query_scalar("select last_value from zero_attempt_rejected_writes")
            .fetch_one(&database.pool)
            .await
            .expect("count failed PostgreSQL writes without rollback");
    assert_eq!(rejected_writes, 2);
    let rows: Vec<(String, String, Option<serde_json::Value>)> =
        sqlx::query_as("select id, outcome, diagnostic_trace_json from model_requests order by id")
            .fetch_all(&database.pool)
            .await
            .expect("load successfully persisted rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "req_zero_attempt_after_rejection");
    assert_eq!(rows[0].1, "failed");
    assert!(rows[0].2.is_some());
    assert_eq!(
        rows[1],
        (
            "req_zero_attempt_finalize_rejected".to_owned(),
            "running".to_owned(),
            None
        )
    );
    assert_eq!(
        store
            .recover_expired(request.deadline_at.at().unwrap())
            .await
            .expect("recover only inserted unfinished row")
            .requests,
        1
    );
    let recovered = observability_repository(&database.pool)
        .usage_record_detail("req_zero_attempt_finalize_rejected")
        .await
        .expect("recovered detail");
    assert_eq!(
        recovered.request.error_kind.as_deref(),
        Some("process_interrupted")
    );
    assert_eq!(recovered.request.attempt_count, 0);
    assert!(recovered.attempts.is_empty());
    assert_eq!(recovered.trace, None);
    database.close().await;
}
