use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRequest, ClientAdmissionRestoreResult,
};
use gateway_core::engine::execution::{
    ProviderCircuitDecision, ProviderCircuitError, ProviderCircuitPort,
};
use gateway_core::engine::{CancellationToken, ModelRequestId};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::ProviderKind;
use gateway_store::redis::{BufferedClientAdmissionPort, BufferedProviderCircuitPort};

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

impl ProviderCircuitPort for RecordingCoordination {
    fn decision<'a>(
        &'a self,
        _: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<ProviderCircuitDecision, ProviderCircuitError>> {
        Box::pin(async { Ok(ProviderCircuitDecision::Allow) })
    }

    fn observe_failure<'a>(
        &'a self,
        _: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<(), ProviderCircuitError>> {
        Box::pin(async move {
            self.record("circuit_failure");
            Ok(())
        })
    }

    fn observe_success<'a>(
        &'a self,
        _: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<(), ProviderCircuitError>> {
        Box::pin(async move {
            self.record("circuit_success");
            Ok(())
        })
    }
}

#[tokio::test]
async fn full_recoverable_coordination_queues_should_drop_writes_without_waiting() {
    let inner = Arc::new(RecordingCoordination::default());
    let (admissions, _admission_writer) = BufferedClientAdmissionPort::with_capacity(
        inner.clone(),
        NonZeroUsize::new(1).expect("capacity"),
    );
    let (circuits, _circuit_writer) = BufferedProviderCircuitPort::with_capacity(
        inner.clone(),
        NonZeroUsize::new(1).expect("capacity"),
    );
    let client = ClientApiKeyId::new("key_buffer_test").expect("client key");
    let request = ModelRequestId::new("req_buffer_test").expect("request ID");
    let provider = ProviderKind::new("openai").expect("provider");

    tokio::time::timeout(Duration::from_millis(50), async {
        admissions
            .release(&client, &request)
            .await
            .expect("first admission enqueue");
        admissions
            .release(&client, &request)
            .await
            .expect("full admission queue remains fail-open");
        circuits
            .observe_success(&provider)
            .await
            .expect("first circuit enqueue");
        circuits
            .observe_failure(&provider)
            .await
            .expect("full circuit queue remains fail-open");
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
    let (circuits, circuit_writer) = BufferedProviderCircuitPort::with_capacity(
        inner.clone(),
        NonZeroUsize::new(8).expect("capacity"),
    );
    let client = ClientApiKeyId::new("key_writer_test").expect("client key");
    let request = ModelRequestId::new("req_writer_test").expect("request ID");
    let provider = ProviderKind::new("openai").expect("provider");
    admissions
        .release(&client, &request)
        .await
        .expect("enqueue admission release");
    circuits
        .observe_success(&provider)
        .await
        .expect("enqueue circuit feedback");
    let cancellation = CancellationToken::new();
    let tasks = [
        spawn_writer(Arc::new(admission_writer), cancellation.clone()),
        spawn_writer(Arc::new(circuit_writer), cancellation.clone()),
    ];
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if inner.operations().len() == 2 {
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
    assert_eq!(operations, ["admission", "circuit_success"]);
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
