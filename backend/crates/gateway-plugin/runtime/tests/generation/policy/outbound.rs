//! 验证真实插件出站准备的组合、资源预算和终止边界

use super::*;
use gateway_core::engine::middleware::outbound::{Next, Request, Target};

async fn runtime(
    instances: Vec<InstanceFixture>,
    limits: RpcLimits,
) -> (tempfile::TempDir, PluginRuntime) {
    let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
    let package = crate::support::package_with_contributions(
        &worker,
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Outbound],
            vec!["outbound".into()],
            vec!["outbound".into()],
        )]),
    );
    setup_package_with_limits(instances, package, limits).await
}

fn instance(id: &'static str, order: i32, mut configuration: serde_json::Value) -> InstanceFixture {
    configuration["outbound"] = true.into();
    InstanceFixture {
        id,
        configuration,
        bindings: vec![binding(
            MIDDLEWARE_CONTRIBUTION,
            "outbound",
            order,
            PluginFailurePolicy::Reject,
        )],
    }
}

fn context() -> MiddlewareContext {
    context_for_provider("openai")
}

fn context_for_provider(provider: &str) -> MiddlewareContext {
    MiddlewareContext::new(
        MiddlewareTarget {
            request_id: ModelRequestId::new("req_outbound").unwrap(),
            mount: MiddlewareMount::Outbound,
            attempt_index: Some(NonZeroU32::MIN),
            operation: Some(OperationKind::Generate),
            endpoint: "/v1/responses".into(),
            transport: ClientTransport::HttpJson,
            provider: Some(ProviderKind::new(provider).unwrap()),
            model: Some("public-a".into()),
            account_id: Some(ProviderAccountId::new("acct_openai").unwrap()),
        },
        MiddlewareAuthority {
            client_key_id: ClientApiKeyId::new("key-middleware").unwrap(),
            account_group_ids: Arc::from([AccountGroupId::new(
                "grp_11111111111111111111111111111111",
            )
            .unwrap()]),
            cancellation: CancellationToken::new(),
            deadline: (SystemTime::now() + Duration::from_secs(5)).into(),
            extension_scope: ExtensionCallScope::default(),
            execution_effects: Some(Arc::default()),
        },
    )
}

fn request() -> Request {
    Request {
        target: Target::Http {
            method: ::http::Method::POST,
            uri: "https://example.test/original".parse().unwrap(),
        },
        headers: vec![MiddlewareHeader::new(
            "x-delete",
            Bytes::from_static(b"original"),
        )],
        body: Bytes::from_static(&[0, 255]),
    }
}

fn terminal() -> Next {
    gateway_core::middleware::compose(Vec::new(), |request| Box::pin(async { Ok(request) }))
}

#[tokio::test]
async fn outbound_plugins_compose_http_and_websocket_with_full_replacement() {
    for websocket in [false, true] {
        let records = tempfile::NamedTempFile::new().unwrap();
        let initial_body = if websocket {
            b"msg".to_vec()
        } else {
            vec![0, 255]
        };
        let target = if websocket {
            serde_json::json!({"kind":"websocket", "uri":"wss://rewritten.test/socket"})
        } else {
            serde_json::json!({"kind":"http", "method":"PUT", "uri":"https://rewritten.test/path?opaque=1"})
        };
        let mut outer = instance(
            "outer",
            0,
            serde_json::json!({
                "records":records.path(), "enter_byte":65, "exit_byte":68,
                "expected_body":initial_body, "target":target,
                "headers":[{"name":"x-multi","value":[97]}, {"name":"x-multi","value":[255]}],
                "second_next":true,
            }),
        );
        outer.bindings[0].client_key_ids = vec!["key-middleware".into()];
        outer.bindings[0].account_group_ids = vec!["grp_11111111111111111111111111111111".into()];
        outer.bindings[0].provider_ids = vec!["openai".into()];
        outer.bindings[0].models = vec!["public-a".into()];
        let mut inner_body = initial_body.clone();
        inner_body.push(65);
        let mut inner = instance(
            "inner",
            1,
            serde_json::json!({
                "records":records.path(), "enter_byte":66, "exit_byte":67, "expected_body":inner_body,
            }),
        );
        inner.bindings[0].provider_ids = vec!["openai".into()];
        let mut excluded = instance("excluded", 2, serde_json::json!({"fault":true}));
        excluded.bindings[0].provider_ids = vec!["xai".into()];
        let (cache, runtime) = runtime(vec![outer, inner, excluded], RpcLimits::default()).await;
        let generation = prepare(&runtime).await;
        let plan = runtime
            .execution_registry()
            .middleware(&generation)
            .unwrap();
        assert!(plan.has_outbound(&context()));
        assert!(!plan.has_outbound(&context_for_provider("unmatched")));
        let mut input = request();
        input.body = initial_body.clone().into();
        if websocket {
            input.target = Target::WebSocket {
                uri: "wss://example.test/socket".parse().unwrap(),
            };
        }
        let result = plan
            .handle_outbound(context(), input, terminal())
            .await
            .unwrap();
        let mut expected_body = initial_body;
        expected_body.extend([65, 66, 67, 68]);
        assert_eq!(result.body.as_ref(), expected_body);
        let headers = result.header_map().unwrap();
        assert!(!headers.contains_key("x-delete"));
        assert_eq!(
            headers
                .get_all("x-multi")
                .iter()
                .map(|value| value.as_bytes())
                .collect::<Vec<_>>(),
            [b"a".as_slice(), &[255]]
        );
        match result.target {
            Target::Http { method, uri } => {
                assert_eq!(method, ::http::Method::PUT);
                assert_eq!(uri.to_string(), "https://rewritten.test/path?opaque=1");
            }
            Target::WebSocket { uri } => assert_eq!(uri.to_string(), "wss://rewritten.test/socket"),
        }
        assert_eq!(
            std::fs::read_to_string(records.path()).unwrap(),
            "enter:outer\nenter:inner\nexit:inner\nexit:outer\n"
        );
        drop(plan);
        drop(generation);
        super::super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn outbound_plugin_can_return_directly_without_running_next() {
    let (cache, runtime) = runtime(
        vec![instance(
            "direct",
            0,
            serde_json::json!({"direct":true,"exit_byte":42}),
        )],
        RpcLimits::default(),
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime
        .execution_registry()
        .middleware(&generation)
        .unwrap();
    let result = plan
        .handle_outbound(
            context(),
            request(),
            gateway_core::middleware::compose(Vec::new(), |_| {
                panic!("direct preparation must not consume next")
            }),
        )
        .await
        .unwrap();
    assert_eq!(result.body.as_ref(), [0, 255, 42]);
    drop(plan);
    drop(generation);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn outbound_http_callbacks_use_the_frozen_body_budget_and_propagate_failure() {
    for body_len in [3, 4] {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(204))
            .expect(if body_len == 3 { 1 } else { 0 })
            .mount(&server)
            .await;
        let mut entry = instance(
            "callback",
            0,
            serde_json::json!({"http_url":server.uri(),"http_body_len":body_len,"fault":true}),
        );
        entry.bindings[0].failure_policy = PluginFailurePolicy::Delegate;
        let (cache, runtime) = runtime(vec![entry], RpcLimits::default()).await;
        let generation = prepare(&runtime).await;
        let plan = runtime
            .execution_registry()
            .middleware(&generation)
            .unwrap();
        let result = plan
            .handle_outbound(
                context().with_request_body_limit(Some(3)),
                request(),
                gateway_core::middleware::compose(Vec::new(), |_| {
                    panic!("plugin failure must not silently replay preparation")
                }),
            )
            .await;
        assert!(result.is_err());
        server.verify().await;
        drop(plan);
        drop(generation);
        super::super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn outbound_invalid_method_is_not_repaired_or_delegated() {
    let mut entry = instance(
        "invalid",
        0,
        serde_json::json!({"direct":true,"target":{"kind":"http","method":"BAD METHOD","uri":"https://example.test/"}}),
    );
    entry.bindings[0].failure_policy = PluginFailurePolicy::Delegate;
    let (cache, runtime) = runtime(vec![entry], RpcLimits::default()).await;
    let generation = prepare(&runtime).await;
    let plan = runtime
        .execution_registry()
        .middleware(&generation)
        .unwrap();
    let result = plan.handle_outbound(context(), request(), terminal()).await;
    assert!(matches!(result, Err(MiddlewareError::InvalidState)));
    drop(plan);
    drop(generation);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn outbound_next_wait_pauses_plugin_budget_and_cancel_releases_the_continuation() {
    for cancel in [false, true] {
        let (cache, runtime) = runtime(
            vec![
                instance(
                    "outer",
                    0,
                    serde_json::json!({"call_budgets_ms":{"outbound":300}}),
                ),
                instance(
                    "inner",
                    1,
                    serde_json::json!({"call_budgets_ms":{"outbound":300}}),
                ),
            ],
            RpcLimits::default(),
        )
        .await;
        let generation = prepare(&runtime).await;
        let plan = runtime
            .execution_registry()
            .middleware(&generation)
            .unwrap();
        let context = context();
        let cancellation = context.cancellation().clone();
        let (started, entered) = tokio::sync::oneshot::channel();
        let (closed, dropped) = tokio::sync::oneshot::channel();
        let pending = tokio::spawn(async move {
            plan.handle_outbound(
                context,
                request(),
                gateway_core::middleware::compose(Vec::new(), move |request| {
                    Box::pin(async move {
                        let _closed = NotifyOnDrop(Some(closed));
                        started.send(()).unwrap();
                        if cancel {
                            std::future::pending::<()>().await;
                        } else {
                            tokio::time::sleep(Duration::from_millis(900)).await;
                        }
                        Ok(request)
                    })
                }),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!pending.is_finished());
        if cancel {
            cancellation.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.is_err(), cancel);
        tokio::time::timeout(Duration::from_secs(2), dropped)
            .await
            .unwrap()
            .unwrap();
        drop(generation);
        super::super::wait_until_empty(cache.path()).await;
    }
}

struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
