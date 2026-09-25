use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use gateway_core::engine::budget::{ClientBudgetCharge, ClientBudgetError, ClientBudgetPort};
use gateway_core::error::GatewayError;

#[derive(Default)]
struct Admissions {
    active: Arc<AtomicBool>,
    limits: Mutex<Vec<RateLimits>>,
    release_gate: Mutex<Option<oneshot::Receiver<()>>>,
    releases: AtomicUsize,
}

impl ClientAdmissionPort for Admissions {
    fn abandon(
        &self,
        key: &gateway_core::policy::ClientApiKeyId,
        request: &gateway_core::engine::ModelRequestId,
    ) {
        let _ = futures::FutureExt::now_or_never(self.release(key, request));
    }

    fn admit(
        &self,
        request: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>> {
        Box::pin(async move {
            self.limits.lock().unwrap().push(request.limits);
            assert!(!self.active.swap(true, Ordering::SeqCst));
            Ok(ClientAdmissionDecision::Granted)
        })
    }
    fn release<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>> {
        Box::pin(async {
            self.releases.fetch_add(1, Ordering::SeqCst);
            let gate = self.release_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.await.expect("release gate");
            }
            Ok(self.active.swap(false, Ordering::SeqCst))
        })
    }
    fn restore(
        &self,
        _: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        Box::pin(async { Ok(ClientAdmissionRestoreResult::default()) })
    }
}

#[derive(Default)]
struct Budget {
    reject: bool,
    active: Arc<AtomicBool>,
    charges: Mutex<Vec<ClientBudgetCharge>>,
    settlement_gate: Mutex<Option<oneshot::Receiver<()>>>,
    settlements: AtomicUsize,
    fail_settlement: bool,
}

impl ClientBudgetPort for Budget {
    fn admit(&self, _: ClientApiKeyId) -> BoxFuture<'_, Result<(), GatewayError>> {
        Box::pin(async {
            assert!(self.active.load(Ordering::SeqCst));
            if self.reject {
                Err(
                    GatewayError::new(GatewayErrorKind::RateLimited, "budget exhausted")
                        .with_client_code("key_daily_budget_exceeded"),
                )
            } else {
                Ok(())
            }
        })
    }
    fn settle(&self, charge: ClientBudgetCharge) -> BoxFuture<'_, Result<(), ClientBudgetError>> {
        Box::pin(async move {
            self.settlements.fetch_add(1, Ordering::SeqCst);
            assert!(
                self.active.load(Ordering::SeqCst),
                "settle before releasing the concurrent request slot"
            );
            let gate = self.settlement_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.await.expect("settlement gate");
            }
            assert!(self.active.load(Ordering::SeqCst));
            self.charges.lock().unwrap().push(charge);
            if self.fail_settlement {
                Err(ClientBudgetError)
            } else {
                Ok(())
            }
        })
    }
}

fn service(admissions: Arc<Admissions>, budget: Arc<Budget>) -> DefaultExecutionService {
    DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        admissions,
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    )
    .with_budget(budget)
}

fn request(service: &DefaultExecutionService, transport: ClientTransport) -> StartExecution {
    StartExecution {
        client: service.authenticate("sk_start_test").unwrap(),
        public_model: PublicModelId::new("gpt-start").unwrap(),
        operation: start_operation(),
        metadata: ExecutionRequestMetadata {
            protocol: "openai".to_owned(),
            endpoint: "/v1/responses".to_owned(),
            transport,
            stream: transport != ClientTransport::HttpJson,
            client_ip: None,
            user_agent: None,
            previous_response_id: None,
        },
    }
}

#[test]
fn budget_rejection_releases_client_concurrency_without_creating_a_charge() {
    let admissions = Arc::new(Admissions::default());
    let budget = Arc::new(Budget {
        reject: true,
        active: admissions.active.clone(),
        charges: Mutex::default(),
        ..Default::default()
    });
    let service = service(admissions.clone(), budget.clone());
    for transport in [
        ClientTransport::HttpJson,
        ClientTransport::HttpSse,
        ClientTransport::WebSocket,
    ] {
        let result = block_on(service.start(request(&service, transport)));
        assert!(
            matches!(result, Err(error) if error.client_error_code() == Some("key_daily_budget_exceeded"))
        );
        assert!(!admissions.active.load(Ordering::SeqCst));
        assert!(budget.charges.lock().unwrap().is_empty());
    }
}

#[test]
fn plugin_page_key_selection_uses_normal_root_admission_and_budget() {
    block_on(async {
        let admissions = Arc::new(Admissions::default());
        let budget = Arc::new(Budget {
            reject: true,
            active: admissions.active.clone(),
            ..Default::default()
        });
        let service = service(admissions.clone(), budget.clone());
        let key = ClientApiKeyId::new("key_start_test").unwrap();
        let prepared = service.prepare_plugin_execution(&key).await.unwrap();
        assert_eq!(prepared.client().policy().key_id(), &key);
        let request = request(&service, ClientTransport::HttpSse);
        let result = service
            .start_prepared(
                prepared,
                gateway_core::engine::execution::PreparedExecutionRequest {
                    public_model: request.public_model,
                    operation: request.operation,
                    metadata: request.metadata,
                },
            )
            .await;
        assert!(
            matches!(result, Err(error) if error.client_error_code() == Some("key_daily_budget_exceeded"))
        );
        assert!(!admissions.active.load(Ordering::SeqCst));
        assert!(budget.charges.lock().unwrap().is_empty());
        assert!(
            matches!(service.prepare_plugin_execution(&ClientApiKeyId::new("key_missing").unwrap()).await, Err(error) if error.kind() == GatewayErrorKind::Unauthorized)
        );
    });
}

#[test]
fn provider_http_endpoint_uses_the_same_admission_and_budget_gate() {
    let admissions = Arc::new(Admissions::default());
    let budget = Arc::new(Budget {
        reject: true,
        active: admissions.active.clone(),
        charges: Mutex::default(),
        ..Default::default()
    });
    let service = service(admissions.clone(), budget.clone());
    let operation = Operation::ProviderHttp(
        ProviderHttpRequest::new(
            "models",
            ProviderHttpMethod::Get,
            None,
            Vec::new(),
            RawHttpPayload::new("provider-http", Bytes::new()).expect("HTTP payload"),
        )
        .expect("provider HTTP operation"),
    );
    let result = block_on(service.start_provider_endpoint(StartProviderExecution {
        client: service.authenticate("sk_start_test").expect("client"),
        provider: ProviderKind::new("openai").expect("provider"),
        upstream_model: None,
        operation,
        metadata: ExecutionRequestMetadata {
            protocol: "provider-http".to_owned(),
            endpoint: "/v1/providers/openai/http/models".to_owned(),
            transport: ClientTransport::HttpJson,
            stream: false,
            client_ip: None,
            user_agent: None,
            previous_response_id: None,
        },
    }));

    assert!(
        matches!(result, Err(error) if error.client_error_code() == Some("key_daily_budget_exceeded"))
    );
    assert!(!admissions.active.load(Ordering::SeqCst));
    assert!(budget.charges.lock().unwrap().is_empty());
}

#[test]
fn bound_token_count_reports_an_explicit_unsupported_capability() {
    let admissions = Arc::new(Admissions::default());
    let budget = Arc::new(Budget {
        active: admissions.active.clone(),
        ..Budget::default()
    });
    let service = service(admissions.clone(), budget.clone());
    let operation = Operation::CountTokens(TokenCountRequest::from_raw_json(
        RawJsonPayload::new("token-count", Bytes::from_static(br#"{"input":"hello"}"#))
            .expect("token count payload"),
    ));

    let result = block_on(service.start_provider_endpoint(StartProviderExecution {
        client: service.authenticate("sk_start_test").expect("client"),
        provider: ProviderKind::new("openai").expect("provider"),
        upstream_model: Some(UpstreamModelId::new("gpt-start").expect("model")),
        operation,
        metadata: ExecutionRequestMetadata {
            protocol: "token-count".to_owned(),
            endpoint: "/v1/providers/openai/models/gpt-start/count_tokens".to_owned(),
            transport: ClientTransport::HttpJson,
            stream: false,
            client_ip: None,
            user_agent: None,
            previous_response_id: None,
        },
    }));

    assert!(matches!(result, Err(error) if error.kind() == GatewayErrorKind::Unsupported));
    assert!(!admissions.active.load(Ordering::SeqCst));
    assert!(budget.charges.lock().unwrap().is_empty());
}

#[test]
fn reused_client_uses_updated_limits_for_each_execution() {
    let snapshots = RuntimeSnapshotHandle::new(start_snapshot());
    let admissions = Arc::new(Admissions::default());
    let service = DefaultExecutionService::new(
        snapshots.clone(),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        admissions.clone(),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    let client = service.authenticate("sk_start_test").unwrap();
    let limited = RateLimits {
        max_concurrency: 1,
        requests_per_minute: 1,
    };
    for (revision, limits) in [(2, limited), (3, RateLimits::unlimited())] {
        snapshots.publish(start_snapshot_with_policy(revision, true, limits, false));
        let mut next = request(&service, ClientTransport::WebSocket);
        next.client = client.clone();
        let started = block_on(service.start(next)).expect("new execution");
        block_on(started.session.detach_finalize());
    }
    assert_eq!(
        *admissions.limits.lock().unwrap(),
        vec![limited, RateLimits::unlimited()]
    );
}

#[test]
fn reused_client_cannot_start_after_key_disable_or_snapshot_suspension() {
    for suspend in [false, true] {
        let snapshots = RuntimeSnapshotHandle::new(start_snapshot());
        let admissions = Arc::new(Admissions::default());
        let service = DefaultExecutionService::new(
            snapshots.clone(),
            Arc::new(TrackingExecutionStore::default()),
            ProviderRegistry::default(),
            admissions.clone(),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        );
        let next = request(&service, ClientTransport::WebSocket);
        if suspend {
            snapshots.suspend();
        } else {
            snapshots.publish(start_snapshot_with_policy(
                2,
                false,
                RateLimits::unlimited(),
                false,
            ));
        }
        let result = block_on(service.start(next));
        let expected = if suspend {
            GatewayErrorKind::Internal
        } else {
            GatewayErrorKind::Unauthorized
        };
        assert!(matches!(result, Err(error) if error.kind() == expected));
        assert!(admissions.limits.lock().unwrap().is_empty());
    }
}

struct FrontendAuthenticationFixture {
    decisions: Mutex<VecDeque<FrontendAuthenticationDecision>>,
    identities: BTreeMap<String, ClientApiKeyId>,
    exclusive: bool,
    calls: AtomicUsize,
}

impl FrontendAuthenticationPlan for FrontendAuthenticationFixture {
    fn authenticate<'a>(
        &'a self,
        _: &'a ClientAuthenticationRequest,
    ) -> BoxFuture<'a, Result<FrontendAuthenticationDecision, FrontendAuthenticationError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.decisions
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(FrontendAuthenticationError)
        })
    }

    fn client_key_id(&self, principal: &str) -> Option<ClientApiKeyId> {
        self.identities.get(principal).cloned()
    }

    fn exclusive(&self) -> bool {
        self.exclusive
    }
}

struct AuthenticationGeneration(Arc<dyn FrontendAuthenticationPlan>);

impl ExtensionSetLease for AuthenticationGeneration {
    fn is_ready(&self) -> bool {
        Arc::strong_count(&self.0) > 0
    }
}

fn authentication_generation(
    index: &FrontendAuthenticationExtensionIndex,
    id: &str,
    plan: Arc<dyn FrontendAuthenticationPlan>,
) -> ExtensionSetReference {
    let id = ExtensionSetId::new(id.to_owned()).unwrap();
    let plan = index.register(id.clone(), plan).unwrap();
    ExtensionSetReference::new(id, Arc::new(AuthenticationGeneration(plan)))
}

#[test]
fn reused_client_is_reauthenticated_by_the_current_frontend_plan_without_identity_drift() {
    block_on(async {
        let authentication = FrontendAuthenticationExtensionIndex::default();
        let initial_plan = Arc::new(FrontendAuthenticationFixture {
            decisions: Mutex::new(VecDeque::from([
                FrontendAuthenticationDecision::Authenticated {
                    principal: "external-user".into(),
                },
            ])),
            identities: BTreeMap::from([(
                "external-user".into(),
                ClientApiKeyId::new("key_start_test").unwrap(),
            )]),
            exclusive: true,
            calls: AtomicUsize::new(0),
        });
        let initial = start_snapshot().with_extensions(Some(authentication_generation(
            &authentication,
            "authentication-first",
            initial_plan.clone(),
        )));
        let snapshots = RuntimeSnapshotHandle::new(initial);
        let admissions = Arc::new(Admissions::default());
        let service = DefaultExecutionService::new(
            snapshots.clone(),
            Arc::new(TrackingExecutionStore::default()),
            ProviderRegistry::default(),
            admissions.clone(),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        )
        .with_frontend_authentication(authentication.clone());
        let client = service
            .authenticate_request(
                ClientAuthenticationRequest::new("External controlled-fixture").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(initial_plan.calls.load(Ordering::SeqCst), 1);

        let replacement_plan = Arc::new(FrontendAuthenticationFixture {
            decisions: Mutex::new(VecDeque::from([
                FrontendAuthenticationDecision::Authenticated {
                    principal: "external-user".into(),
                },
            ])),
            identities: BTreeMap::from([(
                "external-user".into(),
                ClientApiKeyId::new("another-key").unwrap(),
            )]),
            exclusive: true,
            calls: AtomicUsize::new(0),
        });
        snapshots.publish(
            start_snapshot().with_extensions(Some(authentication_generation(
                &authentication,
                "authentication-second",
                replacement_plan.clone(),
            ))),
        );
        let mut next = request(&service, ClientTransport::WebSocket);
        next.client = client;
        let result = service.start(next).await;
        assert!(matches!(result, Err(error) if error.kind() == GatewayErrorKind::Unauthorized));
        assert_eq!(replacement_plan.calls.load(Ordering::SeqCst), 1);
        assert!(admissions.limits.lock().unwrap().is_empty());
    });
}

#[test]
fn cancellation_and_pre_send_failure_settle_zero_and_release_concurrency_for_all_transports() {
    let admissions = Arc::new(Admissions::default());
    let budget = Arc::new(Budget {
        reject: false,
        active: admissions.active.clone(),
        charges: Mutex::default(),
        ..Default::default()
    });
    let service = service(admissions.clone(), budget.clone());
    for transport in [
        ClientTransport::HttpJson,
        ClientTransport::HttpSse,
        ClientTransport::WebSocket,
    ] {
        for detach in [true, false] {
            let mut started = block_on(service.start(request(&service, transport))).unwrap();
            assert!(admissions.active.load(Ordering::SeqCst));
            if detach {
                block_on(started.session.detach_finalize());
            } else {
                assert!(block_on(started.session.next_event()).is_err());
                block_on(started.session.detach_finalize());
            }
            assert!(!admissions.active.load(Ordering::SeqCst));
        }
    }
    let charges = budget.charges.lock().unwrap();
    assert_eq!(
        charges.len(),
        6,
        "one settlement per request, including detached finalized sessions"
    );
    assert!(
        charges
            .iter()
            .all(|charge| charge.amount_usd == gateway_core::metering::Decimal::ZERO)
    );
}

fn early_failure_service(
    store: Arc<TrackingExecutionStore>,
    admissions: Arc<Admissions>,
    budget: Arc<Budget>,
) -> DefaultExecutionService {
    DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        store,
        ProviderRegistry::new([Arc::new(LocalFailingProvider) as Arc<dyn Provider>]).unwrap(),
        admissions,
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    )
    .with_budget(budget)
}

fn assert_early_failure_recorded(
    store: &TrackingExecutionStore,
    request_id: &ModelRequestId,
    transport: ClientTransport,
) {
    assert_eq!(store.creates.load(Ordering::SeqCst), 1);
    assert_eq!(store.finalizes.load(Ordering::SeqCst), 1);
    assert!(store.attempts.lock().unwrap().is_empty());
    let requests = store.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.id, *request_id);
    assert_eq!(request.client_api_key_ref.as_str(), "key_start_test");
    assert_eq!(
        request.client_api_key_id.as_ref(),
        Some(&request.client_api_key_ref)
    );
    assert_eq!(request.protocol, "openai");
    assert_eq!(request.endpoint, "/v1/responses");
    assert_eq!(request.client_transport, transport.as_str());
    assert_eq!(request.operation, OperationKind::Generate);
    assert_eq!(
        request.requested_model.as_ref().map(PublicModelId::as_str),
        Some("gpt-start")
    );
    let finalizations = store.finalizations.lock().unwrap();
    assert_eq!(finalizations.len(), 1);
    let finalization = &finalizations[0];
    assert_eq!(finalization.request_id, *request_id);
    assert_eq!(finalization.outcome, ExecutionOutcome::Failed);
    assert_eq!(finalization.send_state, UpstreamSendState::NotSent);
    assert_eq!(finalization.attempt_count, 0);
    assert!(finalization.downstream_committed_at.is_none());
    assert!(finalization.upstream_request_id.is_none());
    assert!(finalization.upstream_response_id.is_none());
    assert!(finalization.upstream_status_code.is_none());
    assert!(finalization.upstream_transport.is_none());
    assert!(finalization.http_version.is_none());
    assert!(finalization.websocket_pool.is_none());
    assert!(finalization.provider_metadata_json.is_none());
    assert_eq!(
        finalization
            .error
            .as_ref()
            .expect("original failure")
            .kind(),
        GatewayErrorKind::Unsupported,
        "detached cancellation must not replace the original provider failure"
    );
    assert_eq!(finalization.usage, Default::default());
    assert!(finalization.cost.total().is_none());
}

fn assert_zero_cleanup_completed(
    admissions: &Admissions,
    budget: &Budget,
    store: &TrackingExecutionStore,
    request_id: &ModelRequestId,
) {
    assert!(!admissions.active.load(Ordering::SeqCst));
    assert_eq!(admissions.releases.load(Ordering::SeqCst), 1);
    assert_eq!(budget.settlements.load(Ordering::SeqCst), 1);
    let charges = budget.charges.lock().unwrap();
    assert_eq!(charges.len(), 1);
    assert_eq!(charges[0].request_id, *request_id);
    assert_eq!(charges[0].key_id.as_str(), "key_start_test");
    assert_eq!(charges[0].amount_usd, Decimal::ZERO);
    assert_eq!(
        charges[0].completed_at,
        store.finalizations.lock().unwrap()[0].completed_at,
        "settlement must retain the original failure completion time"
    );
}

#[test]
fn early_provider_failure_records_request_and_settles_zero_for_all_transports() {
    block_on(async {
        for transport in [
            ClientTransport::HttpJson,
            ClientTransport::HttpSse,
            ClientTransport::WebSocket,
        ] {
            let store = Arc::new(TrackingExecutionStore::default());
            let admissions = Arc::new(Admissions::default());
            let budget = Arc::new(Budget {
                active: admissions.active.clone(),
                ..Default::default()
            });
            let service = early_failure_service(store.clone(), admissions.clone(), budget.clone());
            let mut started = service.start(request(&service, transport)).await.unwrap();
            assert!(admissions.active.load(Ordering::SeqCst));
            assert_eq!(store.creates.load(Ordering::SeqCst), 0);
            let error = if transport == ClientTransport::HttpJson {
                started.session.collect_uncommitted().await.unwrap_err()
            } else {
                started.session.next_event().await.unwrap_err()
            };
            assert!(matches!(
                error,
                EngineError::Provider(error)
                    if error.kind() == ProviderErrorKind::Unsupported
                        && error.send_state() == UpstreamSendState::NotSent
            ));
            assert!(started.session.is_finalized());
            assert_early_failure_recorded(&store, &started.request_id, transport);
            assert_zero_cleanup_completed(&admissions, &budget, &store, &started.request_id);
            assert!(started.session.next_event().await.unwrap().is_none());
            started.session.detach_finalize().await;
            assert_early_failure_recorded(&store, &started.request_id, transport);
            assert_zero_cleanup_completed(&admissions, &budget, &store, &started.request_id);
        }
    });
}

#[test]
fn detached_early_failure_resumes_cancelled_store_write_and_settles_once_for_all_transports() {
    block_on(async {
        for transport in [
            ClientTransport::HttpJson,
            ClientTransport::HttpSse,
            ClientTransport::WebSocket,
        ] {
            for suspend_create in [true, false] {
                let (complete_write, write_gate) = oneshot::channel();
                let store = Arc::new(TrackingExecutionStore::default());
                if suspend_create {
                    *store.create_gate.lock().unwrap() = Some(write_gate);
                } else {
                    *store.finalize_gate.lock().unwrap() = Some(write_gate);
                }
                let (complete_release, release_gate) = oneshot::channel();
                let admissions = Arc::new(Admissions {
                    release_gate: Mutex::new(Some(release_gate)),
                    ..Default::default()
                });
                let (complete_settlement, settlement_gate) = oneshot::channel();
                let budget = Arc::new(Budget {
                    active: admissions.active.clone(),
                    settlement_gate: Mutex::new(Some(settlement_gate)),
                    ..Default::default()
                });
                let service =
                    early_failure_service(store.clone(), admissions.clone(), budget.clone());
                let mut started = service.start(request(&service, transport)).await.unwrap();
                let mut next = started.session.next_event();
                assert!(futures::poll!(next.as_mut()).is_pending());
                drop(next);
                assert!(!started.session.is_finalized());
                assert_eq!(store.creates.load(Ordering::SeqCst), 1);
                assert_eq!(
                    store.finalizes.load(Ordering::SeqCst),
                    usize::from(!suspend_create)
                );
                assert_eq!(
                    store.requests.lock().unwrap().len(),
                    usize::from(!suspend_create)
                );
                assert!(store.finalizations.lock().unwrap().is_empty());
                assert_eq!(budget.settlements.load(Ordering::SeqCst), 0);
                assert_eq!(admissions.releases.load(Ordering::SeqCst), 0);
                assert!(admissions.active.load(Ordering::SeqCst));

                let mut detached = started.session.detach_finalize();
                assert!(futures::poll!(detached.as_mut()).is_pending());
                assert_eq!(store.creates.load(Ordering::SeqCst), 1);
                assert_eq!(
                    store.finalizes.load(Ordering::SeqCst),
                    usize::from(!suspend_create)
                );
                // receiver 已从 Store 替身取走；只有延续原 future 才能继续接收此信号。
                complete_write
                    .send(())
                    .expect("detached cleanup retains the original store write");
                assert!(futures::poll!(detached.as_mut()).is_pending());
                assert_early_failure_recorded(&store, &started.request_id, transport);
                assert_eq!(budget.settlements.load(Ordering::SeqCst), 1);
                assert!(budget.charges.lock().unwrap().is_empty());
                assert_eq!(admissions.releases.load(Ordering::SeqCst), 0);
                assert!(admissions.active.load(Ordering::SeqCst));

                complete_settlement.send(()).expect("original settlement");
                assert!(futures::poll!(detached.as_mut()).is_pending());
                assert_eq!(budget.settlements.load(Ordering::SeqCst), 1);
                {
                    let charges = budget.charges.lock().unwrap();
                    assert_eq!(charges.len(), 1);
                    assert_eq!(charges[0].amount_usd, Decimal::ZERO);
                }
                assert_eq!(admissions.releases.load(Ordering::SeqCst), 1);
                assert!(admissions.active.load(Ordering::SeqCst));
                complete_release
                    .send(())
                    .expect("original admission release");
                detached.await;
                assert_early_failure_recorded(&store, &started.request_id, transport);
                assert_zero_cleanup_completed(&admissions, &budget, &store, &started.request_id);
            }
        }
    });
}

#[derive(Default)]
struct ChargedProvider {
    policies: Mutex<Vec<bool>>,
    fail: bool,
}

#[derive(Debug)]
struct FixedRoutingPolicy {
    decision: ModelRouteDecision,
    inputs: Mutex<Vec<ModelRouteInput>>,
}

#[derive(Debug)]
struct RetryTestPolicy {
    decision: gateway_core::engine::policy::RetryDecision,
    facts: Arc<Mutex<Vec<gateway_core::engine::policy::RetryFacts>>>,
}

impl RequestPolicyPlan for RetryTestPolicy {
    fn route_model(
        &self,
        _: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(ModelRouteDecision::Unhandled) })
    }
    fn schedule_account(
        &self,
        _: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(AccountScheduleDecision::Delegate) })
    }
    fn retry_decision(
        &self,
        input: gateway_core::engine::policy::RetryInput,
    ) -> BoxFuture<'static, Result<gateway_core::engine::policy::RetryDecision, RequestPolicyFault>>
    {
        self.facts.lock().unwrap().push(input.facts);
        let decision = self.decision;
        Box::pin(async move { Ok(decision) })
    }
}

struct RetryTestProvider {
    error: ProviderError,
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for RetryTestProvider {
    fn name(&self) -> &'static str {
        "openai"
    }
    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }
    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(vec![])
    }
    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
            return Err(ProviderError::new(
                ProviderErrorKind::NoEligibleAccount,
                UpstreamSendState::NotSent,
            ));
        }
        let candidate = request.candidate();
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            candidate.upstream_model().cloned().unwrap(),
            ProviderAccountId::new("acct_openai").unwrap(),
            UpstreamTransport::new("http").unwrap(),
        );
        Ok(ProviderStream::new(
            metadata,
            futures::stream::iter([Err(self.error.clone())]),
            (),
        ))
    }
}

#[test]
fn retry_policy_can_stop_but_cannot_bypass_replay_safety() {
    use gateway_core::engine::policy::RetryDecision;
    for (decision, send_state, replay_safe, expected_allowed, expected_calls) in [
        (
            RetryDecision::Stop,
            UpstreamSendState::NotSent,
            true,
            true,
            1,
        ),
        (
            RetryDecision::Retry,
            UpstreamSendState::NotSent,
            true,
            true,
            2,
        ),
        (
            RetryDecision::Retry,
            UpstreamSendState::Ambiguous,
            true,
            false,
            1,
        ),
        (
            RetryDecision::Retry,
            UpstreamSendState::Sent,
            false,
            false,
            1,
        ),
    ] {
        block_on(async {
            let error = ProviderError::new(ProviderErrorKind::Transport, send_state);
            let error = if replay_safe {
                error.with_replay_safe()
            } else {
                error
            };
            let provider = Arc::new(RetryTestProvider {
                error,
                calls: AtomicUsize::new(0),
            });
            let facts = Arc::new(Mutex::new(vec![]));
            let generation = super::extensions::reference("retry-policy");
            let policies = RequestPolicyExtensionIndex::default();
            let _owner = policies
                .register(
                    generation.id().clone(),
                    Arc::new(RetryTestPolicy {
                        decision,
                        facts: facts.clone(),
                    }),
                )
                .unwrap();
            let service = DefaultExecutionService::new(
                RuntimeSnapshotHandle::new(request_policy_snapshot(
                    generation,
                    &[ProviderKind::new("openai").unwrap()],
                )),
                Arc::new(TrackingExecutionStore::default()),
                ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).unwrap(),
                Arc::new(Admissions::default()),
                Arc::new(UnusedContinuation),
                Arc::new(RecordingClientApiKeyUsage::default()),
            )
            .with_request_policies(policies);
            let mut started = service
                .start(request(&service, ClientTransport::HttpJson))
                .await
                .unwrap();
            assert!(started.session.collect_uncommitted().await.is_err());
            assert_eq!(provider.calls.load(Ordering::SeqCst), expected_calls);
            let observed = facts.lock().unwrap();
            assert_eq!(observed[0].retry_allowed, expected_allowed);
            assert_eq!(observed[0].send_state, send_state);
            assert_eq!(observed[0].remaining_routing_attempts, 31);
        });
    }
}

impl RequestPolicyPlan for FixedRoutingPolicy {
    fn route_model(
        &self,
        input: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        self.inputs.lock().unwrap().push(input);
        let decision = self.decision.clone();
        Box::pin(async move { Ok(decision) })
    }

    fn schedule_account(
        &self,
        _: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(AccountScheduleDecision::Delegate) })
    }
}

#[derive(Debug)]
struct NestedRoutingPolicy;

impl RequestPolicyPlan for NestedRoutingPolicy {
    fn route_model(
        &self,
        input: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        let provider = if input.suppresses_plugin("nested-fixture") {
            "nested"
        } else {
            "openai"
        };
        Box::pin(async move {
            Ok(ModelRouteDecision::Route {
                provider: Some(ProviderKind::new(provider).unwrap()),
                model: None,
            })
        })
    }

    fn schedule_account(
        &self,
        _: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(AccountScheduleDecision::Delegate) })
    }
}

#[derive(Debug)]
struct EffectObservingPolicy;

impl RequestPolicyPlan for EffectObservingPolicy {
    fn route_model(
        &self,
        input: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        input.execution_effects().observe();
        Box::pin(async { Ok(ModelRouteDecision::Unhandled) })
    }

    fn schedule_account(
        &self,
        _: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(AccountScheduleDecision::Delegate) })
    }
}

struct ProcessingProvider;

#[async_trait]
impl Provider for ProcessingProvider {
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
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let candidate = request.candidate();
        let provider = candidate.provider().clone();
        let account = ProviderAccountId::new("acct_openai").unwrap();
        let transport = UpstreamTransport::new("websocket").unwrap();
        let metadata = ProviderCallMetadata::new(
            provider,
            candidate.upstream_model().cloned().unwrap(),
            account,
            transport,
        );
        let response = ResponseMeta::new("response-policy", "gpt-start");
        let event = |event_type: &str, fact| {
            ProviderEvent::canonical_with_wire(
                vec![fact],
                ProtocolWireEvent::json(
                    "openai",
                    Some(event_type.to_owned()),
                    json!({"type": event_type, "future": {"nested": true}}),
                )
                .unwrap(),
            )
        };
        Ok(ProviderStream::new(
            metadata,
            futures::stream::iter([
                Ok(event(
                    "response.created",
                    GatewayEvent::Started(response.clone()),
                )),
                Ok(event(
                    "response.completed",
                    GatewayEvent::Completed(response),
                )),
            ]),
            (),
        ))
    }
}

struct PendingNestedProvider;

#[async_trait]
impl Provider for PendingNestedProvider {
    fn name(&self) -> &'static str {
        "nested"
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
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let candidate = request.candidate();
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            candidate.upstream_model().cloned().unwrap(),
            ProviderAccountId::new("acct_nested").unwrap(),
            UpstreamTransport::new("websocket").unwrap(),
        );
        Ok(ProviderStream::new(
            metadata,
            futures::stream::pending::<Result<ProviderEvent, ProviderError>>(),
            (),
        ))
    }
}

fn request_policy_snapshot(
    generation: ExtensionSetReference,
    providers: &[ProviderKind],
) -> RuntimeSnapshot {
    let directory = Arc::new(RuntimeAccountDirectory::new(
        providers
            .iter()
            .map(|provider| {
                (
                    ProviderAccountId::new(format!("acct_{}", provider.as_str())).unwrap(),
                    RuntimeAccount::new(provider.clone(), BTreeSet::new()),
                )
            })
            .collect(),
    ));
    let capabilities =
        ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(16_000));
    RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            std::num::NonZeroU32::new(2).unwrap(),
            Duration::from_millis(1),
        ),
        providers.to_vec(),
        providers
            .iter()
            .map(|provider| {
                ProviderModel::new(
                    provider.clone(),
                    UpstreamModelId::new("gpt-start").unwrap(),
                    capabilities.clone(),
                )
            })
            .collect(),
        vec![ClientPolicy::new(
            ClientApiKeyId::new("key_start_test").unwrap(),
            PlaintextClientApiKey::new("sk_start_test").unwrap(),
            Arc::new(FrozenAccountScope::new(
                Arc::clone(&directory),
                ClientRoutingScope::all_accounts(),
            )),
            true,
            RateLimits::unlimited(),
        )],
    )
    .unwrap()
    .with_account_directory(directory)
    .with_extensions(Some(generation))
}

#[test]
fn completed_parent_rejects_new_nested_execution_and_cancels_an_active_child() {
    block_on(async {
        let parent_provider = Arc::new(ProcessingProvider);
        let providers = ProviderRegistry::new([
            parent_provider as Arc<dyn Provider>,
            Arc::new(PendingNestedProvider) as Arc<dyn Provider>,
        ])
        .unwrap();
        let provider_index = providers;
        let generation = super::extensions::reference("nested-cancel");
        let policy_index = RequestPolicyExtensionIndex::default();
        let _owner = policy_index
            .register(generation.id().clone(), Arc::new(NestedRoutingPolicy))
            .unwrap();
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(request_policy_snapshot(
                generation,
                &[
                    ProviderKind::new("openai").unwrap(),
                    ProviderKind::new("nested").unwrap(),
                ],
            )),
            Arc::new(TrackingExecutionStore::default()),
            provider_index.clone(),
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        )
        .with_request_policies(policy_index);

        let mut parent = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        let nested_request = |parent_request_id| NestedModelExecutionRequest {
            parent_request_id,
            initiating_plugin_instance_id: "nested-fixture".to_owned(),
            public_model: PublicModelId::new("gpt-start").unwrap(),
            operation: start_operation(),
            metadata: ExecutionRequestMetadata {
                protocol: "openai".to_owned(),
                endpoint: "host.model".to_owned(),
                transport: ClientTransport::InternalPlugin,
                stream: true,
                client_ip: None,
                user_agent: None,
                previous_response_id: None,
            },
            provider: Some(ProviderKind::new("nested").unwrap()),
            account: None,
            parent_account: None,
        };
        let mut child = gateway_core::engine::nested::NestedModelExecutionPort::start(
            &service,
            nested_request(parent.request_id.clone()),
        )
        .await
        .unwrap();

        let events = parent.session.collect_uncommitted().await.unwrap();
        assert!(
            events
                .iter()
                .any(|event| { matches!(event.canonical_facts(), [GatewayEvent::Completed(_)]) })
        );
        parent.session.commit_downstream(Some(200)).await.unwrap();
        assert!(parent.session.is_finalized());

        let new_child = gateway_core::engine::nested::NestedModelExecutionPort::start(
            &service,
            nested_request(parent.request_id.clone()),
        )
        .await;
        assert!(
            matches!(new_child, Err(error) if error.kind() == GatewayErrorKind::PolicyDenied),
            "a finalized parent must be removed from the callback registry"
        );
        assert!(
            matches!(
                child.session.next_event().await,
                Err(EngineError::Cancelled)
            ),
            "an in-flight child must inherit cancellation from the released parent authority"
        );
        child.session.detach_finalize().await;
        parent.session.detach_finalize().await;
    });
}

struct TestNativeResponseTranslator;

impl NativeResponseTranslator for TestNativeResponseTranslator {
    fn source_protocol(&self) -> &str {
        "xai"
    }

    fn target_protocol(&self) -> &str {
        "openai"
    }

    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if event.data().get("drop").and_then(Value::as_bool) == Some(true) {
            return Ok(Vec::new());
        }
        let count = if event.data().get("expand").and_then(Value::as_bool) == Some(true) {
            2
        } else {
            1
        };
        (0..count)
            .map(|part| {
                let mut body = event.data().clone();
                body["native_translated"] = json!(true);
                body["native_part"] = json!(part);
                ProtocolWireEvent::json("openai", event.event_type().map(str::to_owned), body)
                    .map_err(|_| {
                        ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
                    })
            })
            .collect()
    }
}

struct NativeResponseBoundaryProvider;

#[async_trait]
impl Provider for NativeResponseBoundaryProvider {
    fn name(&self) -> &'static str {
        "xai"
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
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let candidate = request.candidate();
        let response = ResponseMeta::new("response-native-boundary", "gpt-start");
        let event = |event_type: &str, body: Value, facts: Vec<GatewayEvent>| {
            let wire = ProtocolWireEvent::json("xai", Some(event_type.to_owned()), body).unwrap();
            if facts.is_empty() {
                ProviderEvent::wire(wire)
            } else {
                ProviderEvent::canonical_with_wire(facts, wire)
            }
        };
        let events = futures::stream::iter([
            Ok(event(
                "response.created",
                json!({"type":"response.created","future":{"nested":true}}),
                vec![GatewayEvent::Started(response.clone())],
            )),
            Ok(event(
                "response.internal",
                json!({"type":"response.internal","drop":true}),
                vec![GatewayEvent::Usage(Usage {
                    input_tokens: Some(3),
                    total_tokens: Some(3),
                    ..Usage::default()
                })],
            )),
            Ok(event(
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","delta":"hi","expand":true}),
                Vec::new(),
            )),
            Ok(event(
                "response.completed",
                json!({"type":"response.completed"}),
                vec![GatewayEvent::Completed(response)],
            )),
        ]);
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            candidate.upstream_model().unwrap().clone(),
            ProviderAccountId::new("acct_xai").unwrap(),
            UpstreamTransport::new("websocket").unwrap(),
        );
        Ok(ProviderStream::new(metadata, events, ())
            .with_native_response_translator(TestNativeResponseTranslator))
    }
}

#[test]
fn native_response_processing_uses_the_real_translation_boundary() {
    block_on(async {
        let provider_index =
            ProviderRegistry::new([Arc::new(NativeResponseBoundaryProvider) as Arc<dyn Provider>])
                .unwrap();
        let generation = super::extensions::reference("native-response");
        let observer_index = RequestObserverExtensionIndex::default();
        let observer = Arc::new(RecordingRequestObserver::default());
        let _observer_owner = observer_index
            .register(generation.id().clone(), observer.clone())
            .unwrap();
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(request_policy_snapshot(
                generation,
                &[ProviderKind::new("xai").unwrap()],
            )),
            Arc::new(TrackingExecutionStore::default()),
            provider_index.clone(),
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        )
        .with_request_observers(observer_index);

        let mut started = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        let events = started.session.collect_uncommitted().await.unwrap();
        assert_eq!(events.len(), 5);
        assert!(events.iter().all(|event| {
            event.wire_event().is_none_or(|wire| {
                wire.protocol() == "openai" && wire.data()["native_translated"] == json!(true)
            })
        }));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.wire_event().is_none())
                .flat_map(ProviderEvent::canonical_facts)
                .filter(|fact| matches!(fact, GatewayEvent::Usage(_)))
                .count(),
            1
        );
        assert_eq!(
            events[0]
                .wire_event()
                .and_then(|wire| wire.data().pointer("/future/nested")),
            Some(&json!(true))
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event
                    .wire_event()
                    .is_some_and(|wire| wire.data()["expand"] == json!(true)))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .filter(|fact| matches!(fact, GatewayEvent::Started(_)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .filter(|fact| matches!(fact, GatewayEvent::Completed(_)))
                .count(),
            1
        );

        {
            let websocket = observer.websocket_observations.lock().unwrap();
            assert_eq!(websocket.len(), 4);
            assert!(websocket.iter().all(|observation| {
                observation.wire().protocol() == "xai"
                    && observation.wire().data().get("native_translated").is_none()
            }));
        }
        started.session.commit_downstream(Some(200)).await.unwrap();
        started.session.detach_finalize().await;
    });
}

struct NativeTranslationFactsProvider;

#[async_trait]
impl Provider for NativeTranslationFactsProvider {
    fn name(&self) -> &'static str {
        "xai"
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
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let candidate = request.candidate();
        let response = ResponseMeta::new("response-translated-facts", "gpt-start");
        let event = |event_type: &str, body: Value, facts: Vec<GatewayEvent>| {
            ProviderEvent::canonical_with_wire(
                facts,
                ProtocolWireEvent::json("xai", Some(event_type.to_owned()), body).unwrap(),
            )
        };
        let events = futures::stream::iter([
            Ok(event(
                "response.created",
                json!({"type":"response.created"}),
                vec![GatewayEvent::Started(response.clone())],
            )),
            Ok(event(
                "response.usage",
                json!({"type":"response.usage","drop":true}),
                vec![GatewayEvent::Usage(Usage {
                    input_tokens: Some(9),
                    output_tokens: Some(4),
                    total_tokens: Some(13),
                    ..Usage::default()
                })],
            )),
            Ok(event(
                "response.completed",
                json!({"type":"response.completed"}),
                vec![GatewayEvent::Completed(response)],
            )),
        ]);
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            candidate.upstream_model().unwrap().clone(),
            ProviderAccountId::new("acct_xai").unwrap(),
            UpstreamTransport::new("http_sse").unwrap(),
        );
        Ok(ProviderStream::new(metadata, events, ())
            .with_native_response_translator(TestNativeResponseTranslator))
    }
}

#[test]
fn response_translation_zero_output_preserves_canonical_usage_and_finalization() {
    block_on(async {
        let provider_index =
            ProviderRegistry::new([Arc::new(NativeTranslationFactsProvider) as Arc<dyn Provider>])
                .unwrap();
        let generation = super::extensions::reference("translated-facts");
        let store = Arc::new(TrackingExecutionStore::default());
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(request_policy_snapshot(
                generation,
                &[ProviderKind::new("xai").unwrap()],
            )),
            store.clone(),
            provider_index.clone(),
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        );

        let mut started = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        let events = started.session.collect_uncommitted().await.unwrap();
        assert_eq!(events.len(), 3);
        let usage = events
            .iter()
            .filter(|event| event.wire_event().is_none())
            .flat_map(ProviderEvent::canonical_facts)
            .find_map(|fact| match fact {
                GatewayEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .expect("wire-filtered Usage must remain client-visible");
        assert_eq!(usage.total_tokens, Some(13));
        assert_eq!(
            events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .filter(|fact| matches!(fact, GatewayEvent::Completed(_)))
                .count(),
            1
        );

        started.session.commit_downstream(Some(200)).await.unwrap();
        started.session.detach_finalize().await;
        let finalizations = store.finalizations.lock().unwrap();
        assert_eq!(finalizations.len(), 1);
        assert_eq!(finalizations[0].usage, usage.clone());
    });
}

#[derive(Default)]
struct FailingNativeResponseTranslator {
    translated: bool,
}

impl NativeResponseTranslator for FailingNativeResponseTranslator {
    fn source_protocol(&self) -> &str {
        "xai"
    }

    fn target_protocol(&self) -> &str {
        "openai"
    }

    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if self.translated {
            return Err(ProviderError::new(
                ProviderErrorKind::Protocol,
                UpstreamSendState::Sent,
            ));
        }
        self.translated = true;
        Ok(vec![
            ProtocolWireEvent::json(
                "openai",
                event.event_type().map(str::to_owned),
                event.data().clone(),
            )
            .unwrap(),
        ])
    }
}

struct FailingNativeResponseProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for FailingNativeResponseProvider {
    fn name(&self) -> &'static str {
        "xai"
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
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let candidate = request.candidate();
        let response = ResponseMeta::new("response-native-failure", "gpt-start");
        let event = |event_type: &str, fact| {
            ProviderEvent::canonical_with_wire(
                vec![fact],
                ProtocolWireEvent::json(
                    "xai",
                    Some(event_type.to_owned()),
                    json!({"type": event_type}),
                )
                .unwrap(),
            )
        };
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            candidate.upstream_model().unwrap().clone(),
            ProviderAccountId::new("acct_xai").unwrap(),
            UpstreamTransport::new("websocket").unwrap(),
        );
        Ok(ProviderStream::new(
            metadata,
            futures::stream::iter([
                Ok(event(
                    "response.created",
                    GatewayEvent::Started(response.clone()),
                )),
                Ok(event(
                    "response.completed",
                    GatewayEvent::Completed(response),
                )),
            ]),
            (),
        )
        .with_native_response_translator(FailingNativeResponseTranslator::default()))
    }
}

#[test]
fn native_response_failure_after_downstream_commit_is_not_replayed() {
    block_on(async {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider_index = ProviderRegistry::new([Arc::new(FailingNativeResponseProvider {
            calls: Arc::clone(&calls),
        }) as Arc<dyn Provider>])
        .unwrap();
        let generation = super::extensions::reference("native-response-failure");
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(request_policy_snapshot(
                generation,
                &[ProviderKind::new("xai").unwrap()],
            )),
            Arc::new(TrackingExecutionStore::default()),
            provider_index.clone(),
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        );

        let mut started = service
            .start(request(&service, ClientTransport::HttpSse))
            .await
            .unwrap();
        let first = started.session.next_event().await.unwrap().unwrap();
        assert_eq!(
            first.commit_requirement(),
            CommitRequirement::CommitBeforeDelivery
        );
        assert!(first.into_provider_events().iter().all(|event| {
            event
                .wire_event()
                .is_some_and(|wire| wire.protocol() == "openai")
        }));
        started.session.commit_downstream(Some(200)).await.unwrap();

        let error = started.session.next_event().await.unwrap_err();
        assert!(matches!(
            error,
            EngineError::Provider(ref error)
                if error.kind() == ProviderErrorKind::Protocol
                    && error.send_state() == UpstreamSendState::Sent
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        started.session.detach_finalize().await;
    });
}

#[test]
fn model_routing_policy_selects_native_provider() {
    block_on(async {
        let target = "xai";
        let openai = Arc::new(ObservationProvider {
            name: "openai",
            behavior: ObservationProviderBehavior::CompleteWithUsage,
            calls: AtomicUsize::new(0),
        });
        let selected = Arc::new(ObservationProvider {
            name: target,
            behavior: ObservationProviderBehavior::CompleteWithUsage,
            calls: AtomicUsize::new(0),
        });
        let provider_index = ProviderRegistry::new([
            openai.clone() as Arc<dyn Provider>,
            selected.clone() as Arc<dyn Provider>,
        ])
        .unwrap();
        let generation = super::extensions::reference(&format!("route-{target}"));
        let policy_index = RequestPolicyExtensionIndex::default();
        let policy = Arc::new(FixedRoutingPolicy {
            decision: ModelRouteDecision::Route {
                provider: Some(ProviderKind::new(target).unwrap()),
                model: None,
            },
            inputs: Mutex::new(Vec::new()),
        });
        let _owner = policy_index
            .register(generation.id().clone(), policy.clone())
            .unwrap();
        let providers = [
            ProviderKind::new("openai").unwrap(),
            ProviderKind::new(target).unwrap(),
        ];
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(request_policy_snapshot(generation, &providers)),
            Arc::new(TrackingExecutionStore::default()),
            provider_index.clone(),
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        )
        .with_request_policies(policy_index);
        let mut started = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        started.session.collect_uncommitted().await.unwrap();
        started.session.detach_finalize().await;
        assert_eq!(openai.calls.load(Ordering::SeqCst), 0);
        assert_eq!(selected.calls.load(Ordering::SeqCst), 1);
        let inputs = policy.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].client_key_id().as_str(), "key_start_test");
        assert_eq!(
            inputs[0]
                .available_providers()
                .iter()
                .map(ProviderKind::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["openai", target])
        );
    });
}

#[test]
fn model_routing_reject_stops_before_provider_execution() {
    block_on(async {
        let provider = Arc::new(ObservationProvider {
            name: "openai",
            behavior: ObservationProviderBehavior::CompleteWithUsage,
            calls: AtomicUsize::new(0),
        });
        let provider_index =
            ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).unwrap();
        let generation = super::extensions::reference("route-reject");
        let policy_index = RequestPolicyExtensionIndex::default();
        let policy: Arc<dyn RequestPolicyPlan> = Arc::new(FixedRoutingPolicy {
            decision: ModelRouteDecision::Reject,
            inputs: Mutex::new(Vec::new()),
        });
        let _owner = policy_index
            .register(generation.id().clone(), policy)
            .unwrap();
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(request_policy_snapshot(
                generation,
                &[ProviderKind::new("openai").unwrap()],
            )),
            Arc::new(TrackingExecutionStore::default()),
            provider_index.clone(),
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        )
        .with_request_policies(policy_index);
        assert!(matches!(
            service.start(request(&service, ClientTransport::HttpJson)).await,
            Err(error) if error.kind() == GatewayErrorKind::PolicyDenied
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn model_routing_cannot_expand_the_frozen_key_model_scope() {
    block_on(async {
        let kind = ProviderKind::new("openai").unwrap();
        let provider = Arc::new(ObservationProvider {
            name: "openai",
            behavior: ObservationProviderBehavior::CompleteWithUsage,
            calls: AtomicUsize::new(0),
        });
        let provider_index =
            ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).unwrap();
        let generation = super::extensions::reference("route-model-scope");
        let policy_index = RequestPolicyExtensionIndex::default();
        let policy: Arc<dyn RequestPolicyPlan> = Arc::new(FixedRoutingPolicy {
            decision: ModelRouteDecision::Route {
                provider: None,
                model: Some(PublicModelId::new("gpt-blocked").unwrap()),
            },
            inputs: Mutex::new(Vec::new()),
        });
        let _owner = policy_index
            .register(generation.id().clone(), policy)
            .unwrap();
        let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new("acct_openai").unwrap(),
            RuntimeAccount::new(kind.clone(), BTreeSet::new()).with_model_access(
                AccountModelAccess::new(
                    AccountModelAccessMode::Allowlist,
                    vec!["gpt-start".to_owned()],
                )
                .unwrap(),
            ),
        )])));
        let capabilities =
            ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(16_000));
        let snapshot = RuntimeSnapshot::new(
            ConfigRevision::new(1).unwrap(),
            AccountSelectionPolicy::new(
                RotationStrategy::Smart,
                std::num::NonZeroU32::new(2).unwrap(),
                Duration::from_millis(1),
            ),
            vec![kind.clone()],
            ["gpt-start", "gpt-blocked"]
                .into_iter()
                .map(|model| {
                    ProviderModel::new(
                        kind.clone(),
                        UpstreamModelId::new(model).unwrap(),
                        capabilities.clone(),
                    )
                })
                .collect(),
            vec![ClientPolicy::new(
                ClientApiKeyId::new("key_start_test").unwrap(),
                PlaintextClientApiKey::new("sk_start_test").unwrap(),
                Arc::new(FrozenAccountScope::new(
                    Arc::clone(&directory),
                    ClientRoutingScope::all_accounts(),
                )),
                true,
                RateLimits::unlimited(),
            )],
        )
        .unwrap()
        .with_account_directory(directory)
        .with_extensions(Some(generation));
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(snapshot),
            Arc::new(TrackingExecutionStore::default()),
            provider_index.clone(),
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        )
        .with_request_policies(policy_index);

        assert!(matches!(
            service.start(request(&service, ClientTransport::HttpJson)).await,
            Err(error) if error.kind() == GatewayErrorKind::NoAvailableProvider
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    });
}

#[derive(Debug, Default)]
struct RejectingSchedulerPolicy {
    inputs: Mutex<Vec<AccountScheduleInput>>,
}

impl RequestPolicyPlan for RejectingSchedulerPolicy {
    fn route_model(
        &self,
        _: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(ModelRouteDecision::Unhandled) })
    }

    fn schedule_account(
        &self,
        input: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        self.inputs.lock().unwrap().push(input);
        Box::pin(async { Ok(AccountScheduleDecision::Reject) })
    }
}

struct PolicySelectingProvider;

#[async_trait]
impl Provider for PolicySelectingProvider {
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
        self: Arc<Self>,
        request: ProviderRequest,
        attempt: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let account = ProviderAccount::new(
            ProviderAccountId::new("acct_openai").unwrap(),
            ProviderKind::new("openai").unwrap(),
            "scheduler test".to_owned(),
            None,
            "test".to_owned(),
            CredentialRevision::new(1).unwrap(),
            None,
        )
        .with_account_facts(
            true,
            CredentialState::Ready,
            QuotaState::unknown(),
            None,
            None,
        )
        .with_scheduling(None, AccountWeight::new(100).unwrap());
        let candidates = [AccountCandidate {
            account,
            signals: AccountRuntimeSignals {
                in_flight: 0,
                last_started_at: None,
                quota_reset_at: None,
                quota_remaining_rank: None,
                cooldown: None,
                failure_rate_basis_points: None,
                first_output_latency_ms: None,
            },
        }];
        let selection = AccountSelectionContext {
            policy: attempt.account_selection_policy(),
            now: SystemTime::now(),
            excluded_accounts: attempt.excluded_accounts().clone(),
            preferred_account: attempt.required_account().cloned(),
            preferred_account_overrides_weight: true,
            round_robin_cursor: 0,
            eligibility: AccountEligibilityPolicy::Enforce,
            account_scope: attempt.account_scope().cloned(),
        };
        match attempt
            .select_account(
                request.candidate().provider(),
                request
                    .candidate()
                    .upstream_model()
                    .map(UpstreamModelId::as_str),
                &candidates,
                &selection,
            )
            .await
        {
            Err(AccountPolicyError::Rejected) => Err(ProviderError::new(
                ProviderErrorKind::RequestPolicyDenied,
                UpstreamSendState::NotSent,
            )),
            Err(AccountPolicyError::Fault | AccountPolicyError::StaleCandidate) => Err(
                ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent),
            ),
            Ok(_) => panic!("rejecting scheduler unexpectedly selected an account"),
        }
    }
}

#[test]
fn scheduler_reject_is_a_terminal_policy_rejection_before_upstream_send() {
    block_on(async {
        let provider_index =
            ProviderRegistry::new([Arc::new(PolicySelectingProvider) as Arc<dyn Provider>])
                .unwrap();
        let generation = super::extensions::reference("schedule-reject");
        let policy_index = RequestPolicyExtensionIndex::default();
        let policy = Arc::new(RejectingSchedulerPolicy::default());
        let _policy_owner = policy_index
            .register(generation.id().clone(), policy.clone())
            .unwrap();
        let observer_index = RequestObserverExtensionIndex::default();
        let observer = Arc::new(RecordingRequestObserver::default());
        let _observer_owner = observer_index
            .register(generation.id().clone(), observer.clone())
            .unwrap();
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(request_policy_snapshot(
                generation,
                &[ProviderKind::new("openai").unwrap()],
            )),
            Arc::new(TrackingExecutionStore::default()),
            provider_index.clone(),
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        )
        .with_request_policies(policy_index)
        .with_request_observers(observer_index);
        let mut started = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        let error = started.session.collect_uncommitted().await.unwrap_err();
        let provider_error = match error {
            EngineError::Provider(error) => error,
            error => panic!("unexpected engine error: {error:?}"),
        };
        assert_eq!(
            provider_error.kind(),
            ProviderErrorKind::RequestPolicyDenied
        );
        assert_eq!(provider_error.send_state(), UpstreamSendState::NotSent);
        assert_eq!(
            GatewayError::from_provider(&provider_error).kind(),
            GatewayErrorKind::PolicyDenied
        );
        started.session.detach_finalize().await;
        let observations = observer.observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].outcome(),
            RequestObservationOutcome::Rejected
        );
        assert_eq!(observations[0].send_state(), UpstreamSendState::NotSent);
        assert_eq!(
            observations[0].attempt_count(),
            0,
            "调度拒绝发生在 ProviderStream/上游 attempt 持久化前"
        );
        assert_eq!(policy.inputs.lock().unwrap().len(), 1);
    });
}

fn known_charge() -> Decimal {
    "1.25".parse().unwrap()
}

#[derive(Clone, Copy)]
enum ObservationProviderBehavior {
    CompleteWithUsage,
    RetryableFailure,
    CapacityFailure,
}

struct ObservationProvider {
    name: &'static str,
    behavior: ObservationProviderBehavior,
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for ObservationProvider {
    fn name(&self) -> &'static str {
        self.name
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
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behavior {
            ObservationProviderBehavior::CompleteWithUsage => {
                let candidate = request.candidate();
                let metadata = ProviderCallMetadata::new(
                    candidate.provider().clone(),
                    candidate.upstream_model().unwrap().clone(),
                    ProviderAccountId::new(format!("acct_{}", self.name)).unwrap(),
                    UpstreamTransport::new("http_sse").unwrap(),
                );
                let response = ResponseMeta::new("response-observed", "gpt-start");
                let mut started: ProviderEvent = GatewayEvent::Started(response.clone()).into();
                started.attach_observation(
                    gateway_core::event::ProviderResponseObservation::new(
                        UpstreamTransport::new("http_sse").unwrap(),
                    )
                    .with_upstream_response_model_if_valid("gpt-reported")
                    .with_service_tier_if_valid("priority"),
                );
                Ok(ProviderStream::new(
                    metadata,
                    futures::stream::iter([
                        Ok(started),
                        Ok(GatewayEvent::Usage(Usage {
                            input_tokens: Some(11),
                            output_tokens: Some(7),
                            total_tokens: Some(18),
                            ..Usage::default()
                        })
                        .into()),
                        Ok(GatewayEvent::ProviderCost(
                            ProviderReportedCost::from_usd_ticks(123_000_000).unwrap(),
                        )
                        .into()),
                        Ok(GatewayEvent::Completed(response).into()),
                    ]),
                    (),
                ))
            }
            ObservationProviderBehavior::CapacityFailure => Err(ProviderError::new(
                ProviderErrorKind::NoEligibleAccount,
                UpstreamSendState::NotSent,
            )),
            ObservationProviderBehavior::RetryableFailure => {
                if call_index > 0 {
                    return Err(ProviderError::new(
                        ProviderErrorKind::NoEligibleAccount,
                        UpstreamSendState::NotSent,
                    ));
                }
                let candidate = request.candidate();
                let metadata = ProviderCallMetadata::new(
                    candidate.provider().clone(),
                    candidate.upstream_model().unwrap().clone(),
                    ProviderAccountId::new(format!("acct_{}", self.name)).unwrap(),
                    UpstreamTransport::new("http_sse").unwrap(),
                );
                let error =
                    ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::NotSent)
                        .with_status(503)
                        .with_retry_after(Duration::from_millis(750))
                        .with_pre_delivery_retry();
                Ok(ProviderStream::new(
                    metadata,
                    futures::stream::iter([Err(error)]),
                    (),
                ))
            }
        }
    }
}

#[derive(Default)]
struct RecordingRequestObserver {
    observations: Mutex<Vec<RequestObservation>>,
    websocket_observations: Mutex<Vec<WebSocketResponseObservation>>,
}

impl RequestObserverPlan for RecordingRequestObserver {
    fn dispatch_websocket_response(
        &self,
        generation: ExtensionSetReference,
        observation: WebSocketResponseObservation,
    ) {
        assert!(generation.is_ready());
        self.websocket_observations
            .lock()
            .unwrap()
            .push(observation);
    }

    fn dispatch(&self, generation: ExtensionSetReference, observation: RequestObservation) {
        assert!(generation.is_ready());
        self.observations.lock().unwrap().push(observation);
    }
}

fn fallback_observation_snapshot(reference: ExtensionSetReference) -> RuntimeSnapshot {
    let providers = [
        ProviderKind::new("openai").unwrap(),
        ProviderKind::new("xai").unwrap(),
    ];
    let enabled_group = AccountGroupId::new("grp_11111111111111111111111111111111").unwrap();
    let disabled_group = AccountGroupId::new("grp_22222222222222222222222222222222").unwrap();
    let directory = Arc::new(RuntimeAccountDirectory::new(
        providers
            .iter()
            .map(|provider| {
                (
                    ProviderAccountId::new(format!("acct_{}", provider.as_str())).unwrap(),
                    RuntimeAccount::new(provider.clone(), BTreeSet::from([enabled_group.clone()])),
                )
            })
            .collect(),
    ));
    let capabilities =
        ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(16_000));
    RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            std::num::NonZeroU32::new(2).unwrap(),
            Duration::from_millis(1),
        ),
        providers.to_vec(),
        providers
            .iter()
            .map(|provider| {
                ProviderModel::new(
                    provider.clone(),
                    UpstreamModelId::new("gpt-start").unwrap(),
                    capabilities.clone(),
                )
            })
            .collect(),
        vec![ClientPolicy::new(
            ClientApiKeyId::new("key_start_test").unwrap(),
            PlaintextClientApiKey::new("sk_start_test").unwrap(),
            Arc::new(FrozenAccountScope::new(
                Arc::clone(&directory),
                ClientRoutingScope::restricted(
                    vec![
                        RoutingGroupSnapshot::new(enabled_group.clone(), "Enabled".to_owned()),
                        RoutingGroupSnapshot::new(disabled_group, "Disabled".to_owned()),
                    ],
                    BTreeSet::from([enabled_group]),
                    BTreeSet::from(providers),
                )
                .unwrap(),
            )),
            true,
            RateLimits::unlimited(),
        )],
    )
    .unwrap()
    .with_account_directory(directory)
    .with_extensions(Some(reference))
}

fn observation_service(
    behavior: ObservationProviderBehavior,
    reject_budget: bool,
) -> (
    DefaultExecutionService,
    Arc<RecordingRequestObserver>,
    Arc<dyn RequestObserverPlan>,
) {
    let openai = Arc::new(ObservationProvider {
        name: "openai",
        behavior,
        calls: AtomicUsize::new(0),
    });
    let xai = Arc::new(ObservationProvider {
        name: "xai",
        behavior: ObservationProviderBehavior::CapacityFailure,
        calls: AtomicUsize::new(0),
    });
    let providers =
        ProviderRegistry::new([openai as Arc<dyn Provider>, xai as Arc<dyn Provider>]).unwrap();
    let generation = super::extensions::reference("observer-terminal");
    let observer_index = RequestObserverExtensionIndex::default();
    let observer = Arc::new(RecordingRequestObserver::default());
    let owner = observer_index
        .register(generation.id().clone(), observer.clone())
        .unwrap();
    let admissions = Arc::new(Admissions::default());
    let budget = Arc::new(Budget {
        reject: reject_budget,
        active: admissions.active.clone(),
        ..Budget::default()
    });
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(fallback_observation_snapshot(generation)),
        Arc::new(TrackingExecutionStore::default()),
        providers,
        admissions,
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    )
    .with_budget(budget)
    .with_request_observers(observer_index);
    (service, observer, owner)
}

#[test]
fn fallback_exhaustion_observation_uses_the_last_actual_provider_not_the_candidate_cursor() {
    block_on(async {
        let openai = Arc::new(ObservationProvider {
            name: "openai",
            behavior: ObservationProviderBehavior::RetryableFailure,
            calls: AtomicUsize::new(0),
        });
        let xai = Arc::new(ObservationProvider {
            name: "xai",
            behavior: ObservationProviderBehavior::CapacityFailure,
            calls: AtomicUsize::new(0),
        });
        let providers = ProviderRegistry::new([
            openai.clone() as Arc<dyn Provider>,
            xai.clone() as Arc<dyn Provider>,
        ])
        .unwrap();
        let generation = super::extensions::reference("observer-fallback");
        let observer_index = RequestObserverExtensionIndex::default();
        let observer = Arc::new(RecordingRequestObserver::default());
        let _observer_owner = observer_index
            .register(generation.id().clone(), observer.clone())
            .unwrap();
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(fallback_observation_snapshot(generation)),
            Arc::new(TrackingExecutionStore::default()),
            providers,
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        )
        .with_request_observers(observer_index);
        let mut started = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        assert!(matches!(
            started.session.collect_uncommitted().await,
            Err(EngineError::Provider(error))
                if error.kind() == ProviderErrorKind::Transport
        ));
        started.session.detach_finalize().await;

        assert_eq!(openai.calls.load(Ordering::SeqCst), 2);
        assert_eq!(xai.calls.load(Ordering::SeqCst), 1);
        let observations = observer.observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].provider().map(ProviderKind::as_str),
            Some("openai"),
            "第二候选的容量拒绝不能覆盖最后一次真实 attempt 的 Provider"
        );
        assert_eq!(observations[0].outcome(), RequestObservationOutcome::Failed);
        assert_eq!(observations[0].upstream_status_code(), Some(503));
        assert_eq!(observations[0].retry_after_ms(), Some(750));
        assert_observation_scope(&observations[0]);
    });
}

#[test]
fn routing_external_effect_stops_a_not_sent_provider_retry() {
    block_on(async {
        let openai = Arc::new(ObservationProvider {
            name: "openai",
            behavior: ObservationProviderBehavior::RetryableFailure,
            calls: AtomicUsize::new(0),
        });
        let xai = Arc::new(ObservationProvider {
            name: "xai",
            behavior: ObservationProviderBehavior::CompleteWithUsage,
            calls: AtomicUsize::new(0),
        });
        let providers = ProviderRegistry::new([
            openai.clone() as Arc<dyn Provider>,
            xai.clone() as Arc<dyn Provider>,
        ])
        .unwrap();
        let generation = super::extensions::reference("routing-effect");
        let policy_index = RequestPolicyExtensionIndex::default();
        let _policy_owner = policy_index
            .register(generation.id().clone(), Arc::new(EffectObservingPolicy))
            .unwrap();
        let observer_index = RequestObserverExtensionIndex::default();
        let observer = Arc::new(RecordingRequestObserver::default());
        let _observer_owner = observer_index
            .register(generation.id().clone(), observer.clone())
            .unwrap();
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(fallback_observation_snapshot(generation)),
            Arc::new(TrackingExecutionStore::default()),
            providers,
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        )
        .with_request_policies(policy_index)
        .with_request_observers(observer_index);
        let mut started = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        assert!(matches!(
            started.session.collect_uncommitted().await,
            Err(EngineError::Provider(error))
                if error.kind() == ProviderErrorKind::Transport
        ));
        started.session.detach_finalize().await;

        assert_eq!(openai.calls.load(Ordering::SeqCst), 1);
        assert_eq!(xai.calls.load(Ordering::SeqCst), 0);
        let observations = observer.observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].send_state(), UpstreamSendState::Ambiguous);
        assert_eq!(observations[0].attempt_count(), 1);
    });
}

#[test]
fn final_observation_is_emitted_once_for_success_rejection_and_detached_cancellation() {
    block_on(async {
        let (service, observer, _owner) =
            observation_service(ObservationProviderBehavior::CompleteWithUsage, false);
        let mut started = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        assert_eq!(
            started.session.collect_uncommitted().await.unwrap().len(),
            4
        );
        started.session.commit_downstream(Some(200)).await.unwrap();
        started.session.detach_finalize().await;
        {
            let observations = observer.observations.lock().unwrap();
            assert_eq!(observations.len(), 1);
            assert_eq!(
                observations[0].account_id().map(ProviderAccountId::as_str),
                Some("acct_openai")
            );
            assert_eq!(
                observations[0]
                    .upstream_model()
                    .map(UpstreamModelId::as_str),
                Some("gpt-start")
            );
            assert_eq!(observations[0].response_model(), Some("gpt-reported"));
            assert_eq!(observations[0].service_tier(), Some("priority"));
            assert_eq!(
                observations[0].outcome(),
                RequestObservationOutcome::Succeeded
            );
            assert_eq!(observations[0].usage().total_tokens, Some(18));
            assert_eq!(observations[0].attempt_count(), 1);
            assert_eq!(observations[0].cost().status(), CostEstimateStatus::Known);
            assert_eq!(
                observations[0].cost().source(),
                CostSource::ProviderReported
            );
            assert_eq!(
                observations[0]
                    .cost()
                    .total()
                    .map(|money| money.amount().canonical()),
                Some("0.0123".to_owned())
            );
            assert!(observations[0].timings().latency_ms.is_some());
            assert_observation_scope(&observations[0]);
        }

        let (service, observer, _owner) =
            observation_service(ObservationProviderBehavior::CompleteWithUsage, true);
        assert!(matches!(
            service.start(request(&service, ClientTransport::HttpJson)).await,
            Err(error) if error.kind() == GatewayErrorKind::RateLimited
        ));
        {
            let observations = observer.observations.lock().unwrap();
            assert_eq!(observations.len(), 1);
            assert_eq!(
                observations[0].outcome(),
                RequestObservationOutcome::Rejected
            );
            assert!(observations[0].provider().is_none());
            assert_eq!(observations[0].attempt_count(), 0);
            assert_observation_scope(&observations[0]);
        }

        let (service, observer, _owner) =
            observation_service(ObservationProviderBehavior::CompleteWithUsage, false);
        let started = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        started.session.detach_finalize().await;
        let observations = observer.observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].outcome(),
            RequestObservationOutcome::Cancelled
        );
        assert!(observations[0].provider().is_none());
        assert_eq!(observations[0].attempt_count(), 0);
        assert_observation_scope(&observations[0]);
    });
}

fn assert_observation_scope(observation: &RequestObservation) {
    assert_eq!(
        observation.client_key_id().map(ClientApiKeyId::as_str),
        Some("key_start_test")
    );
    assert_eq!(
        observation
            .account_group_ids()
            .iter()
            .map(AccountGroupId::as_str)
            .collect::<Vec<_>>(),
        vec![
            "grp_11111111111111111111111111111111",
            "grp_22222222222222222222222222222222",
        ],
        "冻结观察范围必须保留禁用但已绑定的账号组"
    );
}

#[async_trait]
impl Provider for ChargedProvider {
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
        self: Arc<Self>,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        self.policies.lock().unwrap().push(context.disable_fast());
        let candidate = request.candidate();
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            candidate.upstream_model().unwrap().clone(),
            ProviderAccountId::new("acct_start").unwrap(),
            UpstreamTransport::new("websocket").unwrap(),
        );
        let response = ResponseMeta::new("resp_cleanup", "gpt-start");
        let terminal = if self.fail {
            Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::Sent,
            ))
        } else {
            Ok(GatewayEvent::Completed(response.clone()).into())
        };
        let events: Vec<Result<ProviderEvent, ProviderError>> = vec![
            Ok(GatewayEvent::Started(response).into()),
            Ok(GatewayEvent::ProviderCost(
                ProviderReportedCost::from_usd_ticks(known_charge().scaled()).unwrap(),
            )
            .into()),
            terminal,
        ];
        Ok(ProviderStream::new(
            metadata,
            futures::stream::iter(events),
            (),
        ))
    }
}

fn charged_service(
    admissions: Arc<Admissions>,
    budget: Option<Arc<Budget>>,
    fail: bool,
) -> DefaultExecutionService {
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::new([Arc::new(ChargedProvider {
            fail,
            ..Default::default()
        }) as Arc<dyn Provider>])
        .unwrap(),
        admissions,
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    match budget {
        Some(budget) => service.with_budget(budget),
        None => service,
    }
}

async fn consume_charged_prefix(session: &mut dyn ExecutionSession) {
    let first = session.next_event().await.unwrap().unwrap();
    assert_eq!(
        first.commit_requirement(),
        CommitRequirement::CommitBeforeDelivery
    );
    session.commit_downstream(None).await.unwrap();
    let cost = session.next_event().await.unwrap().unwrap();
    assert_eq!(
        cost.commit_requirement(),
        CommitRequirement::AlreadyCommitted
    );
}

fn assert_cleanup_completed(admissions: &Admissions, budget: &Budget, request_id: &ModelRequestId) {
    assert!(!admissions.active.load(Ordering::SeqCst));
    assert_eq!(admissions.releases.load(Ordering::SeqCst), 1);
    assert_eq!(budget.settlements.load(Ordering::SeqCst), 1);
    let charges = budget.charges.lock().unwrap();
    assert_eq!(charges.len(), 1);
    assert_eq!(charges[0].request_id, *request_id);
    assert_eq!(charges[0].key_id.as_str(), "key_start_test");
    assert_eq!(charges[0].amount_usd, known_charge());
}

#[test]
fn normal_completion_settles_known_charge_once_for_all_transports() {
    block_on(async {
        for transport in [
            ClientTransport::HttpJson,
            ClientTransport::HttpSse,
            ClientTransport::WebSocket,
        ] {
            let admissions = Arc::new(Admissions::default());
            let budget = Arc::new(Budget {
                active: admissions.active.clone(),
                ..Default::default()
            });
            let service = charged_service(admissions.clone(), Some(budget.clone()), false);
            let mut started = service.start(request(&service, transport)).await.unwrap();
            if transport == ClientTransport::HttpJson {
                assert_eq!(
                    started.session.collect_uncommitted().await.unwrap().len(),
                    3
                );
                assert!(!started.session.is_finalized());
                started.session.commit_downstream(Some(200)).await.unwrap();
            } else {
                consume_charged_prefix(started.session.as_mut()).await;
                assert!(started.session.next_event().await.unwrap().is_some());
            }
            assert!(started.session.next_event().await.unwrap().is_none());
            assert!(started.session.is_finalized());
            started.session.detach_finalize().await;
            assert_cleanup_completed(&admissions, &budget, &started.request_id);
        }
    });
}

#[test]
fn settlement_survives_cancelled_event_wait_and_resumes_without_restart() {
    block_on(async {
        for fail in [false, true] {
            let admissions = Arc::new(Admissions::default());
            let (release, gate) = oneshot::channel();
            let budget = Arc::new(Budget {
                active: admissions.active.clone(),
                settlement_gate: Mutex::new(Some(gate)),
                ..Default::default()
            });
            let service = charged_service(admissions.clone(), Some(budget.clone()), fail);
            let mut started = service
                .start(request(&service, ClientTransport::WebSocket))
                .await
                .unwrap();
            consume_charged_prefix(started.session.as_mut()).await;
            if !fail {
                assert!(started.session.next_event().await.unwrap().is_some());
            }
            for _ in 0..2 {
                let mut terminal = started.session.next_event();
                assert!(futures::poll!(terminal.as_mut()).is_pending());
                drop(terminal);
                assert!(!started.session.is_finalized());
                assert_eq!(budget.settlements.load(Ordering::SeqCst), 1);
                assert!(budget.charges.lock().unwrap().is_empty());
                assert_eq!(admissions.releases.load(Ordering::SeqCst), 0);
            }
            release
                .send(())
                .expect("the original settlement is still alive");
            assert!(started.session.next_event().await.unwrap().is_none());
            assert!(started.session.is_finalized());
            started.session.detach_finalize().await;
            assert_cleanup_completed(&admissions, &budget, &started.request_id);
        }
    });
}

#[test]
fn detached_cleanup_settles_cancelled_execution_and_resumes_existing_settlement() {
    block_on(async {
        for cancel_event_wait in [false, true] {
            let admissions = Arc::new(Admissions::default());
            let (release, gate) = oneshot::channel();
            let budget = Arc::new(Budget {
                active: admissions.active.clone(),
                settlement_gate: Mutex::new(Some(gate)),
                ..Default::default()
            });
            let service = charged_service(admissions.clone(), Some(budget.clone()), false);
            let mut started = service
                .start(request(&service, ClientTransport::WebSocket))
                .await
                .unwrap();
            consume_charged_prefix(started.session.as_mut()).await;
            if cancel_event_wait {
                started.session.cancel();
                let mut terminal = started.session.next_event();
                assert!(futures::poll!(terminal.as_mut()).is_pending());
                drop(terminal);
                assert!(!started.session.is_finalized());
            }
            let mut detached = started.session.detach_finalize();
            assert!(futures::poll!(detached.as_mut()).is_pending());
            assert_eq!(budget.settlements.load(Ordering::SeqCst), 1);
            assert_eq!(admissions.releases.load(Ordering::SeqCst), 0);
            release
                .send(())
                .expect("detached cleanup retains settlement");
            detached.await;
            assert_cleanup_completed(&admissions, &budget, &started.request_id);
        }
    });
}

#[test]
fn cancelled_release_resumes_without_repeating_settlement_with_or_without_budget() {
    block_on(async {
        for with_budget in [false, true] {
            let (release, gate) = oneshot::channel();
            let admissions = Arc::new(Admissions {
                release_gate: Mutex::new(Some(gate)),
                ..Default::default()
            });
            let budget = Arc::new(Budget {
                active: admissions.active.clone(),
                ..Default::default()
            });
            let service = charged_service(
                admissions.clone(),
                with_budget.then(|| budget.clone()),
                false,
            );
            let mut started = service
                .start(request(&service, ClientTransport::HttpSse))
                .await
                .unwrap();
            consume_charged_prefix(started.session.as_mut()).await;
            assert!(started.session.next_event().await.unwrap().is_some());
            let mut terminal = started.session.next_event();
            assert!(futures::poll!(terminal.as_mut()).is_pending());
            drop(terminal);
            assert!(!started.session.is_finalized());
            assert!(admissions.active.load(Ordering::SeqCst));
            assert_eq!(
                budget.settlements.load(Ordering::SeqCst),
                usize::from(with_budget)
            );
            let mut detached = started.session.detach_finalize();
            assert!(futures::poll!(detached.as_mut()).is_pending());
            assert_eq!(admissions.releases.load(Ordering::SeqCst), 1);
            release
                .send(())
                .expect("the original release is still alive");
            detached.await;
            if with_budget {
                assert_cleanup_completed(&admissions, &budget, &started.request_id);
            } else {
                assert!(!admissions.active.load(Ordering::SeqCst));
                assert_eq!(admissions.releases.load(Ordering::SeqCst), 1);
                assert!(budget.charges.lock().unwrap().is_empty());
            }
        }
    });
}

#[test]
fn cancelled_buffered_commit_keeps_settlement_for_detached_cleanup() {
    block_on(async {
        let admissions = Arc::new(Admissions::default());
        let (release, gate) = oneshot::channel();
        let budget = Arc::new(Budget {
            active: admissions.active.clone(),
            settlement_gate: Mutex::new(Some(gate)),
            ..Default::default()
        });
        let service = charged_service(admissions.clone(), Some(budget.clone()), false);
        let mut started = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        assert_eq!(
            started.session.collect_uncommitted().await.unwrap().len(),
            3
        );
        let mut commit = started.session.commit_downstream(Some(200));
        assert!(futures::poll!(commit.as_mut()).is_pending());
        drop(commit);
        assert!(!started.session.is_finalized());
        release
            .send(())
            .expect("buffered commit retains settlement");
        started.session.detach_finalize().await;
        assert_cleanup_completed(&admissions, &budget, &started.request_id);
    });
}

#[test]
fn settlement_failure_keeps_provider_error_and_releases_concurrency_once() {
    block_on(async {
        for fail_settlement in [false, true] {
            let admissions = Arc::new(Admissions::default());
            let budget = Arc::new(Budget {
                active: admissions.active.clone(),
                fail_settlement,
                ..Default::default()
            });
            let service = charged_service(admissions.clone(), Some(budget.clone()), true);
            let mut started = service
                .start(request(&service, ClientTransport::WebSocket))
                .await
                .unwrap();
            consume_charged_prefix(started.session.as_mut()).await;
            assert!(matches!(
                started.session.next_event().await,
                Err(EngineError::Provider(error)) if error.kind() == ProviderErrorKind::InvalidRequest
            ));
            assert!(started.session.is_finalized());
            started.session.detach_finalize().await;
            // Store 端口已接管精确费用后，结算错误不能改写 Provider 错误或触发第二次结算。
            assert_cleanup_completed(&admissions, &budget, &started.request_id);
        }
    });
}

use async_trait::async_trait;
use bytes::Bytes;
use futures::{channel::oneshot, executor::block_on, future::BoxFuture};
use gateway_core::account::{
    AccountCandidate, AccountEligibilityPolicy, AccountModelAccess, AccountModelAccessMode,
    AccountRuntimeSignals, AccountSelectionContext, AccountSelectionPolicy, AccountWeight,
    CredentialRevision, CredentialState, ProviderAccount, ProviderAccountId, QuotaState,
    RotationStrategy,
};
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRequest, ClientAdmissionRestoreResult,
};
use gateway_core::engine::authentication::{
    ClientAuthenticationRequest, FrontendAuthenticationDecision, FrontendAuthenticationError,
    FrontendAuthenticationExtensionIndex, FrontendAuthenticationPlan,
};
use gateway_core::engine::continuation::{
    NativeContinuationPin, NativeContinuationPort, NativeContinuationStoreError, PreviousResponseId,
};
use gateway_core::engine::execution::{
    ClientApiKeyUsageSink, ClientKeyVerifier, ClientTransport, DefaultExecutionService,
    ExecutionRequestMetadata, ExecutionService, ExecutionSession, StartExecution,
    StartProviderExecution,
};
use gateway_core::engine::nested::NestedModelExecutionRequest;
use gateway_core::engine::observation::{
    RequestObservation, RequestObservationOutcome, RequestObserverExtensionIndex,
    RequestObserverPlan, WebSocketResponseObservation,
};
use gateway_core::engine::policy::{
    AccountPolicyError, AccountScheduleDecision, AccountScheduleInput, ModelRouteDecision,
    ModelRouteInput, RequestPolicyExtensionIndex, RequestPolicyFault, RequestPolicyPlan,
};
use gateway_core::engine::probe::{AccountProbe, AccountProbeErrorSource, AccountProbeRequest};
use gateway_core::engine::provider::{
    NativeResponseTranslator, Provider, ProviderCallMetadata, ProviderRegistry, ProviderRequest,
    ProviderRequestObservation, ProviderStream,
};
use gateway_core::engine::{
    AttemptContext, AttemptRecord, CommitRequirement, EngineError, ExecutionOutcome,
    ExecutionStore, IntermediateFailure, ModelRequestFinalization, ModelRequestId, NewModelRequest,
    ProbeFailure, RecoveryReport,
};
use gateway_core::error::{
    ClientVisibleUpstreamResponse, GatewayErrorKind, ProviderError, ProviderErrorKind, StoreError,
    StoreErrorKind,
};
use gateway_core::event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta};
use gateway_core::metering::{
    CostEstimateStatus, CostSource, Decimal, ProviderReportedCost, Usage,
};
use gateway_core::operation::{
    GenerateRequest, ImageRequest, ImageRequestKind, Operation, OperationKind, ProtocolPayload,
    ProviderHttpMethod, ProviderHttpRequest, RawHttpPayload, RawJsonPayload, TokenCountRequest,
};
use gateway_core::policy::{ClientApiKeyId, ClientPolicy, PlaintextClientApiKey, RateLimits};
use gateway_core::routing::{
    AccountGroupId, ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities,
    ProviderCatalogGeneration, ProviderKind, ProviderModel, ProviderModelCapabilities,
    PublicModelId, RoutingGroupSnapshot, RuntimeAccount, RuntimeAccountDirectory, RuntimeSnapshot,
    UpstreamModelId,
};
use gateway_core::runtime::{
    RuntimeSnapshotHandle,
    extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
};
use gateway_core::upstream::{UpstreamSendState, UpstreamTransport};
use serde_json::{Value, json};

#[test]
fn account_probe_should_not_write_to_the_persistent_execution_store() {
    let store = Arc::new(TrackingExecutionStore::default());
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(probe_snapshot()),
        store.clone(),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );

    let error = block_on(service.probe(
        AccountProbeRequest {
            account_id: ProviderAccountId::new("acct_probe").expect("account ID"),
            provider_kind: ProviderKind::new("openai").expect("provider kind"),
            upstream_model: UpstreamModelId::new("gpt-probe").expect("model ID"),
            operation: probe_operation(),
        },
        None,
    ))
    .expect_err("empty Provider registry should stop the probe after it starts");

    assert_eq!(error.kind(), GatewayErrorKind::NoAvailableProvider);
    assert_eq!(error.source(), AccountProbeErrorSource::Gateway);
    assert_eq!(error.send_state(), None);
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
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );

    let error = block_on(service.probe(
        AccountProbeRequest {
            account_id: ProviderAccountId::new("acct_probe").expect("account ID"),
            provider_kind: ProviderKind::new("openai").expect("provider kind"),
            upstream_model: UpstreamModelId::new("gpt-probe").expect("model ID"),
            operation: probe_operation(),
        },
        None,
    ))
    .expect_err("the provider rejects every probe");

    assert_eq!(error.kind(), GatewayErrorKind::UpstreamUnavailable);
    assert_eq!(error.source(), AccountProbeErrorSource::Upstream);
    assert_eq!(error.send_state(), Some(UpstreamSendState::NotSent));
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
fn provider_local_probe_failure_should_remain_distinct_from_upstream() {
    let store = Arc::new(TrackingExecutionStore::default());
    let providers = ProviderRegistry::new([Arc::new(LocalFailingProvider) as Arc<dyn Provider>])
        .expect("provider registry");
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(probe_snapshot()),
        store,
        providers,
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );

    let error = block_on(service.probe(
        AccountProbeRequest {
            account_id: ProviderAccountId::new("acct_probe").expect("account ID"),
            provider_kind: ProviderKind::new("openai").expect("provider kind"),
            upstream_model: UpstreamModelId::new("gpt-probe").expect("model ID"),
            operation: probe_operation(),
        },
        None,
    ))
    .expect_err("the Provider rejects the probe before sending it");

    assert_eq!(error.source(), AccountProbeErrorSource::Provider);
    assert_eq!(error.send_state(), Some(UpstreamSendState::NotSent));
    assert!(error.upstream_response().is_none());
}

#[test]
fn diagnostic_probe_does_not_apply_data_plane_account_model_policy() {
    let provider = ProviderKind::new("openai").expect("provider");
    let account_id = ProviderAccountId::new("acct_probe").expect("account");
    let snapshot = probe_snapshot().with_account_directory(Arc::new(RuntimeAccountDirectory::new(
        BTreeMap::from([(
            account_id.clone(),
            RuntimeAccount::new(provider.clone(), BTreeSet::new()).with_model_access(
                AccountModelAccess::new(
                    AccountModelAccessMode::Denylist,
                    vec!["gpt-probe".to_owned()],
                )
                .expect("model policy"),
            ),
        )]),
    )));
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::new([Arc::new(LocalFailingProvider) as Arc<dyn Provider>])
            .expect("providers"),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );

    let error = block_on(service.probe(
        AccountProbeRequest {
            account_id,
            provider_kind: provider,
            upstream_model: UpstreamModelId::new("gpt-probe").expect("model"),
            operation: probe_operation(),
        },
        None,
    ))
    .expect_err("the diagnostic reaches the selected Provider");

    assert_eq!(error.source(), AccountProbeErrorSource::Provider);
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
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );

    let error = block_on(service.probe(
        AccountProbeRequest {
            account_id: ProviderAccountId::new("acct_probe").expect("account ID"),
            provider_kind: ProviderKind::new("openai").expect("provider kind"),
            upstream_model: UpstreamModelId::new("gpt-probe").expect("model ID"),
            operation: probe_operation(),
        },
        None,
    ))
    .expect_err("the provider error must survive observation failure");

    assert_eq!(error.kind(), GatewayErrorKind::UpstreamUnavailable);
    assert!(!store.touched.load(Ordering::SeqCst));
    assert!(store.probe_failures().is_empty());
}

struct FailingProvider;

struct NativeCatalogProvider {
    fail: bool,
}

#[async_trait]
impl Provider for NativeCatalogProvider {
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
    async fn query_client_model_catalog(
        &self,
        scope: &FrozenAccountScope,
        protocol: &str,
        version: &str,
    ) -> Result<
        Option<Vec<gateway_core::routing::ProviderModelDescriptor>>,
        gateway_core::routing::ProviderCatalogUnavailable,
    > {
        assert!(scope.allows(&ProviderAccountId::new("acct_start").expect("account")));
        assert!(!scope.allows(&ProviderAccountId::new("acct_other").expect("account")));
        assert_eq!((protocol, version), ("codex", "0.154.0"));
        if self.fail {
            return Err(gateway_core::routing::ProviderCatalogUnavailable);
        }
        Ok(Some(
            ["gpt-new", "gpt-start"]
                .into_iter()
                .map(|id| gateway_core::routing::ProviderModelDescriptor {
                    model: UpstreamModelId::new(id).expect("id"),
                    content: gateway_core::routing::ProviderModelContent::Native(
                        RawJsonPayload::new(
                            "codex",
                            serde_json::to_vec(&json!({"slug":id,"future":null}))
                                .expect("JSON")
                                .into(),
                        )
                        .expect("payload"),
                    ),
                })
                .collect(),
        ))
    }
    async fn execute(
        self: Arc<Self>,
        _: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        panic!("catalog must not use metered execution")
    }
}

#[test]
fn client_catalog_forwards_scope_maps_whole_objects_and_omits_unroutable_models() {
    #[derive(Debug)]
    struct Aliases(Vec<gateway_core::routing::ContributedModelAlias>);
    impl gateway_core::runtime::extensions::ExtensionSetLease for Aliases {
        fn is_ready(&self) -> bool {
            true
        }
        fn model_aliases(&self) -> &[gateway_core::routing::ContributedModelAlias] {
            &self.0
        }
    }
    for fail in [false, true] {
        let snapshot = start_snapshot()
            .with_model_mappings(BTreeMap::from([
                ("alias".to_owned(), "gpt-start".to_owned()),
                ("missing-alias".to_owned(), "unavailable-model".to_owned()),
            ]))
            .with_extensions(Some(ExtensionSetReference::new(
                gateway_core::runtime::extensions::ExtensionSetId::new("catalog-aliases".into())
                    .unwrap(),
                Arc::new(Aliases(vec![
                    gateway_core::routing::ContributedModelAlias {
                        owner: "catalog-plugin".into(),
                        id: PublicModelId::new("plugin-alias").unwrap(),
                        provider: ProviderKind::new("openai").unwrap(),
                        target: UpstreamModelId::new("gpt-start").unwrap(),
                    },
                ])),
            )));
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(snapshot),
            Arc::new(TrackingExecutionStore::default()),
            ProviderRegistry::new([Arc::new(NativeCatalogProvider { fail }) as Arc<dyn Provider>])
                .expect("registry"),
            Arc::new(UnusedAdmissions),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        );
        let client = service.authenticate("sk_start_test").expect("authenticate");
        let result = block_on(service.client_model_catalog(&client, "codex", "0.154.0"));
        if fail {
            assert!(
                result.is_err(),
                "native failure cannot use the global synthesized catalog"
            );
            continue;
        }
        let models = result.expect("models");
        let pairs = models
            .iter()
            .map(|entry| {
                let gateway_core::routing::PublicModelDescriptor::Native { model, payload } = entry
                else {
                    panic!("native required")
                };
                (model.as_str(), payload.body())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            pairs.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ["gpt-start", "alias", "plugin-alias"]
        );
        assert_eq!(pairs[0].1, pairs[1].1, "Core preserves alias payload bytes");
        assert_eq!(
            pairs[0].1, pairs[2].1,
            "插件别名使用相同原生对象，不伪造字段"
        );
    }
}

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
        self: Arc<Self>,
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

struct LocalFailingProvider;

#[async_trait]
impl Provider for LocalFailingProvider {
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
        self: Arc<Self>,
        _: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            UpstreamSendState::NotSent,
        ))
    }
}

struct ColdFailingProvider {
    requested_model: Option<PublicModelId>,
}

#[async_trait]
impl Provider for ColdFailingProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }

    fn request_observation(&self, _: &Operation, _: &ClientApiKeyId) -> ProviderRequestObservation {
        ProviderRequestObservation {
            requested_model: self.requested_model.clone(),
            ..Default::default()
        }
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(Vec::new())
    }

    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let metadata = ProviderCallMetadata::for_provider_endpoint(
            request.candidate().provider().clone(),
            ProviderAccountId::new("acct_usage").expect("account"),
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
        Arc::new(UnusedContinuation),
        usage.clone(),
    );

    service
        .authenticate("sk_usage_test")
        .expect("successful authentication");

    assert_eq!(usage.recorded(), vec!["key_usage_test".to_owned()]);
}

#[test]
fn client_key_verification_should_not_record_client_key_usage() {
    let usage = Arc::new(RecordingClientApiKeyUsage::default());
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(client_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        usage.clone(),
    );

    let key_id = service
        .verify_client_key("sk_usage_test")
        .expect("client key verification");

    assert_eq!(key_id.as_str(), "key_usage_test");
    assert!(usage.recorded().is_empty());
}

#[test]
fn request_verification_should_apply_entry_authentication_without_recording_key_usage() {
    block_on(async {
        let usage = Arc::new(RecordingClientApiKeyUsage::default());
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(client_snapshot()),
            Arc::new(TrackingExecutionStore::default()),
            ProviderRegistry::default(),
            Arc::new(UnusedAdmissions),
            Arc::new(UnusedContinuation),
            usage.clone(),
        );

        let client = service
            .verify_request(
                ClientAuthenticationRequest::bearer("sk_usage_test")
                    .expect("client authentication request"),
            )
            .await
            .expect("request verification");

        assert_eq!(client.policy().key_id().as_str(), "key_usage_test");
        assert!(usage.recorded().is_empty());
    });
}

#[test]
fn provider_endpoint_should_persist_its_real_v1_endpoint() {
    assert_provider_endpoint_observation(None);
    assert_provider_endpoint_observation(Some("gpt-image-2"));
}

fn assert_provider_endpoint_observation(model: Option<&str>) {
    let store = Arc::new(TrackingExecutionStore::default());
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(client_snapshot()),
        store.clone(),
        ProviderRegistry::new([Arc::new(ColdFailingProvider {
            requested_model: model.map(|model| PublicModelId::new(model).expect("model")),
        }) as Arc<dyn Provider>])
        .expect("provider registry"),
        Arc::new(UnusedAdmissions),
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
        upstream_model: None,
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
    assert_eq!(store.requested_models(), vec![model.map(str::to_owned)]);
    let upstream_models = store.upstream_models();
    assert!(!upstream_models.is_empty());
    assert!(upstream_models.iter().all(Option::is_none));
}

#[test]
fn known_catalog_should_reject_a_model_that_the_provider_did_not_publish() {
    let store = Arc::new(TrackingExecutionStore::default());
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        store.clone(),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
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

    assert_eq!(
        (error.kind(), error.client_message()),
        (
            GatewayErrorKind::ModelNotFound,
            "the requested model was not found in the provider catalogs available to this API key; check the model name",
        )
    );
    let rejections = store.entry_rejections.lock().unwrap();
    assert_eq!(rejections.len(), 1);
    assert_eq!(rejections[0].error.kind(), GatewayErrorKind::ModelNotFound);
    assert!(store.requests.lock().unwrap().is_empty());
}

#[test]
fn continuation_owned_by_another_client_api_key_should_fail_closed() {
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(start_snapshot()),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
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
    entry_rejections: Mutex<Vec<gateway_core::engine::EntryRejection>>,
    requests: Mutex<Vec<NewModelRequest>>,
    attempts: Mutex<Vec<AttemptRecord>>,
    finalizations: Mutex<Vec<ModelRequestFinalization>>,
    create_gate: Mutex<Option<oneshot::Receiver<()>>>,
    finalize_gate: Mutex<Option<oneshot::Receiver<()>>>,
    creates: AtomicUsize,
    finalizes: AtomicUsize,
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
        self.requests
            .lock()
            .expect("model requests lock")
            .iter()
            .map(|request| request.endpoint.clone())
            .collect()
    }

    fn requested_models(&self) -> Vec<Option<String>> {
        self.requests
            .lock()
            .expect("model requests lock")
            .iter()
            .map(|request| {
                request
                    .requested_model
                    .as_ref()
                    .map(|model| model.as_str().to_owned())
            })
            .collect()
    }

    fn upstream_models(&self) -> Vec<Option<String>> {
        self.attempts
            .lock()
            .expect("attempts lock")
            .iter()
            .map(|attempt| {
                attempt
                    .upstream_model_id
                    .as_ref()
                    .map(|model| model.as_str().to_owned())
            })
            .collect()
    }
}

#[async_trait]
impl ExecutionStore for TrackingExecutionStore {
    async fn record_entry_rejection(
        &self,
        rejection: gateway_core::engine::EntryRejection,
    ) -> Result<(), StoreError> {
        self.entry_rejections.lock().unwrap().push(rejection);
        Ok(())
    }

    async fn create_model_request(&self, request: NewModelRequest) -> Result<(), StoreError> {
        self.touch();
        self.creates.fetch_add(1, Ordering::SeqCst);
        let gate = self.create_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.await.expect("create model request gate");
        }
        self.requests
            .lock()
            .expect("model requests lock")
            .push(request);
        Ok(())
    }

    async fn record_attempt(&self, attempt: AttemptRecord) -> Result<(), StoreError> {
        self.touch();
        self.attempts.lock().expect("attempts lock").push(attempt);
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

    async fn finalize_model_request(
        &self,
        finalization: ModelRequestFinalization,
    ) -> Result<(), StoreError> {
        self.touch();
        self.finalizes.fetch_add(1, Ordering::SeqCst);
        let gate = self.finalize_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.await.expect("finalize model request gate");
        }
        self.finalizations
            .lock()
            .expect("finalizations lock")
            .push(finalization);
        Ok(())
    }

    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        self.touch();
        Ok(RecoveryReport::default())
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
    start_snapshot_with_policy(1, true, RateLimits::unlimited(), false)
}

fn start_snapshot_with_policy(
    revision: u64,
    enabled: bool,
    limits: RateLimits,
    disable_fast: bool,
) -> RuntimeSnapshot {
    let provider = ProviderKind::new("openai").expect("provider kind");
    let capabilities =
        ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(16_000));
    RuntimeSnapshot::new(
        ConfigRevision::new(revision).expect("config revision"),
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
            Arc::new(
                account_scope(&provider, "acct_start")
                    .as_ref()
                    .clone()
                    .with_disable_fast(disable_fast),
            ),
            enabled,
            limits,
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

#[derive(Default)]
struct BoundedAdmissions {
    active: Mutex<BTreeSet<ModelRequestId>>,
    granted: AtomicUsize,
    rpm_exhausted: AtomicBool,
    hold_acquisition: AtomicBool,
}

#[derive(Default)]
struct QueuedAccountProvider {
    waiting: gateway_core::concurrency::ConcurrencyWaitQueue<&'static str>,
}

#[async_trait]
impl Provider for QueuedAccountProvider {
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
        self: Arc<Self>,
        _: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let mut waiting = gateway_core::concurrency::CapacityWait::new(
            &self.waiting,
            gateway_core::concurrency::ConcurrencyQueuePolicy {
                max_waiting: 1,
                timeout: Duration::from_secs(5),
            },
            context.deadline(),
            context.concurrency_wait_budget(),
        );
        loop {
            waiting.wait(&["account"]).await.map_err(|error| {
                ProviderError::new(error.provider_kind(), UpstreamSendState::NotSent)
            })?;
        }
    }
}

#[test]
fn account_wait_inherits_the_budget_spent_during_client_admission() {
    use futures::FutureExt;
    block_on(async {
        let admissions = Arc::new(BoundedAdmissions::default());
        let snapshot = start_snapshot_with_policy(
            1,
            true,
            RateLimits {
                max_concurrency: 1,
                requests_per_minute: 0,
            },
            false,
        )
        .with_client_queue_policy(gateway_core::concurrency::ConcurrencyQueuePolicy {
            max_waiting: 1,
            timeout: Duration::from_secs(1),
        });
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(snapshot),
            Arc::new(TrackingExecutionStore::default()),
            ProviderRegistry::new(
                [Arc::new(QueuedAccountProvider::default()) as Arc<dyn Provider>],
            )
            .unwrap(),
            admissions.clone(),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
        );
        let running = service
            .start(request(&service, ClientTransport::HttpSse))
            .await
            .unwrap();
        let started_at = Instant::now();
        let mut queued = service.start(request(&service, ClientTransport::HttpSse));
        assert!(queued.as_mut().now_or_never().is_none());
        futures_timer::Delay::new(Duration::from_millis(200)).await;
        drop(running);
        let mut started = queued.await.unwrap();
        let error = started.session.collect_uncommitted().await.unwrap_err();
        assert!(
            matches!(
                error,
                EngineError::Provider(ref error)
                    if error.kind() == ProviderErrorKind::ConcurrencyQueueTimeout
            ),
            "{error:?}"
        );
        assert!(
            started_at.elapsed() < Duration::from_secs(3),
            "账号层不能重新获得自己的 5 秒预算，实际耗时 {:?}",
            started_at.elapsed()
        );
        assert!(admissions.active.lock().unwrap().is_empty());
    });
}

impl ClientAdmissionPort for BoundedAdmissions {
    fn abandon(&self, _: &ClientApiKeyId, id: &ModelRequestId) {
        self.active.lock().unwrap().remove(id);
    }

    fn admit(
        &self,
        request: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>> {
        Box::pin(async move {
            use gateway_core::engine::admission::ClientAdmissionRejection;
            if self.rpm_exhausted.load(Ordering::SeqCst) {
                return Ok(ClientAdmissionDecision::Rejected(
                    ClientAdmissionRejection::RateLimited,
                ));
            }
            {
                let mut active = self.active.lock().unwrap();
                if !request.allow_concurrency_acquire
                    || (request.limits.max_concurrency > 0
                        && active.len() as u64 >= request.limits.max_concurrency)
                {
                    return Ok(ClientAdmissionDecision::Rejected(
                        ClientAdmissionRejection::ConcurrencyLimited,
                    ));
                }
                active.insert(request.model_request_id);
            }
            self.granted.fetch_add(1, Ordering::SeqCst);
            if self.hold_acquisition.load(Ordering::SeqCst) {
                futures::future::pending::<()>().await;
            }
            Ok(ClientAdmissionDecision::Granted)
        })
    }

    fn release<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        id: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>> {
        Box::pin(async move { Ok(self.active.lock().unwrap().remove(id)) })
    }

    fn restore(
        &self,
        _: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        Box::pin(async { Ok(ClientAdmissionRestoreResult::default()) })
    }
}

fn queue_service(
    max_concurrency: u64,
    max_waiting: u32,
    timeout: Duration,
) -> (DefaultExecutionService, Arc<BoundedAdmissions>) {
    let admissions = Arc::new(BoundedAdmissions::default());
    let snapshot = start_snapshot_with_policy(
        1,
        true,
        RateLimits {
            max_concurrency,
            requests_per_minute: 0,
        },
        false,
    )
    .with_client_queue_policy(gateway_core::concurrency::ConcurrencyQueuePolicy {
        max_waiting,
        timeout,
    });
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::default(),
        admissions.clone(),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    (service, admissions)
}

#[test]
fn client_queue_allows_five_running_five_waiting_and_rejects_the_eleventh() {
    use futures::FutureExt;
    block_on(async {
        let (service, admissions) = queue_service(5, 5, Duration::from_secs(2));
        let mut running = Vec::new();
        for _ in 0..5 {
            running.push(
                service
                    .start(request(&service, ClientTransport::HttpSse))
                    .await
                    .unwrap(),
            );
        }
        let mut waiting = Vec::new();
        for _ in 0..5 {
            let mut future = service.start(request(&service, ClientTransport::HttpSse));
            assert!(future.as_mut().now_or_never().is_none());
            waiting.push(future);
        }
        let rejected = service
            .start(request(&service, ClientTransport::HttpSse))
            .await
            .err()
            .unwrap();
        assert_eq!(rejected.kind(), GatewayErrorKind::ConcurrencyQueueFull);
        assert_eq!(
            admissions.granted.load(Ordering::SeqCst),
            5,
            "waiting must not consume admission/RPM"
        );
        drop(running.remove(0));
        let first = waiting.remove(0).await.unwrap();
        assert_eq!(admissions.active.lock().unwrap().len(), 5);
        assert_eq!(admissions.granted.load(Ordering::SeqCst), 6);
        assert!(waiting[0].as_mut().now_or_never().is_none());
        drop((first, waiting, running));
        assert!(admissions.active.lock().unwrap().is_empty());
    });
}

#[test]
fn cancelled_waiter_releases_its_place_and_new_requests_do_not_overtake_fifo() {
    use futures::FutureExt;
    block_on(async {
        let (service, admissions) = queue_service(1, 2, Duration::from_secs(2));
        let running = service
            .start(request(&service, ClientTransport::WebSocket))
            .await
            .unwrap();
        let mut first = service.start(request(&service, ClientTransport::WebSocket));
        let mut cancelled = service.start(request(&service, ClientTransport::WebSocket));
        assert!(first.as_mut().now_or_never().is_none());
        assert!(cancelled.as_mut().now_or_never().is_none());
        drop(cancelled);
        drop(running);
        let mut later = service.start(request(&service, ClientTransport::WebSocket));
        assert!(
            later.as_mut().now_or_never().is_none(),
            "a newly freed slot belongs to the existing queue head"
        );
        let first = first.await.unwrap();
        assert_eq!(admissions.granted.load(Ordering::SeqCst), 2);
        drop(first);
        let later = later.await.unwrap();
        drop(later);
        assert!(admissions.active.lock().unwrap().is_empty());
    });
}

#[test]
fn queue_timeout_and_rpm_rejection_leave_no_new_admission() {
    block_on(async {
        let (service, admissions) = queue_service(1, 1, Duration::from_millis(20));
        let running = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .unwrap();
        let timeout = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .err()
            .unwrap();
        assert_eq!(timeout.kind(), GatewayErrorKind::ConcurrencyQueueTimeout);
        admissions.rpm_exhausted.store(true, Ordering::SeqCst);
        let limited = service
            .start(request(&service, ClientTransport::HttpJson))
            .await
            .err()
            .unwrap();
        assert_eq!(limited.kind(), GatewayErrorKind::RateLimited);
        assert_eq!(admissions.granted.load(Ordering::SeqCst), 1);
        drop(running);
    });
}

#[test]
fn cancelling_pending_admission_cleans_up_a_slot_acquired_before_the_reply() {
    use futures::FutureExt;
    let (service, admissions) = queue_service(1, 1, Duration::from_secs(2));
    admissions.hold_acquisition.store(true, Ordering::SeqCst);
    let mut pending = service.start(request(&service, ClientTransport::HttpSse));
    assert!(pending.as_mut().now_or_never().is_none());
    assert_eq!(admissions.active.lock().unwrap().len(), 1);
    drop(pending);
    assert!(admissions.active.lock().unwrap().is_empty());
}

#[test]
fn reused_websocket_client_gets_group_fast_policy_from_each_new_request_snapshot() {
    let snapshots = RuntimeSnapshotHandle::new(start_snapshot());
    let admissions = Arc::new(Admissions::default());
    let provider = Arc::new(ChargedProvider::default());
    let service = DefaultExecutionService::new(
        snapshots.clone(),
        Arc::new(TrackingExecutionStore::default()),
        ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).unwrap(),
        admissions,
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    let client = service.authenticate("sk_start_test").unwrap();
    for (revision, disable_fast) in [(2, true), (3, false)] {
        snapshots.publish(start_snapshot_with_policy(
            revision,
            true,
            RateLimits::unlimited(),
            disable_fast,
        ));
        let mut next = request(&service, ClientTransport::WebSocket);
        next.client = client.clone();
        let mut started = block_on(service.start(next)).unwrap();
        block_on(started.session.collect_uncommitted()).unwrap();
        block_on(started.session.detach_finalize());
    }
    assert_eq!(*provider.policies.lock().unwrap(), vec![true, false]);
}

#[test]
fn repeated_connection_failures_never_block_later_requests_for_the_provider() {
    let store = Arc::new(TrackingExecutionStore::default());
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(client_snapshot()),
        store.clone(),
        ProviderRegistry::new([Arc::new(ColdFailingProvider {
            requested_model: None,
        }) as Arc<dyn Provider>])
        .expect("provider registry"),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
    );
    for _ in 0..5 {
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
            upstream_model: None,
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

        let _error = block_on(started.session.collect_uncommitted())
            .expect_err("the cold provider stops execution after persistence");
    }
    assert_eq!(store.requests.lock().unwrap().len(), 5);
    assert!(store.entry_rejections.lock().unwrap().is_empty());
}
