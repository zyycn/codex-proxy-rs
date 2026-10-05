//! 验证 Redis 协调写入队列的有界丢弃、刷写与容量释放顺序

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::engine::ModelRequestId;
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRejection, ClientAdmissionRequest, ClientAdmissionRestoreResult,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::policy::{ClientApiKeyId, RateLimits};
use gateway_store::redis::BufferedClientAdmissionPort;

#[derive(Default)]
struct RecordingCoordination {
    operations: Mutex<Vec<&'static str>>,
}

impl RecordingCoordination {
    fn record(&self, operation: &'static str) {
        self.operations
            .lock()
            .expect("coordination operations lock")
            .push(operation);
    }

    fn operations(&self) -> Vec<&'static str> {
        self.operations
            .lock()
            .expect("coordination operations lock")
            .clone()
    }
}

impl ClientAdmissionPort for RecordingCoordination {
    fn abandon(
        &self,
        key: &gateway_core::policy::ClientApiKeyId,
        request: &gateway_core::engine::ModelRequestId,
    ) {
        let _ = futures::FutureExt::now_or_never(self.release(key, request));
    }

    fn admit(
        &self,
        _: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>> {
        Box::pin(async { Ok(ClientAdmissionDecision::Granted) })
    }

    fn release<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>> {
        Box::pin(async move {
            self.record("admission");
            Ok(true)
        })
    }

    fn restore(
        &self,
        _: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        Box::pin(async { Ok(ClientAdmissionRestoreResult::default()) })
    }
}

struct CapacityCoordination {
    active: Mutex<BTreeSet<String>>,
    maximum: usize,
}

impl CapacityCoordination {
    fn new(maximum: usize) -> Self {
        Self {
            active: Mutex::new(BTreeSet::new()),
            maximum,
        }
    }

    fn active(&self) -> usize {
        self.active.lock().expect("active admission lock").len()
    }
}

impl ClientAdmissionPort for CapacityCoordination {
    fn abandon(&self, _: &ClientApiKeyId, request: &ModelRequestId) {
        self.active
            .lock()
            .expect("active admission lock")
            .remove(request.as_str());
    }

    fn admit(
        &self,
        request: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>> {
        Box::pin(async move {
            let mut active = self.active.lock().expect("active admission lock");
            if active.len() >= self.maximum {
                return Ok(ClientAdmissionDecision::Rejected(
                    ClientAdmissionRejection::ConcurrencyLimited,
                ));
            }
            active.insert(request.model_request_id.as_str().to_owned());
            Ok(ClientAdmissionDecision::Granted)
        })
    }

    fn release<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        request: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>> {
        Box::pin(async move {
            Ok(self
                .active
                .lock()
                .expect("active admission lock")
                .remove(request.as_str()))
        })
    }

    fn restore(
        &self,
        _: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        Box::pin(async { Ok(ClientAdmissionRestoreResult::default()) })
    }
}

#[tokio::test]
async fn full_recoverable_coordination_queues_should_drop_writes_without_waiting() {
    let inner = Arc::new(RecordingCoordination::default());
    let (admissions, _admission_writer) = BufferedClientAdmissionPort::with_capacity(
        inner.clone(),
        NonZeroUsize::new(1).expect("capacity"),
    );
    let client = ClientApiKeyId::new("key_buffer_test").expect("client key");
    let request = ModelRequestId::new("req_buffer_test").expect("request ID");

    tokio::time::timeout(Duration::from_millis(50), async {
        admissions
            .release(&client, &request)
            .await
            .expect("first admission enqueue");
        admissions
            .release(&client, &request)
            .await
            .expect("full admission queue remains fail-open");
    })
    .await
    .expect("coordination enqueue must never wait for Redis");

    assert!(inner.operations().is_empty());
}

#[tokio::test]
async fn redis_coordination_writers_should_flush_each_side_effect() {
    let inner = Arc::new(RecordingCoordination::default());
    let (admissions, admission_writer) = BufferedClientAdmissionPort::with_capacity(
        inner.clone(),
        NonZeroUsize::new(8).expect("capacity"),
    );
    let client = ClientApiKeyId::new("key_writer_test").expect("client key");
    let request = ModelRequestId::new("req_writer_test").expect("request ID");
    admissions
        .release(&client, &request)
        .await
        .expect("enqueue admission release");
    let cancellation = CancellationToken::new();
    let tasks = [spawn_writer(
        Arc::new(admission_writer),
        cancellation.clone(),
    )];
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if inner.operations().len() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("writers should drain queued coordination writes");
    cancellation.cancel();
    for task in tasks {
        task.await
            .expect("coordination writer task")
            .expect("coordination writer cancellation");
    }

    let mut operations = inner.operations();
    operations.sort_unstable();
    assert_eq!(operations, ["admission"]);
}

#[tokio::test]
async fn awaited_buffered_release_should_not_publish_capacity_before_writer_runs() {
    let inner = Arc::new(CapacityCoordination::new(8));
    let (admissions, admission_writer) = BufferedClientAdmissionPort::with_capacity(
        inner.clone(),
        NonZeroUsize::new(8).expect("capacity"),
    );
    let client = ClientApiKeyId::new("key_capacity_handoff").expect("client key");
    let request = |index| ClientAdmissionRequest {
        model_request_id: ModelRequestId::new(format!("req_capacity_handoff_{index}"))
            .expect("request ID"),
        client_api_key_id: client.clone(),
        lease_ttl: Duration::from_secs(60),
        allow_concurrency_acquire: true,
        limits: RateLimits {
            max_concurrency: 8,
            requests_per_minute: 0,
        },
    };

    let mut granted = Vec::new();
    for index in 0..8 {
        let request = request(index);
        assert_eq!(
            admissions.admit(request.clone()).await.expect("admit"),
            ClientAdmissionDecision::Granted
        );
        granted.push(request.model_request_id);
    }
    assert_eq!(inner.active(), 8);

    assert!(
        admissions
            .release(&client, &granted[0])
            .await
            .expect("enqueue release")
    );
    assert_eq!(inner.active(), 8, "await only confirms queue admission");
    let replacement = request(8);
    assert_eq!(
        admissions
            .admit(replacement.clone())
            .await
            .expect("admit before writer"),
        ClientAdmissionDecision::Rejected(ClientAdmissionRejection::ConcurrencyLimited)
    );

    let cancellation = CancellationToken::new();
    let task = spawn_writer(Arc::new(admission_writer), cancellation.clone());
    tokio::time::timeout(Duration::from_secs(1), async {
        while inner.active() == 8 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("writer should publish released capacity");
    assert_eq!(
        admissions
            .admit(replacement)
            .await
            .expect("admit after writer"),
        ClientAdmissionDecision::Granted
    );
    cancellation.cancel();
    task.await
        .expect("admission writer task")
        .expect("admission writer cancellation");
}

fn spawn_writer<T>(
    writer: Arc<T>,
    cancellation: CancellationToken,
) -> tokio::task::JoinHandle<Result<(), gateway_core::task::WorkerTaskError>>
where
    T: gateway_core::task::DaemonTask + 'static,
{
    tokio::spawn(async move { writer.run(cancellation).await })
}
