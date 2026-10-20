//! 验证混合中间件挂载保持独立顺序与各次调用的续接状态

use super::*;
use gateway_core::{
    engine::middleware::{http, service, websocket},
    middleware::compose,
};
use http_body_util::BodyExt as _;

struct Sender;
impl websocket::Sender for Sender {
    fn send(&self, _: websocket::Message) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async { panic!("forwarded messages use next") })
    }
}

#[tokio::test]
async fn mixed_mounts_keep_independent_order_and_fresh_continuations() {
    let records_dir = tempfile::tempdir().unwrap();
    let records = records_dir.path().join("calls");
    let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
    let package = crate::support::package_with_contributions(
        &worker,
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![
                Stage::Http,
                Stage::WebSocket,
                Stage::Service,
                Stage::Request,
                Stage::Attempt,
            ],
            vec![
                "http".into(),
                "websocket".into(),
                "service".into(),
                "openai".into(),
            ],
            vec![
                "http".into(),
                "websocket".into(),
                "service".into(),
                "openai".into(),
            ],
        )]),
    );
    let (_cache, runtime) = setup_package(
        [
            ("first", vec![("http", 30), ("websocket", -10)]),
            ("second", vec![("http", -10), ("websocket", 30)]),
            (
                "other",
                vec![("service", 0), ("request", 0), ("attempt", 0)],
            ),
        ]
        .into_iter()
        .map(|(id, mounts)| InstanceFixture {
            id,
            configuration: serde_json::json!({"mode":"mixed_mounts","records":records}),
            bindings: mounts
                .into_iter()
                .map(|(stage, order)| {
                    binding(
                        MIDDLEWARE_CONTRIBUTION,
                        stage,
                        order,
                        PluginFailurePolicy::Reject,
                    )
                })
                .collect(),
        })
        .collect(),
        package,
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime
        .execution_registry()
        .middleware(&generation)
        .unwrap();
    assert!(plan.has_http() && plan.has_websocket() && plan.has_service());
    let extensions = ExtensionCallScope::default().with_request(Arc::new(
        gateway_core::engine::extensions::RequestContext {
            method: "POST".into(),
            uri: "/v1/responses?fixture=1".into(),
            http_version: "HTTP/2.0".into(),
            headers: vec![
                MiddlewareHeader::new("x-repeat", Bytes::from_static(b"one")),
                MiddlewareHeader::new("x-repeat", Bytes::from_static(b"two")),
            ],
            peer_addr: Some("192.0.2.1:42000".parse().unwrap()),
            client_ip: Some("203.0.113.7".parse().unwrap()),
        },
    ));
    let mut expected = Vec::new();
    for direction in [
        websocket::Direction::Incoming,
        websocket::Direction::Outgoing,
    ] {
        let response = plan
            .handle_http(
                http::Context {
                    plugin_instance_id: None,
                    request_id: "request".into(),
                    call_id: "call".into(),
                    parent_call_id: None,
                    extensions: extensions.clone(),
                    cancellation: CancellationToken::new(),
                    plan: Some(plan.clone()),
                },
                http::Request::new(http::empty_body()),
                compose(Vec::new(), |request: http::Request| {
                    Box::pin(async { Ok(http::Response::new(request.into_body())) })
                }),
            )
            .await
            .unwrap();
        assert!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );
        expected.extend([
            "enter:Http:second",
            "enter:Http:first",
            "exit:Http:first",
            "exit:Http:second",
        ]);
        let message = plan
            .handle_websocket(
                websocket::Context {
                    extensions: extensions.clone(),
                    plan: Some(plan.clone()),
                    connection_id: "connection".into(),
                    direction,
                    headers: Arc::from([]),
                    cancellation: CancellationToken::new(),
                    sender: Arc::new(Sender),
                },
                websocket::Message {
                    kind: websocket::Kind::Binary,
                    payload: Bytes::from_static(b"payload"),
                },
                compose(Vec::new(), |message| Box::pin(async { Ok(Some(message)) })),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.payload, "payload");
        expected.extend([
            "enter:WebSocket:first",
            "enter:WebSocket:second",
            "exit:WebSocket:second",
            "exit:WebSocket:first",
        ]);
    }
    let value = plan
        .handle_service(
            service::Context {
                operation: "fixture.operation",
                request_id: "request".into(),
                call_id: "service".into(),
                parent_call_id: None,
                extensions: extensions.clone(),
                cancellation: CancellationToken::new(),
                plan: plan.clone(),
            },
            serde_json::json!({"value":42}),
            compose(Vec::new(), |value| Box::pin(async { Ok(value) })),
        )
        .await
        .unwrap();
    assert_eq!(value["value"], 42);
    expected.extend(["enter:Service:other", "exit:Service:other"]);
    assert_eq!(
        std::fs::read_to_string(records)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected
    );
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn data_plane_http_callbacks_keep_the_frozen_body_budget_across_publications() {
    use gateway_core::{
        engine::policy::RetryFacts,
        error::ProviderErrorKind,
        policy::ClientPolicy,
        routing::{RuntimeSnapshot, request_settings::RequestSettings},
        runtime::RuntimeSnapshotHandle,
        settings::SettingsValues,
        upstream::UpstreamSendState,
    };

    let snapshot = |limit: u64, revision| {
        RuntimeSnapshot::new(
            ConfigRevision::new(revision).unwrap(),
            SettingsValues::new(1, "smart", Default::default(), None, None)
                .with_responses_max_decompressed_body_bytes(limit),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
    };
    for body_bytes in [128, 129] {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let records = tempfile::tempdir().unwrap();
        let marker = records.path().join("http-results.jsonl");
        let (cache, runtime) = setup(vec![InstanceFixture {
            id: "frozen-limits",
            configuration: serde_json::json!({
                "http_probe_methods":["middleware.handle","policy.route_model","policy.schedule_account","policy.retry_decision"],
                "http_probe_url":server.uri(),"http_probe_body_bytes":body_bytes,"http_probe_marker":marker,
                "retry_decision":{"decision":"delegate"},
                "route_decision":{"decision":"unhandled"},
                "schedule_decision":{"decision":"delegate"},
            }),
            bindings: vec![
                binding(MIDDLEWARE_CONTRIBUTION,"request",0,PluginFailurePolicy::Reject),
                binding(MIDDLEWARE_CONTRIBUTION,"attempt",0,PluginFailurePolicy::Reject),
                binding(MODEL_ROUTER_CONTRIBUTION,"routing",0,PluginFailurePolicy::Reject),
                binding(SCHEDULER_CONTRIBUTION,"scheduling",0,PluginFailurePolicy::Reject),
                binding("test.example.retryPolicy","retry",0,PluginFailurePolicy::Delegate),
            ],
        }]).await;
        let snapshots = RuntimeSnapshotHandle::new(snapshot(128, 1));
        runtime.bind_snapshots(&snapshots).unwrap();
        let settings = RequestSettings::new(snapshots.acquire().unwrap()).with_execution(
            &ClientPolicy::new(
                ClientApiKeyId::new("key-middleware").unwrap(),
                gateway_core::policy::PlaintextClientApiKey::new("frozen-budget-fixture").unwrap(),
                Arc::new(gateway_core::routing::FrozenAccountScope::new(
                    Arc::default(),
                    gateway_core::routing::ClientRoutingScope::all_accounts(),
                )),
                true,
                Default::default(),
            ),
            Some(60_000),
        );
        let generation = prepare(&runtime).await;
        let plan = runtime
            .execution_registry()
            .middleware(&generation)
            .unwrap();
        let policy = policy_context(&runtime, generation.clone(), "req_frozen_limits")
            .with_request_body_limit(128);
        for (revision, current_limit) in [(2, 64), (3, 256)] {
            snapshots.publish(snapshot(current_limit, revision));
            for attempt in [false, true] {
                let mut request = MiddlewareRequest::new(
                    "openai",
                    middleware_headers(),
                    Bytes::from_static(br#"{"input":"hello"}"#),
                );
                let context = if attempt {
                    attempt_middleware_context(Arc::new(ExecutionEffects::default()))
                        .with_request_body_limit(Some(128))
                } else {
                    request = request.with_settings(settings.clone());
                    middleware_context(ClientTransport::HttpJson)
                };
                let response = plan
                    .handle(
                        context,
                        request,
                        Downstream {
                            calls: Arc::default(),
                            closes: Arc::default(),
                            reads: Arc::default(),
                            error: false,
                        }
                        .into_next(),
                    )
                    .await
                    .unwrap();
                let (_, _, _, mut body, _) = response.into_parts();
                while body.next_frame().await.unwrap().is_some() {}
                body.close().await;
            }
            policy
                .route_model(
                    operation(),
                    PublicModelId::new("public-a").unwrap(),
                    BTreeSet::from([ProviderKind::new("example").unwrap()]),
                )
                .await
                .unwrap();
            policy
                .select_account(
                    NonZeroU32::MIN,
                    &ProviderKind::new("example").unwrap(),
                    None,
                    &[candidate("acct_high", 100)],
                    &mut selection_context(),
                )
                .await
                .unwrap();
            policy
                .retry_decision(RetryFacts {
                    attempt_index: NonZeroU32::MIN,
                    provider: ProviderKind::new("example").unwrap(),
                    model: None,
                    error_kind: ProviderErrorKind::RateLimited,
                    upstream_status: Some(429),
                    send_state: UpstreamSendState::NotSent,
                    remaining_routing_attempts: 1,
                    remaining_deadline: Duration::from_secs(3),
                    retry_allowed: true,
                })
                .await;
        }
        let records = std::fs::read_to_string(marker)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 10, "{records:?}");
        let expected = if body_bytes == 128 { 200 } else { 413 };
        assert!(
            records.iter().all(|record| record["status"] == expected),
            "{records:?}"
        );
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), if body_bytes == 128 { 10 } else { 0 });
        assert!(
            received
                .iter()
                .all(|request| request.body == vec![b'x'; body_bytes])
        );
        drop(policy);
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
        super::super::wait_until_empty(cache.path()).await;
    }
}
