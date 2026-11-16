//! 验证真实 OpenAI Provider 经 Core 重试收尾时保留上游错误与本地会话边界

use super::session_binding::turn_request;
use super::*;
use async_trait::async_trait;
use gateway_core::engine::provider::ProviderRegistry;
use gateway_core::engine::{
    AttemptCoordinator, AttemptRecord, EngineError, ExecutionStore, GatewayEngine,
    IntermediateFailure, ModelRequestFinalization, NewModelRequest, RecoveryReport,
};
use gateway_core::error::StoreError;
use gateway_core::routing::AccountRoutingSnapshot;

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

#[tokio::test]
async fn contributed_model_scope_survives_affinity_and_account_rotation() {
    use gateway_core::routing::ContributedModel;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Catalog(Vec<ContributedModel>);
    impl ExtensionSetLease for Catalog {
        fn is_ready(&self) -> bool {
            true
        }
        fn models(&self) -> &[ContributedModel] {
            &self.0
        }
    }

    for native_supports_tools in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let server = MockServer::start().await;
        let calls = AtomicUsize::new(0);
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(move |_: &wiremock::Request| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(429).set_body_json(json!({
                        "error":{"type":"rate_limit_error","message":"fixture rate limit"}
                    }))
                } else {
                    ResponseTemplate::new(200)
                        .set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream")
                }
            })
            .expect(2)
            .mount(&server)
            .await;
        let provider = provider_with_affinity_and_base_url(
            &store,
            Arc::new(MemorySessionAffinity::default()),
            server.uri(),
        );
        let Operation::Generate(generate) = turn_request("scope-session", "scope-turn") else {
            unreachable!()
        };
        let mut body = generate.protocol_payload().body().clone();
        if native_supports_tools {
            body.insert(
                "tools".into(),
                json!([{
                    "type":"function", "name":"fixture", "parameters":{"type":"object"}
                }]),
            );
        }
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", body)
                .unwrap()
                .with_context(generate.protocol_payload().context().clone()),
        ));
        // 先冻结 A 的会话亲和，再开放 B/C；后续选号不能借亲和绕过模型能力范围
        drop(
            provider
                .clone()
                .execute(
                    planned_request("openai", operation.clone()),
                    context("req_scope_seed", CancellationToken::new()),
                )
                .await
                .unwrap(),
        );
        for account in ["acct_subagent_a", "acct_subagent_b"] {
            create_account(&store, account).await;
        }
        let kind = ProviderKind::new("openai").unwrap();
        let capabilities = ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None);
        let models = if native_supports_tools {
            vec![ProviderModel::new(
                kind.clone(),
                UpstreamModelId::new("gpt-5.4").unwrap(),
                capabilities.clone().with_upstream_feature_validation(),
            )]
        } else {
            Vec::new()
        };
        // 适配专属模型只属于 B/C；同名原生模型支持 tools 时，适配账号 A 不支持
        let contributed_accounts = if native_supports_tools {
            vec!["acct_provider_contract"]
        } else {
            vec!["acct_subagent_a", "acct_subagent_b"]
        };
        let snapshot = RuntimeSnapshot::new(
            ConfigRevision::new(1).unwrap(),
            SettingsValues::new(3, "smart", Default::default(), None, None),
            vec![kind.clone()],
            models,
            Vec::new(),
        )
        .unwrap()
        .with_extensions(Some(ExtensionSetReference::new(
            ExtensionSetId::new("scope-test".into()).unwrap(),
            Arc::new(Catalog(vec![ContributedModel {
                owner: "scope-test".into(),
                id: PublicModelId::new("gpt-5.4").unwrap(),
                provider: kind,
                accounts: contributed_accounts
                    .into_iter()
                    .map(|id| ProviderAccountId::new(id).unwrap())
                    .collect(),
                capabilities,
                presentation: Default::default(),
            }])),
        )));
        let plan = snapshot
            .plan(
                &PublicModelId::new("gpt-5.4").unwrap(),
                &operation,
                contract_account_scope(),
                &RoutingContext::default(),
            )
            .unwrap();
        let providers =
            ProviderRegistry::new([provider as Arc<dyn gateway_core::engine::provider::Provider>])
                .unwrap();
        let coordinator =
            AttemptCoordinator::new(GatewayEngine::new(Arc::new(NoopExecutionStore), providers));
        let mut session = coordinator
            .start(
                execution_request("req_scope_rotation", &operation, Duration::from_secs(10)),
                operation,
                plan,
                None,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        session.collect_uncommitted().await.unwrap();
        session.commit_downstream(Some(200)).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let selected: BTreeSet<_> = requests
            .iter()
            .map(|request| request.headers["authorization"].to_str().unwrap())
            .collect();
        assert_eq!(
            selected,
            BTreeSet::from(["Bearer at-acct_subagent_a", "Bearer at-acct_subagent_b",]),
            "native_supports_tools={native_supports_tools}"
        );
    }
}

#[tokio::test]
async fn cyber_policy_rotation_preserves_upstream_response_and_next_request_stays_local() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let server = MockServer::start().await;
    let body = json!({"error":{"code":"cyber_policy","type":"invalid_request_error","message":"synthetic policy rejection"}});
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(&body))
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        server.uri(),
    );
    let providers =
        ProviderRegistry::new([provider as Arc<dyn gateway_core::engine::provider::Provider>])
            .unwrap();
    let coordinator =
        AttemptCoordinator::new(GatewayEngine::new(Arc::new(NoopExecutionStore), providers));
    let operation = turn_request("policy-session", "policy-turn");
    let plan = execution_plan(&operation);
    let request = |id| execution_request(id, &operation, Duration::from_secs(10));
    let mut session = coordinator
        .start(
            request("req_policy_first"),
            operation.clone(),
            plan.clone(),
            None,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let error = session
        .collect_uncommitted()
        .await
        .expect_err("policy rejection must surface");
    let EngineError::Provider(error) = error else {
        panic!("expected Provider error: {error:?}")
    };
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(error.upstream_status(), Some(400));
    assert_eq!(
        error.client_visible_upstream_error().unwrap().message(),
        "synthetic policy rejection"
    );
    let response = error
        .client_visible_upstream_response()
        .expect("original HTTP rejection must survive empty account selection");
    assert_eq!(
        serde_json::from_slice::<Value>(response.body()).unwrap(),
        body
    );
    assert_eq!(response.status(), 400);
    // 同一会话的新请求没有可恢复的上游失败，只返回本地策略提示
    let mut next = coordinator
        .start(
            request("req_policy_next"),
            operation.clone(),
            plan,
            None,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let EngineError::Provider(error) = next.collect_uncommitted().await.unwrap_err() else {
        panic!("expected local selection failure");
    };
    assert_eq!(error.kind(), ProviderErrorKind::SessionPolicyRestricted);
    assert!(error.retry_is_prohibited());
    assert_eq!(error.upstream_status(), None);
    assert!(error.client_visible_upstream_response().is_none());
    assert_eq!(
        error.client_visible_upstream_error().unwrap().code(),
        Some("cyber_policy")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn streaming_not_found_preserves_accounts_and_stops_gateway_retries() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let raw = json!({
        "type": "error", "status": 404,
        "error": {
            "type": "invalid_request_error", "code": null, "param": "input",
            "message": "Item with id 'rs_missing' not found. Items are not persisted when `store` is set to false. Try again with `store` set to true, or remove this item from your input."
        }
    })
    .to_string();
    let created = json!({
        "type": "response.created",
        "response": {"id": "resp_missing_item", "model": "gpt-5.4", "output": []}
    })
    .to_string();
    for websocket in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let requests = Arc::new(AtomicUsize::new(0));
        let upstream_requests = Arc::clone(&requests);
        let http = MockServer::start().await;
        let (base_url, server) = if websocket {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let created = created.clone();
            let raw = raw.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut ws = accept_codex_test_websocket(socket).await;
                    while let Some(Ok(message)) = ws.next().await {
                        match message {
                            Message::Text(_) => {
                                let count = upstream_requests.fetch_add(1, Ordering::SeqCst) + 1;
                                ws.send(Message::Text(created.clone().into()))
                                    .await
                                    .unwrap();
                                ws.send(Message::Ping(Bytes::new())).await.unwrap();
                                ws.send(Message::Text(raw.clone().into())).await.unwrap();
                                if count == 3 {
                                    return;
                                }
                            }
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                }
            });
            (base_url, Some(server))
        } else {
            let response = ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!("data: {created}\n\n: keepalive\n\ndata: {raw}\n\n"));
            Mock::given(method("POST"))
                .and(path("/codex/responses"))
                .respond_with(move |_: &wiremock::Request| {
                    upstream_requests.fetch_add(1, Ordering::SeqCst);
                    response.clone()
                })
                .mount(&http)
                .await;
            (http.uri(), None)
        };
        let provider = provider_with_base_url(&store, base_url);
        let providers =
            ProviderRegistry::new([provider as Arc<dyn gateway_core::engine::provider::Provider>])
                .unwrap();
        let coordinator =
            AttemptCoordinator::new(GatewayEngine::new(Arc::new(NoopExecutionStore), providers));
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                Map::from_iter([
                    ("model".to_owned(), json!("gpt-5.4")),
                    (
                        "input".to_owned(),
                        json!([{"type": "item_reference", "id": "rs_missing"}]),
                    ),
                    ("store".to_owned(), json!(false)),
                ]),
            )
            .unwrap()
            .with_context(Map::from_iter([(
                "use_websocket".to_owned(),
                json!(websocket),
            )])),
        ));
        let plan = execution_plan(&operation);
        // 重复请求必须继续使用同一可调度账号，不能把历史条目缺失累计为凭据失效
        for index in 1..=3 {
            let mut session = coordinator
                .start(
                    execution_request(
                        &format!("req_missing_item_{index}"),
                        &operation,
                        Duration::from_secs(5),
                    ),
                    operation.clone(),
                    plan.clone(),
                    None,
                    None,
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            let EngineError::Provider(error) =
                timeout(Duration::from_secs(5), session.collect_uncommitted())
                    .await
                    .unwrap()
                    .unwrap_err()
            else {
                panic!("expected the original upstream rejection");
            };
            assert_eq!(
                error.kind(),
                ProviderErrorKind::InvalidRequest,
                "websocket={websocket}"
            );
            assert_eq!(error.upstream_status(), Some(404));
            assert!(!error.replay_is_safe());
            assert!(error.retry_is_prohibited());
            assert_eq!(error.raw_upstream_error().unwrap().as_str(), raw);
            assert_eq!(requests.load(Ordering::SeqCst), index);
            let account = store.account("acct_provider_contract").unwrap();
            assert_eq!(account.credential_state(), CredentialState::Ready);
            assert_eq!(account.last_error_reason(), None);
        }
        if let Some(server) = server {
            timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
        }
    }
}

#[tokio::test]
async fn refused_connections_respect_the_same_rotation_limit_over_http_and_websocket() {
    let upstream = MockServer::start().await;
    for websocket in [false, true] {
        for (rotations, child) in [(0, false), (2, false), (2, true)] {
            if child && websocket {
                continue;
            }
            let store = Arc::new(MemoryAccountStore::default());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = listener.local_addr().unwrap();
            drop(listener);
            let proxy =
                gateway_core::account::OutboundProxy::parse(&format!("socks5h://{endpoint}"))
                    .unwrap();
            for id in ["acct_provider_contract", "acct_subagent_b"] {
                create_account(&store, id).await;
                store.set_egress(id, Some(proxy.clone()), None);
                if child {
                    store.set_oauth_transport(
                        id,
                        provider_openai::credential::ResponsesTransport::Http,
                    );
                }
            }
            let operation = if child {
                Operation::Generate(generate_with_session_context(
                    "root",
                    Some("child"),
                    Some(r#"{"subagent_kind":"thread_spawn"}"#),
                ))
            } else if websocket {
                capacity_websocket_operation()
            } else {
                http_generate_operation()
            };
            let leases = Arc::new(TestLeaseCoordinator::default());
            let provider = provider_with_affinity_and_base_url_and_leases(
                &store,
                Arc::new(MemorySessionAffinity::default()),
                upstream.uri(),
                Arc::clone(&leases),
            );
            let providers = ProviderRegistry::new([
                provider as Arc<dyn gateway_core::engine::provider::Provider>
            ])
            .unwrap();
            let coordinator = AttemptCoordinator::new(GatewayEngine::new(
                Arc::new(NoopExecutionStore),
                providers,
            ));
            let settings = SettingsValues::new(3, "smart", Default::default(), None, None)
                .with_max_account_rotations(rotations);
            let plan = execution_plan_with_settings(&operation, settings);
            let mut session = coordinator
                .start(
                    execution_request("req_refused", &operation, Duration::from_secs(15)),
                    operation,
                    plan,
                    None,
                    None,
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            let result = session.collect_uncommitted().await;
            assert!(
                matches!(result, Err(EngineError::Provider(_))),
                "ws={websocket}, rotations={rotations}: {result:?}"
            );
            let requests = leases.requests.lock().unwrap();
            let accounts: Vec<_> = requests.iter().map(|r| r.account_id()).collect();
            if rotations == 0 {
                assert!((3..=4).contains(&accounts.len()), "{accounts:?}");
            } else {
                assert!(
                    matches!(&result, Err(EngineError::Provider(error)) if error.unreachable_proxy().is_some()),
                    "{result:?}"
                );
                assert_eq!(
                    accounts.len(),
                    2,
                    "ws={websocket}: {accounts:?}, {result:?}"
                );
            }
            assert!(
                accounts.iter().all(|a| *a == accounts[0]),
                "ws={websocket}: {accounts:?}"
            );
        }
    }
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unreachable_proxy_is_skipped_across_rotations_and_reconsidered_on_the_next_request() {
    use gateway_core::account::OutboundProxy;

    const A: &str = "acct_provider_contract";
    const B: &str = "acct_subagent_a";
    const C: &str = "acct_subagent_b";
    const D: &str = "acct_affinity";
    let store = Arc::new(MemoryAccountStore::default());
    let upstream = MockServer::start().await;
    // 关闭监听以产生真实的连接拒绝，下一请求让同一入口恢复服务
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    for (id, weight) in [(A, 100), (B, 90), (C, 80), (D, 70)] {
        create_account(&store, id).await;
        store.set_scheduling(id, None, AccountWeight::new(weight).unwrap());
        if matches!(id, A | B) {
            let proxy = OutboundProxy::parse(&format!("http://{id}@{addr}")).unwrap();
            store.set_egress(id, Some(proxy), None);
        }
    }
    let rate_limit_then_success = || {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        Mock::given(method("POST"))
            .respond_with(move |_: &wiremock::Request| {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(429).set_body_json(json!({
                        "error":{"type":"rate_limit_error","message":"fixture rate limit"}
                    }))
                } else {
                    ResponseTemplate::new(200)
                        .set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream")
                }
            })
            .expect(2)
    };
    rate_limit_then_success().mount(&upstream).await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        upstream.uri(),
        Arc::clone(&leases),
    );
    let providers =
        ProviderRegistry::new([provider as Arc<dyn gateway_core::engine::provider::Provider>])
            .unwrap();
    let coordinator =
        AttemptCoordinator::new(GatewayEngine::new(Arc::new(NoopExecutionStore), providers));
    let run = async |id: &'static str| {
        // 每个请求启动独立会话，避免已有的会话亲和优先于测试设置的账号权重
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                Map::from_iter([
                    ("model".into(), json!("gpt-5.4")),
                    ("input".into(), json!("hello")),
                    ("prompt_cache_key".into(), json!(id)),
                ]),
            )
            .unwrap()
            .with_context(Map::from_iter([("use_websocket".into(), json!(false))])),
        ));
        let plan = execution_plan_with_settings(
            &operation,
            SettingsValues::new(3, "smart", Default::default(), None, None)
                .with_max_account_rotations(2),
        );
        let mut session = coordinator
            .start(
                execution_request(id, &operation, Duration::from_secs(15)),
                operation,
                plan,
                None,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let result = session.collect_uncommitted().await;
        let accounts = std::mem::take(&mut *leases.requests.lock().unwrap())
            .iter()
            .map(|r| r.account_id().as_str().to_owned())
            .collect::<Vec<_>>();
        assert!(result.is_ok(), "{id}: {result:?}, accounts={accounts:?}");
        accounts
    };
    // C 的限流再次触发换号，B 权重更高也不能换回已失败的入口
    assert_eq!(run("req_proxy_skip").await, [A, A, C, D]);
    let recovered = MockServer::builder()
        .listener(std::net::TcpListener::bind(addr).unwrap())
        .start()
        .await;
    rate_limit_then_success().mount(&recovered).await;
    // 新请求可重新使用 A；这次 A 只被限流，同一入口的 B 仍是最优账号
    assert_eq!(run("req_proxy_recovered").await, [A, B]);
}

#[tokio::test]
async fn stalled_http_proxy_retries_same_account_before_using_reserved_rotation_budget() {
    stalled_proxy_recovery(false).await;
}

#[tokio::test]
async fn stalled_pooled_websocket_and_http_leave_one_opening_for_rotation() {
    stalled_proxy_recovery(true).await;
}

async fn stalled_proxy_recovery(websocket: bool) {
    use provider_openai::credential::ResponsesTransport;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    create_account_with_enabled(&store, "acct_subagent_b", false).await;
    if !websocket {
        store.set_oauth_transport("acct_subagent_a", ResponsesTransport::Http);
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    store.set_egress(
        "acct_subagent_a",
        Some(
            gateway_core::account::OutboundProxy::parse(&format!(
                "socks5h://{}",
                listener.local_addr().unwrap()
            ))
            .unwrap(),
        ),
        None,
    );
    let openings = Arc::new(AtomicUsize::new(0));
    let proxy_openings = Arc::clone(&openings);
    let proxy_store = Arc::clone(&store);
    let proxy = tokio::spawn(async move {
        let mut sockets = Vec::new();
        loop {
            sockets.push(listener.accept().await.unwrap().0);
            if proxy_openings.fetch_add(1, Ordering::SeqCst) == 0 {
                // A 已选定后再开放 B，避免测试依赖选号权重或随机顺序
                proxy_store
                    .set_enabled(&ProviderAccountId::new("acct_subagent_b").unwrap(), true)
                    .await
                    .unwrap();
            }
        }
    });
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let providers =
        ProviderRegistry::new([provider as Arc<dyn gateway_core::engine::provider::Provider>])
            .unwrap();
    let coordinator =
        AttemptCoordinator::new(GatewayEngine::new(Arc::new(NoopExecutionStore), providers));
    let operation = Operation::Generate(generate_with_session_context(
        "budget-session",
        Some("budget-session"),
        None,
    ));
    let plan = execution_plan(&operation);
    let started = std::time::Instant::now();
    let mut session = coordinator
        .start(
            execution_request("req_reserved_rotation", &operation, Duration::from_secs(40)),
            operation.clone(),
            plan,
            None,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(30), session.collect_uncommitted()).await;
    let elapsed = started.elapsed();
    proxy.abort();
    let _ = proxy.await;
    result
        .expect("rotation must finish within the original startup window")
        .unwrap();
    let expected_openings = if websocket { 3 } else { 2 };
    assert_eq!(openings.load(Ordering::SeqCst), expected_openings);
    assert!(
        elapsed >= Duration::from_secs(20),
        "A must get its own retry: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(28),
        "B must retain useful time: {elapsed:?}"
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "B must skip a fresh WS opening");
    assert_eq!(requests[0].method, "POST");
    eprintln!(
        "stalled proxy: websocket={websocket}, A openings={expected_openings}, one B payload, elapsed={elapsed:?}"
    );
}

fn execution_plan(operation: &Operation) -> gateway_core::routing::RoutingPlan {
    execution_plan_with_settings(
        operation,
        SettingsValues::new(3, "smart", Default::default(), None, None),
    )
}

fn execution_plan_with_settings(
    operation: &Operation,
    settings: SettingsValues,
) -> gateway_core::routing::RoutingPlan {
    let kind = ProviderKind::new("openai").unwrap();
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        settings.with_request_location(global_request_location(), true),
        vec![kind.clone()],
        vec![ProviderModel::new(
            kind,
            UpstreamModelId::new("gpt-5.4").unwrap(),
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), Some(32_000))
                .with_upstream_feature_validation(),
        )],
        Vec::new(),
    )
    .unwrap();
    snapshot
        .plan(
            &PublicModelId::new("gpt-5.4").unwrap(),
            operation,
            contract_account_scope(),
            &RoutingContext::default(),
        )
        .unwrap()
}

fn execution_request(id: &str, operation: &Operation, timeout: Duration) -> NewModelRequest {
    NewModelRequest {
        admission_decision_ms: None,
        id: ModelRequestId::new(id).unwrap(),
        client_api_key_name_snapshot: None,
        client_api_key_ref: ClientApiKeyId::new("key_openai_contract").unwrap(),
        config_revision: ConfigRevision::new(1).unwrap(),
        routing: AccountRoutingSnapshot::all(),
        protocol: "openai".to_owned(),
        operation: operation.kind(),
        endpoint: "responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model: Some(PublicModelId::new("gpt-5.4").unwrap()),
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
        deadline_at: (SystemTime::now() + timeout).into(),
    }
}
