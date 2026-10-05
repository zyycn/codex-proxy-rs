//! OpenAI 协议 HTTP 与 WebSocket 接口的测试入口

mod auth;
mod endpoint;
mod error;
mod images;
mod live;
mod middleware;
mod models;
mod responses;
mod router;
mod search;
mod usage;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_core::account::ProviderAccountId;
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRequest, ClientAdmissionRestoreResult,
};
use gateway_core::engine::continuation::{
    NativeContinuationPin, NativeContinuationPort, NativeContinuationStoreError, PreviousResponseId,
};
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientApiKeyUsageSink, DefaultExecutionService, ExecutionService,
};
use gateway_core::engine::provider::ProviderRegistry;
use gateway_core::engine::{
    AttemptRecord, ExecutionStore, IntermediateFailure, ModelRequestFinalization, ModelRequestId,
    NewModelRequest, RecoveryReport,
};
use gateway_core::error::StoreError;
use gateway_core::health::{WorkerHealthSnapshot, WorkerHealthSource};
use gateway_core::lifecycle::{ConnectionDraining, ConnectionGuard, ConnectionLifecycle};
use gateway_core::policy::{
    ClientApiKeyId, ClientPolicy, CodexClientMinVersions, CodexClientVersion,
    PlaintextClientApiKey, RateLimits,
};
use gateway_core::routing::{
    ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities, ProviderKind,
    ProviderModel, RuntimeAccount, RuntimeAccountDirectory, RuntimeSnapshot, UpstreamModelId,
};
use gateway_core::runtime::RuntimeSnapshotHandle;
use gateway_core::upstream::UpstreamSendState;

pub(super) async fn api_router(execution: Arc<dyn ExecutionService>) -> axum::Router {
    api_router_with_origins(execution, Vec::new()).await
}

pub(super) fn api_router_with_admin(admin: gateway_admin::AdminServices) -> axum::Router {
    api_router_with_config(
        admin,
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".to_owned(),
        },
    )
}

pub(super) fn api_router_with_admin_and_execution(
    admin: gateway_admin::AdminServices,
    execution: Arc<dyn ExecutionService>,
) -> axum::Router {
    api_router_with_config_and_execution(
        admin,
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".to_owned(),
        },
        execution,
    )
}

pub(super) fn api_router_with_admin_and_client(
    admin: gateway_admin::AdminServices,
    plaintext: &str,
    client_key_id: &str,
) -> axum::Router {
    let execution = Arc::new(DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot_with_client_key(plaintext, "openai", client_key_id)),
        Arc::new(UnusedExecutionStore),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        Arc::new(IgnoredClientApiKeyUsage),
    ));
    api_router_with_admin_and_execution(admin, execution)
}

pub(super) fn api_router_with_config(
    admin: gateway_admin::AdminServices,
    config: gateway_api::ApiConfig,
) -> axum::Router {
    api_bundle(admin, config).router()
}

pub(super) fn api_bundle(
    admin: gateway_admin::AdminServices,
    config: gateway_api::ApiConfig,
) -> gateway_api::ApiBundle {
    let execution = Arc::new(DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot("unused-client-route-key", "openai")),
        Arc::new(UnusedExecutionStore),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        Arc::new(IgnoredClientApiKeyUsage),
    ));
    gateway_api::initialize(
        config,
        execution,
        admin,
        Vec::new(),
        Arc::new(EmptyWorkerHealth),
        Arc::new(TestLifecycle::default()),
    )
    .unwrap()
}

fn api_router_with_config_and_execution(
    admin: gateway_admin::AdminServices,
    config: gateway_api::ApiConfig,
    execution: Arc<dyn ExecutionService>,
) -> axum::Router {
    gateway_api::initialize(
        config,
        execution,
        admin,
        Vec::new(),
        Arc::new(EmptyWorkerHealth),
        Arc::new(TestLifecycle::default()),
    )
    .expect("API bundle")
    .router()
}

pub(super) async fn api_router_with_worker_health(
    execution: Arc<dyn ExecutionService>,
    worker_health: Arc<dyn WorkerHealthSource>,
) -> axum::Router {
    api_router_with_origins_and_worker_health(execution, Vec::new(), worker_health).await
}

pub(super) async fn api_router_with_origins(
    execution: Arc<dyn ExecutionService>,
    cors_allowed_origins: Vec<String>,
) -> axum::Router {
    api_router_with_origins_and_worker_health(
        execution,
        cors_allowed_origins,
        Arc::new(EmptyWorkerHealth),
    )
    .await
}

async fn api_router_with_origins_and_worker_health(
    execution: Arc<dyn ExecutionService>,
    cors_allowed_origins: Vec<String>,
    worker_health: Arc<dyn WorkerHealthSource>,
) -> axum::Router {
    let admin = crate::admin::AdminTestFixture::new().await;
    gateway_api::initialize(
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins,
            request_timeout_seconds: None,
            request_id_header: "x-request-id".to_owned(),
        },
        execution,
        admin.services,
        Vec::new(),
        worker_health,
        Arc::new(TestLifecycle::default()),
    )
    .expect("API bundle")
    .router()
}

pub(super) fn authenticated_client(plaintext: &str) -> AuthenticatedClient {
    authenticated_client_for_provider(plaintext, "openai")
}

pub(super) fn authenticated_client_for_provider(
    plaintext: &str,
    provider_name: &str,
) -> AuthenticatedClient {
    authenticated_client_for_provider_with_limit(plaintext, provider_name, 64 * 1024 * 1024)
}

pub(super) fn authenticated_client_for_provider_with_limit(
    plaintext: &str,
    provider_name: &str,
    bytes: usize,
) -> AuthenticatedClient {
    let snapshot = snapshot(plaintext, provider_name);
    let settings = snapshot
        .settings()
        .clone()
        .with_responses_max_decompressed_body_bytes(bytes as u64);
    let snapshot = snapshot.with_settings(&settings).unwrap();
    let source = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot),
        Arc::new(UnusedExecutionStore),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        Arc::new(IgnoredClientApiKeyUsage),
    );
    source
        .authenticate(plaintext)
        .expect("authenticated client")
}

pub(super) fn authenticated_client_with_min_versions(
    plaintext: &str,
    desktop: Option<&str>,
    cli: Option<&str>,
) -> AuthenticatedClient {
    let snapshot = snapshot(plaintext, "openai");
    let settings =
        snapshot
            .settings()
            .clone()
            .with_min_codex_client_versions(CodexClientMinVersions::new(
                desktop.map(|version| {
                    CodexClientVersion::parse(version).expect("Desktop min version")
                }),
                cli.map(|version| CodexClientVersion::parse(version).expect("CLI min version")),
            ));
    let snapshot = snapshot.with_settings(&settings).unwrap();
    let source = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot),
        Arc::new(UnusedExecutionStore),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        Arc::new(IgnoredClientApiKeyUsage),
    );
    source
        .authenticate(plaintext)
        .expect("authenticated client")
}

struct IgnoredClientApiKeyUsage;

impl ClientApiKeyUsageSink for IgnoredClientApiKeyUsage {
    fn record_used(&self, _: &ClientApiKeyId) {}
}

fn snapshot(plaintext: &str, provider_name: &str) -> RuntimeSnapshot {
    snapshot_with_client_key(plaintext, provider_name, "key_api_test")
}

fn snapshot_with_client_key(
    plaintext: &str,
    provider_name: &str,
    client_key_id: &str,
) -> RuntimeSnapshot {
    let provider = ProviderKind::new(provider_name).expect("provider");
    let account_directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
        ProviderAccountId::new("acct_api_test").expect("account ID"),
        RuntimeAccount::new(provider.clone(), BTreeSet::new()),
    )])));
    let capabilities = ModelCapabilities::new(
        BTreeSet::from([gateway_core::operation::OperationKind::Generate]),
        Some(16_000),
    );
    RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        gateway_core::settings::SettingsValues::new(2, 1, "smart", Default::default(), None, None),
        vec![provider.clone()],
        ["model-a", "model-b"]
            .into_iter()
            .map(|model| {
                ProviderModel::new(
                    provider.clone(),
                    UpstreamModelId::new(model).expect("model"),
                    capabilities.clone(),
                )
            })
            .collect(),
        vec![ClientPolicy::new(
            ClientApiKeyId::new(client_key_id).expect("key ID"),
            PlaintextClientApiKey::new(plaintext).expect("plaintext key"),
            Arc::new(FrozenAccountScope::new(
                Arc::clone(&account_directory),
                ClientRoutingScope::all_accounts(),
            )),
            true,
            RateLimits::unlimited(),
        )],
    )
    .expect("runtime snapshot")
    .with_account_directory(account_directory)
}

#[derive(Default)]
struct TestLifecycle {
    cancellation: gateway_core::lifecycle::CancellationToken,
}

struct TestConnectionGuard;

impl ConnectionGuard for TestConnectionGuard {}

impl ConnectionLifecycle for TestLifecycle {
    fn try_register(&self) -> Result<Box<dyn ConnectionGuard>, ConnectionDraining> {
        Ok(Box::new(TestConnectionGuard))
    }

    fn cancellation(&self) -> gateway_core::lifecycle::CancellationToken {
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

struct UnusedContinuation;

impl NativeContinuationPort for UnusedContinuation {
    fn resolve<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a PreviousResponseId,
    ) -> BoxFuture<'a, Result<Option<NativeContinuationPin>, NativeContinuationStoreError>> {
        Box::pin(async { unreachable!("authentication fixture does not execute") })
    }

    fn record<'a>(
        &'a self,
        _: NativeContinuationPin,
    ) -> BoxFuture<'a, Result<(), NativeContinuationStoreError>> {
        Box::pin(async { unreachable!("authentication fixture does not execute") })
    }
}
