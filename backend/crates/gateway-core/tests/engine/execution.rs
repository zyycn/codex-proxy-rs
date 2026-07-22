use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use futures::{executor::block_on, future::BoxFuture};
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRequest, ClientAdmissionRestoreResult,
};
use gateway_core::engine::continuation::{
    NativeContinuationPin, NativeContinuationPort, NativeContinuationStoreError, PreviousResponseId,
};
use gateway_core::engine::credential::{
    AccountSelectionPolicy, ProviderAccountId, RotationStrategy,
};
use gateway_core::engine::execution::{
    DefaultExecutionService, ProviderCircuitDecision, ProviderCircuitError, ProviderCircuitPort,
    provider_failure_affects_circuit,
};
use gateway_core::engine::probe::{AccountProbe, AccountProbeRequest};
use gateway_core::engine::provider::ProviderRegistry;
use gateway_core::engine::{
    AttemptRecord, ExecutionStore, IntermediateFailure, ModelRequestFinalization, ModelRequestId,
    NewModelRequest, RecoveryReport, UpstreamSendState,
};
use gateway_core::error::{GatewayErrorKind, ProviderErrorKind, StoreError};
use gateway_core::operation::{
    ContentPart, GenerateRequest, Message, MessageRole, Operation, OperationKind,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::snapshot::RuntimeSnapshotHandle;
use gateway_core::routing::{
    ConfigRevision, ModelCapabilities, ProviderKind, ProviderModel, RuntimeSnapshot,
    UpstreamModelId,
};

#[test]
fn only_provider_attributable_failures_should_affect_circuit() {
    assert!(provider_failure_affects_circuit(ProviderErrorKind::Timeout));
    assert!(provider_failure_affects_circuit(
        ProviderErrorKind::Transport
    ));
    assert!(!provider_failure_affects_circuit(
        ProviderErrorKind::RateLimited
    ));
    assert!(!provider_failure_affects_circuit(
        ProviderErrorKind::InvalidRequest
    ));
}

#[test]
fn account_probe_should_not_write_to_the_persistent_execution_store() {
    let store = Arc::new(TrackingExecutionStore::default());
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(probe_snapshot()),
        store.clone(),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedCircuits),
        Arc::new(UnusedContinuation),
    );

    let error = block_on(service.probe(AccountProbeRequest {
        account_id: ProviderAccountId::new("acct_probe").expect("account ID"),
        provider_kind: ProviderKind::new("openai").expect("provider kind"),
        upstream_model: UpstreamModelId::new("gpt-probe").expect("model ID"),
        operation: probe_operation(),
    }))
    .expect_err("empty Provider registry should stop the probe after it starts");

    assert_eq!(error.kind(), GatewayErrorKind::NoAvailableProvider);
    assert!(!store.touched.load(Ordering::SeqCst));
}

#[derive(Default)]
struct TrackingExecutionStore {
    touched: AtomicBool,
}

impl TrackingExecutionStore {
    fn touch(&self) {
        self.touched.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl ExecutionStore for TrackingExecutionStore {
    async fn create_model_request(&self, _: NewModelRequest) -> Result<(), StoreError> {
        self.touch();
        Ok(())
    }

    async fn record_attempt(&self, _: AttemptRecord) -> Result<(), StoreError> {
        self.touch();
        Ok(())
    }

    async fn mark_send_state(
        &self,
        _: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        self.touch();
        Ok(())
    }

    async fn mark_downstream_committed(
        &self,
        _: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        self.touch();
        Ok(())
    }

    async fn record_client_status(&self, _: &ModelRequestId, _: u16) -> Result<(), StoreError> {
        self.touch();
        Ok(())
    }

    async fn record_intermediate_failure(&self, _: IntermediateFailure) -> Result<(), StoreError> {
        self.touch();
        Ok(())
    }

    async fn finalize_model_request(&self, _: ModelRequestFinalization) -> Result<(), StoreError> {
        self.touch();
        Ok(())
    }

    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        self.touch();
        Ok(RecoveryReport::default())
    }
}

struct UnusedAdmissions;

impl ClientAdmissionPort for UnusedAdmissions {
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
        Box::pin(async { Ok(true) })
    }

    fn restore(
        &self,
        _: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        Box::pin(async { Ok(ClientAdmissionRestoreResult::default()) })
    }
}

struct UnusedCircuits;

impl ProviderCircuitPort for UnusedCircuits {
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
        Box::pin(async { Ok(()) })
    }

    fn observe_success<'a>(
        &'a self,
        _: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<(), ProviderCircuitError>> {
        Box::pin(async { Ok(()) })
    }
}

struct UnusedContinuation;

impl NativeContinuationPort for UnusedContinuation {
    fn resolve<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a PreviousResponseId,
    ) -> BoxFuture<'a, Result<Option<NativeContinuationPin>, NativeContinuationStoreError>> {
        Box::pin(async { Ok(None) })
    }
}

fn probe_snapshot() -> RuntimeSnapshot {
    let provider = ProviderKind::new("openai").expect("provider kind");
    let capabilities = ModelCapabilities::new(
        BTreeSet::from([OperationKind::Generate]),
        128_000,
        Some(16_000),
    );
    RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("config revision"),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            std::num::NonZeroU32::new(1).expect("concurrency"),
            Duration::from_millis(1),
        ),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            UpstreamModelId::new("gpt-probe").expect("model ID"),
            capabilities,
        )],
        Vec::new(),
    )
    .expect("probe snapshot")
}

fn probe_operation() -> Operation {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("ping".to_owned())],
    )
    .expect("probe message");
    Operation::Generate(GenerateRequest::new(vec![message]).expect("probe request"))
}
