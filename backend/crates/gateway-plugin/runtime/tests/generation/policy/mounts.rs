//! 验证混合中间件挂载保持独立顺序与各次调用的续接状态

use super::*;
use gateway_core::middleware::{compose, http, service, websocket};
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
    let plan = runtime.middleware_registry().resolve(&generation).unwrap();
    assert!(plan.has_http() && plan.has_websocket() && plan.has_service());
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
                    extensions: Default::default(),
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
                extensions: Default::default(),
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
