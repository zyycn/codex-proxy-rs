mod auth;
mod error;
mod models;
mod responses;
mod router;

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRequest, ClientAdmissionRestoreResult,
};
use gateway_core::engine::continuation::{
    NativeContinuationPin, NativeContinuationPort, NativeContinuationStoreError, PreviousResponseId,
};
use gateway_core::engine::credential::{AccountSelectionPolicy, RotationStrategy};
use gateway_core::engine::execution::{
    AuthenticatedClient, DefaultExecutionService, ExecutionService, ProviderCircuitDecision,
    ProviderCircuitError, ProviderCircuitPort,
};
use gateway_core::engine::provider::ProviderRegistry;
use gateway_core::engine::{
    AttemptRecord, ExecutionStore, IntermediateFailure, ModelRequestFinalization, ModelRequestId,
    NewModelRequest, RecoveryReport, UpstreamSendState,
};
use gateway_core::error::StoreError;
use gateway_core::health::{WorkerHealthSnapshot, WorkerHealthSource};
use gateway_core::lifecycle::{ConnectionDraining, ConnectionGuard, ConnectionLifecycle};
use gateway_core::policy::{ClientApiKeyId, ClientPolicy, PlaintextClientApiKey, RateLimits};
use gateway_core::routing::snapshot::RuntimeSnapshotHandle;
use gateway_core::routing::{
    ConfigRevision, InstanceHealth, ModelCapabilities, ProviderInstance, ProviderInstanceId,
    ProviderKind, ProviderModel, RuntimeSnapshot, UpstreamModelId,
};

pub(super) async fn api_router(execution: Arc<dyn ExecutionService>) -> axum::Router {
    let admin = crate::admin::AdminTestFixture::new().await;
    gateway_api::initialize(
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".to_owned(),
        },
        execution,
        admin.services,
        Vec::new(),
        Arc::new(EmptyWorkerHealth),
        Arc::new(TestLifecycle::default()),
    )
    .expect("API bundle")
    .router()
}

pub(super) fn authenticated_client(plaintext: &str) -> AuthenticatedClient {
    let source = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot(plaintext)),
        Arc::new(UnusedExecutionStore),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedCircuits),
        Arc::new(UnusedContinuation),
    );
    source
        .authenticate(plaintext)
        .expect("authenticated client")
}

fn snapshot(plaintext: &str) -> RuntimeSnapshot {
    let provider = ProviderKind::new("openai").expect("provider");
    let instance_id = ProviderInstanceId::new("inst_openai_api_test").expect("instance");
    let capabilities = ModelCapabilities::new(
        BTreeSet::from([gateway_core::operation::OperationKind::Generate]),
        128_000,
        Some(16_000),
    );
    RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            NonZeroU32::new(2).expect("concurrency"),
            Duration::from_millis(1),
        ),
        vec![ProviderInstance::new(
            instance_id.clone(),
            provider.clone(),
            "https://api.example.invalid".to_owned(),
            true,
            InstanceHealth::Healthy,
        )],
        ["model-a", "model-b"]
            .into_iter()
            .map(|model| {
                ProviderModel::new(
                    instance_id.clone(),
                    UpstreamModelId::new(model).expect("model"),
                    capabilities.clone(),
                )
            })
            .collect(),
        vec![ClientPolicy::new(
            ClientApiKeyId::new("key_api_test").expect("key ID"),
            PlaintextClientApiKey::new(plaintext).expect("plaintext key"),
            provider,
            true,
            RateLimits::unlimited(),
        )],
    )
    .expect("runtime snapshot")
}

#[derive(Default)]
struct TestLifecycle {
    cancellation: gateway_core::engine::CancellationToken,
}

struct TestConnectionGuard;

impl ConnectionGuard for TestConnectionGuard {}

impl ConnectionLifecycle for TestLifecycle {
    fn try_register(&self) -> Result<Box<dyn ConnectionGuard>, ConnectionDraining> {
        Ok(Box::new(TestConnectionGuard))
    }

    fn cancellation(&self) -> gateway_core::engine::CancellationToken {
        self.cancellation.clone()
    }

    fn is_draining(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

struct EmptyWorkerHealth;

impl WorkerHealthSource for EmptyWorkerHealth {
    fn snapshot(&self) -> Vec<WorkerHealthSnapshot> {
        Vec::new()
    }
}

struct UnusedExecutionStore;

#[async_trait]
impl ExecutionStore for UnusedExecutionStore {
    async fn create_model_request(&self, _: NewModelRequest) -> Result<(), StoreError> {
        unreachable!("authentication fixture does not execute")
    }

    async fn record_attempt(&self, _: AttemptRecord) -> Result<(), StoreError> {
        unreachable!("authentication fixture does not execute")
    }

    async fn mark_send_state(
        &self,
        _: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        unreachable!("authentication fixture does not execute")
    }

    async fn mark_downstream_committed(
        &self,
        _: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        unreachable!("authentication fixture does not execute")
    }

    async fn record_client_status(&self, _: &ModelRequestId, _: u16) -> Result<(), StoreError> {
        unreachable!("authentication fixture does not execute")
    }

    async fn record_intermediate_failure(&self, _: IntermediateFailure) -> Result<(), StoreError> {
        unreachable!("authentication fixture does not execute")
    }

    async fn finalize_model_request(&self, _: ModelRequestFinalization) -> Result<(), StoreError> {
        unreachable!("authentication fixture does not execute")
    }

    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        unreachable!("authentication fixture does not execute")
    }
}

struct UnusedAdmissions;

impl ClientAdmissionPort for UnusedAdmissions {
    fn admit(
        &self,
        _: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>> {
        Box::pin(async { unreachable!("authentication fixture does not execute") })
    }

    fn release<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>> {
        Box::pin(async { unreachable!("authentication fixture does not execute") })
    }

    fn restore(
        &self,
        _: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        Box::pin(async { unreachable!("authentication fixture does not execute") })
    }
}

struct UnusedCircuits;

impl ProviderCircuitPort for UnusedCircuits {
    fn decision<'a>(
        &'a self,
        _: &'a ProviderInstanceId,
    ) -> BoxFuture<'a, Result<ProviderCircuitDecision, ProviderCircuitError>> {
        Box::pin(async { unreachable!("authentication fixture does not execute") })
    }

    fn observe_failure<'a>(
        &'a self,
        _: &'a ProviderInstanceId,
    ) -> BoxFuture<'a, Result<(), ProviderCircuitError>> {
        Box::pin(async { unreachable!("authentication fixture does not execute") })
    }

    fn observe_success<'a>(
        &'a self,
        _: &'a ProviderInstanceId,
    ) -> BoxFuture<'a, Result<(), ProviderCircuitError>> {
        Box::pin(async { unreachable!("authentication fixture does not execute") })
    }
}

struct UnusedContinuation;

impl NativeContinuationPort for UnusedContinuation {
    fn resolve<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a PreviousResponseId,
    ) -> BoxFuture<'a, Result<Option<NativeContinuationPin>, NativeContinuationStoreError>> {
        Box::pin(async { unreachable!("authentication fixture does not execute") })
    }
}
