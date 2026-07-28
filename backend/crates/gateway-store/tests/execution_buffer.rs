use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::engine::{
    AttemptRecord, CancellationToken, ExecutionStore, IntermediateFailure,
    ModelRequestFinalization, ModelRequestId, NewModelRequest, ProbeFailure, RecoveryReport,
    UpstreamSendState,
};
use gateway_core::error::{
    OpaqueUpstreamValue, ProviderError, ProviderErrorKind, StoreError, StoreErrorKind,
};
use gateway_core::routing::{ProviderKind, UpstreamModelId};
use gateway_core::task::DaemonTask as _;
use gateway_store::postgres::BufferedExecutionStore;

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
