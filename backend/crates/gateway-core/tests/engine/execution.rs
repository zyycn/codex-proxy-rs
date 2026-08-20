use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{executor::block_on, future::BoxFuture};
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRequest, ClientAdmissionRestoreResult,
};
use gateway_core::engine::continuation::{
    NativeContinuationPin, NativeContinuationPort, NativeContinuationStoreError, PreviousResponseId,
};
use gateway_core::engine::credential::{
    AccountSelectionPolicy, CredentialRevision, ProviderAccountId, RotationStrategy,
};
use gateway_core::engine::execution::{
    ClientApiKeyUsageSink, ClientTransport, DefaultExecutionService, ExecutionRequestMetadata,
    ExecutionService, ProviderCircuitDecision, ProviderCircuitError, ProviderCircuitPort,
    StartExecution, StartProviderExecution, provider_failure_affects_circuit,
};
use gateway_core::engine::probe::{AccountProbe, AccountProbeRequest};
use gateway_core::engine::provider::{
    Provider, ProviderCallMetadata, ProviderCatalogGeneration, ProviderModelCapabilities,
    ProviderRegistry, ProviderRequest, ProviderResource, ProviderStream, UpstreamTransport,
};
use gateway_core::engine::{
    AttemptContext, AttemptRecord, ExecutionStore, IntermediateFailure, ModelRequestFinalization,
    ModelRequestId, NewModelRequest, ProbeFailure, RecoveryReport, UpstreamSendState,
};
use gateway_core::error::{
    ClientVisibleUpstreamResponse, GatewayErrorKind, ProviderError, ProviderErrorKind, StoreError,
    StoreErrorKind,
};
use gateway_core::operation::{
    GenerateRequest, ImageRequest, ImageRequestKind, Operation, OperationKind, ProtocolPayload,
    RawJsonPayload,
};
use gateway_core::policy::{ClientApiKeyId, ClientPolicy, PlaintextClientApiKey, RateLimits};
use gateway_core::routing::snapshot::RuntimeSnapshotHandle;
use gateway_core::routing::{
    ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities, ProviderKind,
    ProviderModel, PublicModelId, RuntimeAccount, RuntimeAccountDirectory, RuntimeSnapshot,
    UpstreamModelId,
};
use serde_json::json;

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
        Arc::new(RecordingClientApiKeyUsage::default()),
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

#[test]
fn probe_failures_should_be_observable_without_a_model_request_row() {
    let store = Arc::new(TrackingExecutionStore::default());
    let providers = ProviderRegistry::new([Arc::new(FailingProvider) as Arc<dyn Provider>])
        .expect("provider registry");
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(probe_snapshot()),
        store.clone(),
        providers,
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedCircuits),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );

    let error = block_on(service.probe(AccountProbeRequest {
        account_id: ProviderAccountId::new("acct_probe").expect("account ID"),
        provider_kind: ProviderKind::new("openai").expect("provider kind"),
        upstream_model: UpstreamModelId::new("gpt-probe").expect("model ID"),
        operation: probe_operation(),
    }))
    .expect_err("the provider rejects every probe");

    assert_eq!(error.kind(), GatewayErrorKind::UpstreamUnavailable);
    let upstream = error
        .upstream_response()
        .expect("probe must preserve its request-local upstream response");
    assert_eq!(upstream.status(), 502);
    assert_eq!(
        upstream.content_type(),
        Some(b"application/json".as_slice())
    );
    assert_eq!(
        upstream.body(),
        &Bytes::from_static(br#"{"error":{"message":"source upstream failure"}}"#),
    );
    assert!(!format!("{error:?}").contains("source upstream failure"));
    assert!(!store.touched.load(Ordering::SeqCst));
    assert_eq!(store.probe_failures(), vec!["transport".to_owned()]);
}

#[test]
fn probe_observation_store_failure_preserves_the_provider_error() {
    let store = Arc::new(TrackingExecutionStore {
        fail_probe_observation: AtomicBool::new(true),
        ..TrackingExecutionStore::default()
    });
    let providers = ProviderRegistry::new([Arc::new(FailingProvider) as Arc<dyn Provider>])
        .expect("provider registry");
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(probe_snapshot()),
        store.clone(),
        providers,
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedCircuits),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );

    let error = block_on(service.probe(AccountProbeRequest {
        account_id: ProviderAccountId::new("acct_probe").expect("account ID"),
        provider_kind: ProviderKind::new("openai").expect("provider kind"),
        upstream_model: UpstreamModelId::new("gpt-probe").expect("model ID"),
        operation: probe_operation(),
    }))
    .expect_err("the provider error must survive observation failure");

    assert_eq!(error.kind(), GatewayErrorKind::UpstreamUnavailable);
    assert!(!store.touched.load(Ordering::SeqCst));
    assert!(store.probe_failures().is_empty());
}

struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(Vec::new())
    }

    async fn execute(
        &self,
        _request: ProviderRequest,
        _context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        Err(
            ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::NotSent)
                .with_client_visible_upstream_response(ClientVisibleUpstreamResponse::new(
                    502,
                    Some(b"application/json".to_vec()),
                    Bytes::from_static(br#"{"error":{"message":"source upstream failure"}}"#),
                )),
        )
    }
}

struct ColdFailingProvider;

#[async_trait]
impl Provider for ColdFailingProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(Vec::new())
    }

    async fn execute(
        &self,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let metadata = ProviderCallMetadata::for_provider_endpoint(
            request.candidate().provider().clone(),
            ProviderResource::Account {
                id: ProviderAccountId::new("acct_usage").expect("account"),
                revision: CredentialRevision::new(1).expect("revision"),
            },
            UpstreamTransport::new("http_json").expect("transport"),
        );
        Ok(ProviderStream::new(
            metadata,
            futures::stream::once(async {
                Err(ProviderError::new(
                    ProviderErrorKind::Transport,
                    UpstreamSendState::NotSent,
                ))
            }),
            (),
        ))
    }
}

#[test]
fn successful_authentication_should_record_client_key_usage() {
    let usage = Arc::new(RecordingClientApiKeyUsage::default());
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(client_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedCircuits),
        Arc::new(UnusedContinuation),
        usage.clone(),
    );

    service
        .authenticate("sk_usage_test")
        .expect("successful authentication");

    assert_eq!(usage.recorded(), vec!["key_usage_test".to_owned()]);
}

#[test]
fn provider_endpoint_should_persist_its_real_v1_endpoint() {
    let store = Arc::new(TrackingExecutionStore::default());
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(client_snapshot()),
        store.clone(),
        ProviderRegistry::new([Arc::new(ColdFailingProvider) as Arc<dyn Provider>])
            .expect("provider registry"),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedCircuits),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    let client = service
        .authenticate("sk_usage_test")
        .expect("authenticated client");
    let operation = Operation::GenerateImage(ImageRequest::from_raw_json(
        ImageRequestKind::Generation,
        RawJsonPayload::new(
            "openai",
            Bytes::from_static(br#"{"model":"gpt-image-2","prompt":"hello"}"#),
        )
        .expect("image payload"),
    ));

    let mut started = block_on(service.start_provider_endpoint(StartProviderExecution {
        client,
        provider: ProviderKind::new("openai").expect("provider"),
        operation,
        metadata: ExecutionRequestMetadata {
            protocol: "openai".to_owned(),
            endpoint: "/v1/images/generations".to_owned(),
            transport: ClientTransport::HttpJson,
            stream: false,
            client_ip: None,
            user_agent: None,
            previous_response_id: None,
        },
    }))
    .expect("provider endpoint request should start without a text catalog entry");

    let error = block_on(started.session.collect_uncommitted())
        .expect_err("the cold provider stops execution after persistence");
    assert_eq!(
        store.model_request_endpoints(),
        vec!["/v1/images/generations".to_owned()],
        "execution error: {error:?}"
    );
    assert_eq!(store.requested_models(), vec![None]);
    let upstream_models = store.upstream_models();
    assert!(!upstream_models.is_empty());
    assert!(upstream_models.iter().all(Option::is_none));
}

#[test]
fn circuit_store_failure_should_fail_open_during_request_start() {
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(FailingDecisionCircuits),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    let client = service
        .authenticate("sk_start_test")
        .expect("authenticated client");

    let started = block_on(service.start(StartExecution {
        client,
        public_model: PublicModelId::new("gpt-start").expect("public model"),
        operation: start_operation(),
        metadata: ExecutionRequestMetadata {
            protocol: "openai".to_owned(),
            endpoint: "/v1/responses".to_owned(),
            transport: ClientTransport::HttpJson,
            stream: false,
            client_ip: None,
            user_agent: None,
            previous_response_id: None,
        },
    }))
    .expect("recoverable circuit state must not reject the request");

    assert!(!started.session.is_finalized());
}

#[test]
fn slow_circuit_store_should_time_out_and_fail_open_during_request_start() {
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(PendingDecisionCircuits),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    let client = service
        .authenticate("sk_start_test")
        .expect("authenticated client");
    let started_at = Instant::now();

    let started = block_on(service.start(StartExecution {
        client,
        public_model: PublicModelId::new("gpt-start").expect("public model"),
        operation: start_operation(),
        metadata: ExecutionRequestMetadata {
            protocol: "openai".to_owned(),
            endpoint: "/v1/responses".to_owned(),
            transport: ClientTransport::HttpJson,
            stream: false,
            client_ip: None,
            user_agent: None,
            previous_response_id: None,
        },
    }))
    .expect("slow recoverable circuit state must not reject the request");

    assert!(started_at.elapsed() < Duration::from_secs(2));
    assert!(!started.session.is_finalized());
}

#[test]
fn known_catalog_should_reject_a_model_that_the_provider_did_not_publish() {
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedCircuits),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    let client = service
        .authenticate("sk_start_test")
        .expect("authenticated client");
    let model = "gpt-future-not-in-catalog";

    let result = block_on(service.start(StartExecution {
        client,
        public_model: PublicModelId::from_client_wire(model).expect("client model"),
        operation: start_operation_for_model(model),
        metadata: ExecutionRequestMetadata {
            protocol: "openai".to_owned(),
            endpoint: "/v1/responses".to_owned(),
            transport: ClientTransport::HttpJson,
            stream: false,
            client_ip: None,
            user_agent: None,
            previous_response_id: None,
        },
    }));
    let Err(error) = result else {
        panic!("a known provider catalog must be authoritative for model availability");
    };

    assert_eq!(error.kind(), GatewayErrorKind::NoAvailableProvider);
}

#[test]
fn continuation_owned_by_another_client_api_key_should_fail_closed() {
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedCircuits),
        Arc::new(RejectedContinuation::OwnershipMismatch),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    let client = service
        .authenticate("sk_start_test")
        .expect("authenticated client");

    let result = block_on(service.start(StartExecution {
        client,
        public_model: PublicModelId::new("gpt-start").expect("public model"),
        operation: start_operation(),
        metadata: execution_metadata_with_continuation(),
    }));
    let Err(error) = result else {
        panic!("cross-client continuation must be rejected");
    };

    assert_eq!(error.kind(), GatewayErrorKind::PolicyDenied);
}

#[test]
fn invalid_continuation_record_should_not_be_forwarded_as_an_external_handle() {
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedCircuits),
        Arc::new(RejectedContinuation::InvalidData),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    let client = service
        .authenticate("sk_start_test")
        .expect("authenticated client");

    let result = block_on(service.start(StartExecution {
        client,
        public_model: PublicModelId::new("gpt-start").expect("public model"),
        operation: start_operation(),
        metadata: execution_metadata_with_continuation(),
    }));
    let Err(error) = result else {
        panic!("invalid continuation state must be rejected");
    };

    assert_eq!(error.kind(), GatewayErrorKind::Internal);
}

fn execution_metadata_with_continuation() -> ExecutionRequestMetadata {
    ExecutionRequestMetadata {
        protocol: "openai".to_owned(),
        endpoint: "/v1/responses".to_owned(),
        transport: ClientTransport::HttpJson,
        stream: false,
        client_ip: None,
        user_agent: None,
        previous_response_id: Some(PreviousResponseId::new("response-private")),
    }
}

#[derive(Default)]
struct RecordingClientApiKeyUsage {
    ids: Mutex<Vec<String>>,
}

impl RecordingClientApiKeyUsage {
    fn recorded(&self) -> Vec<String> {
        self.ids.lock().expect("recorded API Key IDs").clone()
    }
}

impl ClientApiKeyUsageSink for RecordingClientApiKeyUsage {
    fn record_used(&self, key_id: &ClientApiKeyId) {
        self.ids
            .lock()
            .expect("recorded API Key IDs")
            .push(key_id.as_str().to_owned());
    }
}

#[derive(Default)]
struct TrackingExecutionStore {
    touched: AtomicBool,
    probe_failures: Mutex<Vec<String>>,
    model_request_endpoints: Mutex<Vec<String>>,
    requested_models: Mutex<Vec<Option<String>>>,
    upstream_models: Mutex<Vec<Option<String>>>,
    fail_probe_observation: AtomicBool,
}

impl TrackingExecutionStore {
    fn touch(&self) {
        self.touched.store(true, Ordering::SeqCst);
    }

    fn probe_failures(&self) -> Vec<String> {
        self.probe_failures
            .lock()
            .expect("probe failures lock")
            .clone()
    }

    fn model_request_endpoints(&self) -> Vec<String> {
        self.model_request_endpoints
            .lock()
            .expect("model request endpoints lock")
            .clone()
    }

    fn requested_models(&self) -> Vec<Option<String>> {
        self.requested_models
            .lock()
            .expect("requested models lock")
            .clone()
    }

    fn upstream_models(&self) -> Vec<Option<String>> {
        self.upstream_models
            .lock()
            .expect("upstream models lock")
            .clone()
    }
}

#[async_trait]
impl ExecutionStore for TrackingExecutionStore {
    async fn create_model_request(&self, request: NewModelRequest) -> Result<(), StoreError> {
        self.touch();
        self.requested_models
            .lock()
            .expect("requested models lock")
            .push(
                request
                    .requested_model
                    .as_ref()
                    .map(|model| model.as_str().to_owned()),
            );
        self.model_request_endpoints
            .lock()
            .expect("model request endpoints lock")
            .push(request.endpoint);
        Ok(())
    }

    async fn record_attempt(&self, attempt: AttemptRecord) -> Result<(), StoreError> {
        self.touch();
        self.upstream_models
            .lock()
            .expect("upstream models lock")
            .push(
                attempt
                    .upstream_model_id
                    .as_ref()
                    .map(|model| model.as_str().to_owned()),
            );
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

    async fn record_probe_failure(&self, failure: ProbeFailure) -> Result<(), StoreError> {
        if self.fail_probe_observation.load(Ordering::SeqCst) {
            return Err(StoreError::new(StoreErrorKind::Unavailable));
        }
        self.probe_failures
            .lock()
            .expect("probe failures lock")
            .push(failure.error.kind().as_str().to_owned());
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

struct FailingDecisionCircuits;

impl ProviderCircuitPort for FailingDecisionCircuits {
    fn decision<'a>(
        &'a self,
        _: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<ProviderCircuitDecision, ProviderCircuitError>> {
        Box::pin(async { Err(ProviderCircuitError) })
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

struct PendingDecisionCircuits;

impl ProviderCircuitPort for PendingDecisionCircuits {
    fn decision<'a>(
        &'a self,
        _: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<ProviderCircuitDecision, ProviderCircuitError>> {
        Box::pin(futures::future::pending())
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

    fn record<'a>(
        &'a self,
        _: NativeContinuationPin,
    ) -> BoxFuture<'a, Result<(), NativeContinuationStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

enum RejectedContinuation {
    OwnershipMismatch,
    InvalidData,
}

impl NativeContinuationPort for RejectedContinuation {
    fn resolve<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a PreviousResponseId,
    ) -> BoxFuture<'a, Result<Option<NativeContinuationPin>, NativeContinuationStoreError>> {
        Box::pin(async move {
            Err(match self {
                Self::OwnershipMismatch => NativeContinuationStoreError::ownership_mismatch(),
                Self::InvalidData => NativeContinuationStoreError::invalid_data("invalid record"),
            })
        })
    }

    fn record<'a>(
        &'a self,
        _: NativeContinuationPin,
    ) -> BoxFuture<'a, Result<(), NativeContinuationStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

fn account_scope(provider: &ProviderKind, account_id: &str) -> Arc<FrozenAccountScope> {
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new(account_id).expect("account ID"),
            RuntimeAccount::new(provider.clone(), BTreeSet::new()),
        )]))),
        ClientRoutingScope::all_accounts(),
    ))
}

fn probe_snapshot() -> RuntimeSnapshot {
    let provider = ProviderKind::new("openai").expect("provider kind");
    let capabilities =
        ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(16_000));
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

fn client_snapshot() -> RuntimeSnapshot {
    let provider = ProviderKind::new("openai").expect("provider kind");
    RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("config revision"),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            std::num::NonZeroU32::new(1).expect("concurrency"),
            Duration::from_millis(1),
        ),
        vec![provider.clone()],
        Vec::new(),
        vec![ClientPolicy::new(
            ClientApiKeyId::new("key_usage_test").expect("client API key ID"),
            PlaintextClientApiKey::new("sk_usage_test").expect("plaintext client API key"),
            account_scope(&provider, "acct_usage"),
            true,
            RateLimits::unlimited(),
        )],
    )
    .expect("client snapshot")
}

fn start_snapshot() -> RuntimeSnapshot {
    let provider = ProviderKind::new("openai").expect("provider kind");
    let capabilities =
        ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(16_000));
    RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("config revision"),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            std::num::NonZeroU32::new(1).expect("concurrency"),
            Duration::from_millis(1),
        ),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider.clone(),
            UpstreamModelId::new("gpt-start").expect("model ID"),
            capabilities,
        )],
        vec![ClientPolicy::new(
            ClientApiKeyId::new("key_start_test").expect("client API key ID"),
            PlaintextClientApiKey::new("sk_start_test").expect("plaintext client API key"),
            account_scope(&provider, "acct_start"),
            true,
            RateLimits::unlimited(),
        )],
    )
    .expect("start snapshot")
}

fn probe_operation() -> Operation {
    let body = json!({
        "model": "gpt-probe",
        "input": [{"type": "message", "role": "user", "content": "ping"}],
    });
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().expect("request object").clone())
            .expect("OpenAI payload"),
    ))
}

fn start_operation() -> Operation {
    start_operation_for_model("gpt-start")
}

fn start_operation_for_model(model: &str) -> Operation {
    let body = json!({
        "model": model,
        "input": [{"type": "message", "role": "user", "content": "ping"}],
    });
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().expect("request object").clone())
            .expect("OpenAI payload"),
    ))
}
