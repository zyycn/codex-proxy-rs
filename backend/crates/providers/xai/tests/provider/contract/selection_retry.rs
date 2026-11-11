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

struct ProxyBoundSelector(gateway_core::account::OutboundProxy);

impl GrokSessionSelector for ProxyBoundSelector {
    fn select(&self, request: GrokSessionSelection) -> GrokSessionSelectorFuture<'_> {
        Box::pin(async move {
            let id = account_id("provider");
            if request.excluded_accounts().contains(&id)
                || request
                    .required_account()
                    .is_some_and(|required| required != &id)
            {
                return Err(GrokSessionSelectorError::NoEligibleSession);
            }
            SelectedGrokSession::new(
                id,
                CredentialRevision::new(1).unwrap(),
                SecretValue::new("oauth-access"),
                SecretValue::new("verified-user"),
                None,
                GrokSessionBinding::new("acct_provider")
                    .unwrap()
                    .with_outbound_proxy(Some(self.0.clone())),
                (),
            )
            .map_err(|_| GrokSessionSelectorError::InvalidSession(None))
        })
    }

    fn record_failure<'a>(
        &'a self,
        _: &'a SelectedGrokSession,
        _: GrokCredentialFailure,
    ) -> GrokCredentialFeedbackFuture<'a> {
        Box::pin(async {})
    }
}

// 测试仅替换上游地址，真实 Reqwest 仍按 Provider 给出的出口绑定建连
struct LoopbackInferenceTransport {
    inner: provider_xai::ReqwestGrokInferenceTransport,
    endpoint: url::Url,
}

impl GrokInferenceTransport for LoopbackInferenceTransport {
    fn execute(&self, request: GrokInferenceRequest) -> GrokInferenceTransportFuture<'_> {
        assert_eq!(
            request
                .headers()
                .iter()
                .find(|header| header.name() == "authorization")
                .unwrap()
                .value()
                .as_ref(),
            b"Bearer oauth-access"
        );
        let session = SelectedGrokSession::new(
            account_id("provider"),
            CredentialRevision::new(1).unwrap(),
            SecretValue::new("oauth-access"),
            SecretValue::new("verified-user"),
            None,
            request.binding().clone(),
            (),
        )
        .unwrap();
        let headers = provider_xai::build_grok_headers(
            &crate::support::xai_wire_profile(),
            &session,
            &provider_xai::GrokClientIdentity::new(),
            &ModelRequestId::new("req_local_forward").unwrap(),
            None,
            None,
            &UpstreamModelId::new(MODEL).unwrap(),
        );
        self.inner.execute(
            GrokInferenceRequest::new(
                self.endpoint.clone(),
                headers,
                request.body().to_vec(),
                request.binding().clone(),
            )
            .with_connect_timeout(request.connect_timeout()),
        )
    }
}

#[tokio::test]
async fn proxy_direct_fallback_reaches_xai_through_core_without_changing_account_binding() {
    use gateway_core::account::OutboundProxy;
    use tokio::io::AsyncReadExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    for allow in [false, true] {
        let origin = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(SUCCESS_SSE, "text/event-stream"))
            .mount(&origin)
            .await;
        let origin_url = url::Url::parse(&origin.uri()).unwrap();
        let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = OutboundProxy::parse(&format!(
            "socks5h://{}",
            proxy_listener.local_addr().unwrap()
        ))
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let proxy_server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = proxy_listener.accept().await.unwrap();
                observed.fetch_add(1, Ordering::SeqCst);
                let mut greeting = [0; 2];
                let _ = socket.read_exact(&mut greeting).await;
            }
        });
        let selector = Arc::new(ProxyBoundSelector(proxy));
        let transport = Arc::new(LoopbackInferenceTransport {
            inner: provider_xai::ReqwestGrokInferenceTransport::new(
                crate::support::loopback_endpoint_policy(&origin_url),
            )
            .unwrap(),
            endpoint: origin_url.join("v1/responses").unwrap(),
        });
        let provider = provider_with_catalog_transport(
            selector,
            transport,
            StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
            Arc::new(StaticCatalogTransport),
        )
        .await;
        let registry = ProviderRegistry::new([provider as Arc<dyn Provider>]).unwrap();
        let coordinator =
            AttemptCoordinator::new(GatewayEngine::new(Arc::new(NoopExecutionStore), registry));
        for _ in 0..if allow { 2 } else { 1 } {
            let operation = operation();
            let plan = routing_plan_with_proxy_fallback(&operation, false, allow);
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
            assert_eq!(result.is_ok(), allow, "allow={allow}, result={result:?}");
            if allow {
                session.commit_downstream(Some(200)).await.unwrap();
            }
        }
        let requests = origin.received_requests().await.unwrap();
        assert_eq!(requests.len(), if allow { 2 } else { 0 });
        assert!(calls.load(Ordering::SeqCst) >= if allow { 2 } else { 1 });
        for request in requests {
            assert_eq!(request.headers["authorization"], "Bearer oauth-access");
        }
        proxy_server.abort();
    }
}

#[tokio::test]
async fn internal_xai_payload_retry_never_requests_direct_fallback() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unreachable = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let cause = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(unreachable)
        .send()
        .await
        .unwrap_err();
    assert!(cause.is_connect());
    let transport = StubInferenceTransport::sequence([
        InferenceMode::Error(
            GrokInferenceTransportError::new(
                GrokInferenceTransportErrorKind::InvalidRequest,
                UpstreamSendState::Sent,
            )
            .with_status(400)
            .with_upstream_code(OpaqueUpstreamValue::new("reasoning_decode_failed")),
        ),
        InferenceMode::Error(
            GrokInferenceTransportError::new(
                GrokInferenceTransportErrorKind::Transport,
                UpstreamSendState::NotSent,
            )
            .with_source(cause),
        ),
    ]);
    let selector = Arc::new(ProxyBoundSelector(
        gateway_core::account::OutboundProxy::parse("socks5h://127.0.0.1:18080").unwrap(),
    ));
    let provider = provider_with_catalog_transport(
        selector,
        transport.clone(),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(StaticCatalogTransport),
    )
    .await;
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", json!({
            "model":"client-model", "input":[
                {"type":"reasoning","id":"reason_stale","summary":[],"content":null,"encrypted_content":"stale-ciphertext"},
                {"type":"message","role":"user","content":"continue"}
            ]
        }).as_object().unwrap().clone()).unwrap()
    ));
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", operation),
            context_with_proxy_fallback(CancellationToken::new(), None, None, false, true),
        )
        .await
        .unwrap();
    let error = next_provider_error(&mut stream).await;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(!matches!(
        error.pre_delivery_retry(),
        Some(gateway_core::error::PreDeliveryRetry::SameAccountDirectFallback { .. })
    ));
}

#[tokio::test]
async fn direct_fallback_xai_respects_exhausted_opening_budget() {
    let transport = StubInferenceTransport::success();
    let selector = Arc::new(ProxyBoundSelector(
        gateway_core::account::OutboundProxy::parse("socks5h://127.0.0.1:18080").unwrap(),
    ));
    let provider = provider_with_catalog_transport(
        selector,
        transport.clone(),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(StaticCatalogTransport),
    )
    .await;
    let context = context_with_proxy_fallback(CancellationToken::new(), None, None, false, true);
    for _ in 0..4 {
        context.connection_budget().begin().unwrap();
    }
    let mut stream = provider
        .execute(provider_request("xai"), context)
        .await
        .unwrap();
    let error = next_provider_error(&mut stream).await;
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert_eq!(error.kind(), ProviderErrorKind::Timeout);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn xai_tls_failure_never_requests_direct_fallback() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("https://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut hello = [0; 5];
        socket.read_exact(&mut hello).await.unwrap();
        let mut record = vec![0; usize::from(u16::from_be_bytes([hello[3], hello[4]]))];
        socket.read_exact(&mut record).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
        // 读完 ClientHello 后再半关闭，避免未读数据触发 TCP RST 掩盖真实 TLS 协议错误
        let _ = socket.read_to_end(&mut Vec::new()).await;
    });
    let cause = reqwest::Client::builder()
        .use_rustls_tls()
        .no_proxy()
        .build()
        .unwrap()
        .post(endpoint)
        .send()
        .await
        .unwrap_err();
    assert!(cause.is_connect());
    let mut source = Some(&cause as &(dyn std::error::Error + 'static));
    let mut tls_error = false;
    while let Some(error) = source {
        tls_error |= error.is::<rustls::Error>();
        source = error
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            .map(|inner| inner as &(dyn std::error::Error + 'static))
            .or_else(|| error.source());
    }
    assert!(tls_error, "fixture must fail TLS validation: {cause:?}");
    server.await.unwrap();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::Transport,
            UpstreamSendState::NotSent,
        )
        .with_source(cause),
    );
    let selector = Arc::new(ProxyBoundSelector(
        gateway_core::account::OutboundProxy::parse("socks5h://127.0.0.1:18080").unwrap(),
    ));
    let provider = provider_with_catalog_transport(
        selector,
        transport.clone(),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(StaticCatalogTransport),
    )
    .await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context_with_proxy_fallback(CancellationToken::new(), None, None, false, true),
        )
        .await
        .unwrap();
    let error = next_provider_error(&mut stream).await;
    assert!(!matches!(
        error.pre_delivery_retry(),
        Some(gateway_core::error::PreDeliveryRetry::SameAccountDirectFallback { .. })
    ));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

fn routing_plan(operation: &Operation, backup: bool) -> gateway_core::routing::RoutingPlan {
    routing_plan_with_proxy_fallback(operation, backup, false)
}

fn routing_plan_with_proxy_fallback(
    operation: &Operation,
    backup: bool,
    enabled: bool,
) -> gateway_core::routing::RoutingPlan {
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
        gateway_core::settings::SettingsValues::new(3, "smart", Default::default(), None, None)
            .with_proxy_direct_fallback_enabled(enabled),
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
