use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use gateway_core::engine::{
    AttemptRecord, CancellationToken, ExecutionStore, IntermediateFailure,
    ModelRequestFinalization, ModelRequestId, NewModelRequest, ProbeFailure, RecoveryReport,
    UpstreamSendState,
};
use gateway_core::error::StoreError;
use gateway_core::task::DaemonTask as _;
use gateway_store::postgres::BufferedExecutionStore;

#[derive(Default)]
struct RecordingStore {
    operations: Mutex<Vec<&'static str>>,
}

impl RecordingStore {
    fn record(&self, operation: &'static str) {
        self.operations
            .lock()
            .expect("operations lock")
            .push(operation);
    }
}

#[async_trait]
impl ExecutionStore for RecordingStore {
    async fn create_model_request(&self, _: NewModelRequest) -> Result<(), StoreError> {
        self.record("create");
        Ok(())
    }

    async fn record_attempt(&self, _: AttemptRecord) -> Result<(), StoreError> {
        self.record("attempt");
        Ok(())
    }

    async fn mark_send_state(
        &self,
        _: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        self.record("send");
        Ok(())
    }

    async fn mark_downstream_committed(
        &self,
        _: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        self.record("commit");
        Ok(())
    }

    async fn record_client_status(&self, _: &ModelRequestId, _: u16) -> Result<(), StoreError> {
        self.record("status");
        Ok(())
    }

    async fn record_intermediate_failure(&self, _: IntermediateFailure) -> Result<(), StoreError> {
        self.record("intermediate_failure");
        Ok(())
    }

    async fn record_probe_failure(&self, _: ProbeFailure) -> Result<(), StoreError> {
        self.record("probe_failure");
        Ok(())
    }

    async fn finalize_model_request(&self, _: ModelRequestFinalization) -> Result<(), StoreError> {
        self.record("finalize");
        Ok(())
    }

    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        self.record("recover");
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
}
