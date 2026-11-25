//! 验证 xAI 选号错误在 Core 的原始错误保留、候选切换与终止边界

use super::*;
use async_trait::async_trait;
use gateway_core::engine::provider::{ProviderCallMetadata, ProviderRegistry, ProviderStream};
use gateway_core::engine::{
    AttemptCoordinator, AttemptRecord, EngineError, ExecutionStore, GatewayEngine,
    IntermediateFailure, ModelRequestFinalization, NewModelRequest, RecoveryReport,
};
use gateway_core::error::StoreError;
use gateway_core::event::{ProviderEvent, ResponseMeta};
use gateway_core::routing::AccountRoutingSnapshot;
use gateway_core::upstream::UpstreamTransport;

struct NoopExecutionStore;

#[async_trait]
impl ExecutionStore for NoopExecutionStore {
    async fn create_model_request(&self, _: NewModelRequest) -> Result<(), StoreError> {
        Ok(())
    }

    async fn record_attempt(&self, _: AttemptRecord) -> Result<(), StoreError> {
        Ok(())
    }

    async fn mark_send_state(
        &self,
        _: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    async fn mark_downstream_committed(
        &self,
        _: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    async fn record_client_status(&self, _: &ModelRequestId, _: u16) -> Result<(), StoreError> {
        Ok(())
    }

    async fn record_intermediate_failure(&self, _: IntermediateFailure) -> Result<(), StoreError> {
        Ok(())
    }

    async fn finalize_model_request(&self, _: ModelRequestFinalization) -> Result<(), StoreError> {
        Ok(())
    }

    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        Ok(RecoveryReport::default())
    }
}

fn routing_plan(operation: &Operation, backup: bool) -> gateway_core::routing::RoutingPlan {
    let names = if backup {
        vec!["xai", "zzfallback"]
    } else {
        vec!["xai"]
    };
    let kinds: Vec<_> = names
        .iter()
        .map(|name| ProviderKind::new(*name).unwrap())
        .collect();
    let entries = kinds
        .iter()
        .map(|kind| {
            (
                account_id(if kind.as_str() == "xai" {
                    "provider"
                } else {
                    "backup"
                }),
                RuntimeAccount::new(kind.clone(), BTreeSet::new()),
            )
        })
        .collect();
    let directory = Arc::new(RuntimeAccountDirectory::new(entries));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(3, "smart", Default::default(), None, None),
        kinds.clone(),
        kinds
            .iter()
            .map(|kind| {
                ProviderModel::new(
                    kind.clone(),
                    UpstreamModelId::new(MODEL).unwrap(),
                    ModelCapabilities::new(BTreeSet::from([operation.kind()]), None),
                )
            })
            .collect(),
        Vec::new(),
    )
    .unwrap()
    .with_account_directory(directory.clone());
    let plan = snapshot
        .plan(
            &PublicModelId::new(MODEL).unwrap(),
            operation,
            Arc::new(FrozenAccountScope::new(
                directory,
                ClientRoutingScope::all_accounts(),
            )),
            &RoutingContext::default(),
        )
        .unwrap();
    assert_eq!(plan.candidates()[0].provider().as_str(), "xai");
    plan
}

fn model_request(operation: &Operation) -> NewModelRequest {
    NewModelRequest {
        admission_decision_ms: None,
        id: ModelRequestId::new("req_xai_selection_retry").unwrap(),
        client_api_key_name_snapshot: None,
        client_api_key_ref: ClientApiKeyId::new("key_xai_contract").unwrap(),
        config_revision: ConfigRevision::new(1).unwrap(),
        routing: AccountRoutingSnapshot::all(),
        protocol: "openai".to_owned(),
        operation: operation.kind(),
        endpoint: "responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model: Some(PublicModelId::new(MODEL).unwrap()),
        client_ip: None,
        user_agent: None,
        reasoning_effort: None,
        reasoning_preset: None,
        request_kind: None,
        subagent_kind: None,
        compact: false,
        continuation: Default::default(),
        image_generation_requested: false,
        started_at: SystemTime::now(),
        deadline_at: (SystemTime::now() + Duration::from_secs(10)).into(),
    }
}

#[tokio::test]
async fn cooldown_after_rotation_preserves_upstream_rejection() {
    let store = MemoryProviderAccountStore::shared();
    let repository = GrokCredentialRepository::new(store.clone());
    let input = create_input("provider", "provider");
    seed_input(&store, &input).await.unwrap();
    repository
        .update_state(&UpdateGrokCredentialState {
            account_id: input.account_id.clone(),
            expected_revision: CredentialRevision::new(1).unwrap(),
            credential_state: CredentialState::Ready,
            error_reason: None,
            error_message: None,
            observed_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    let cache = MemoryGrokCatalogCache::shared();
    let selector = GrokAccountSessionSelector::new(
        ProviderKind::new("xai").unwrap(),
        repository.clone(),
        cache.clone(),
        Arc::new(crate::support::grok_quota_service(
            repository.clone(),
            Arc::new(UnavailableBillingTransport),
        )),
        Arc::new(DiagnosticLeasePort),
        Arc::new(MemoryCooldownPort::default()),
        Arc::new(AccountFeedbackStats::default()),
        Arc::new(crate::support::TestSessionAffinity::default()),
    );
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::RateLimited,
            UpstreamSendState::Sent,
        )
        .with_status(429)
        .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
            "original upstream rate limit",
            Some("rate_limit_exceeded".into()),
            Some("rate_limit_error".into()),
        )),
    );
    let provider = Arc::new(
        GrokBuildProvider::new(
            Arc::new(selector),
            transport.clone(),
            Arc::new(crate::support::grok_catalog_service(
                repository,
                Arc::new(StaticCatalogTransport),
                cache,
            )),
            StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
            Arc::new(AccountFeedbackStats::default()),
            crate::support::xai_wire_profile(),
        )
        .unwrap(),
    );
    let registry = ProviderRegistry::new([provider as Arc<dyn Provider>]).unwrap();
    let coordinator =
        AttemptCoordinator::new(GatewayEngine::new(Arc::new(NoopExecutionStore), registry));
    let operation = operation();
    let plan = routing_plan(&operation, false);
    let mut session = coordinator
        .start(
            model_request(&operation),
            operation,
            plan,
            None,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let error = session.collect_uncommitted().await.unwrap_err();
    let EngineError::Provider(error) = error else {
        panic!("expected provider error: {error:?}")
    };
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(error.upstream_status(), Some(429));
    assert_eq!(
        error.client_visible_upstream_error().unwrap().message(),
        "original upstream rate limit"
    );
}

struct FallbackProvider(AtomicUsize);
#[async_trait]
impl Provider for FallbackProvider {
    fn name(&self) -> &'static str {
        "zzfallback"
    }
    fn catalog_generation(&self) -> gateway_core::routing::ProviderCatalogGeneration {
        Default::default()
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<gateway_core::routing::ProviderModelCapabilities>, ProviderError> {
        Ok(Vec::new())
    }

    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let candidate = request.candidate();
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            candidate.upstream_model().unwrap().clone(),
            account_id("backup"),
            UpstreamTransport::new("http_sse").unwrap(),
        );
        let events = [
            GatewayEvent::Started(ResponseMeta::new("fallback-response", MODEL)),
            GatewayEvent::Completed(ResponseMeta::new("fallback-response", MODEL)),
        ];
        Ok(ProviderStream::new(
            metadata,
            Box::pin(stream::iter(
                events
                    .into_iter()
                    .map(|event| Ok(ProviderEvent::canonical(event))),
            )),
            (),
        ))
    }
}

#[tokio::test]
async fn unavailable_selection_advances_to_backup_without_sending_upstream() {
    for failure in [
        GrokSessionSelectorError::NoEligibleSession,
        GrokSessionSelectorError::CapacityUnavailable { retry_after: None },
        GrokSessionSelectorError::AccountCoolingDown { retry_after: None },
        GrokSessionSelectorError::ModelCoolingDown { retry_after: None },
        GrokSessionSelectorError::Unavailable(None),
    ] {
        let transport = StubInferenceTransport::success();
        let provider = provider(StubSelector::failing(failure), transport.clone()).await;
        let backup = Arc::new(FallbackProvider(AtomicUsize::new(0)));
        let registry = ProviderRegistry::new([
            provider as Arc<dyn Provider>,
            backup.clone() as Arc<dyn Provider>,
        ])
        .unwrap();
        let coordinator =
            AttemptCoordinator::new(GatewayEngine::new(Arc::new(NoopExecutionStore), registry));
        let operation = operation();
        let plan = routing_plan(&operation, true);
        assert_eq!(plan.candidates().len(), 2);
        let mut session = coordinator
            .start(
                model_request(&operation),
                operation,
                plan,
                None,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let result = session.collect_uncommitted().await;
        let events = result.expect("backup completes the request");
        assert!(events.iter().flat_map(ProviderEvent::canonical_facts).any(|event| matches!(event, GatewayEvent::Completed(meta) if meta.response_id() == "fallback-response")));
        session.commit_downstream(Some(200)).await.unwrap();
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        assert_eq!(backup.0.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn policy_failures_do_not_advance_to_backup() {
    for (failure, expected) in [
        (
            GrokSessionSelectorError::PolicyRejected,
            ProviderErrorKind::RequestPolicyDenied,
        ),
        (
            GrokSessionSelectorError::PolicyFiltered,
            ProviderErrorKind::RequestPolicyDenied,
        ),
        (
            GrokSessionSelectorError::PolicyUnavailable,
            ProviderErrorKind::Unavailable,
        ),
    ] {
        let transport = StubInferenceTransport::success();
        let provider = provider(StubSelector::failing(failure), transport.clone()).await;
        let backup = Arc::new(FallbackProvider(AtomicUsize::new(0)));
        let registry = ProviderRegistry::new([
            provider as Arc<dyn Provider>,
            backup.clone() as Arc<dyn Provider>,
        ])
        .unwrap();
        let coordinator =
            AttemptCoordinator::new(GatewayEngine::new(Arc::new(NoopExecutionStore), registry));
        let operation = operation();
        let plan = routing_plan(&operation, true);
        let mut session = coordinator
            .start(
                model_request(&operation),
                operation,
                plan,
                None,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let EngineError::Provider(error) = session.collect_uncommitted().await.unwrap_err() else {
            panic!("expected policy failure");
        };
        assert_eq!(error.kind(), expected);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        assert_eq!(backup.0.load(Ordering::SeqCst), 0);
    }
}
