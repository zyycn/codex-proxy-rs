//! 验证真实插件 WebSocket 消息转发、改写与控制消息处理

use super::*;
use gateway_core::middleware::{compose, websocket as core};

#[derive(Default)]
struct Sender(std::sync::Mutex<Vec<core::Message>>);
impl core::Sender for Sender {
    fn send(&self, message: core::Message) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            self.0.lock().unwrap().push(message);
            Ok(())
        })
    }
}
async fn websocket_plugin(
    mode: &str,
) -> (
    tempfile::TempDir,
    PluginRuntime,
    ExtensionSetReference,
    gateway_core::engine::middleware::FrozenMiddlewarePlan,
) {
    let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
    let package = crate::support::package_with_contributions(
        &worker,
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![Stage::WebSocket],
            vec!["websocket".into()],
            vec!["websocket".into()],
        )]),
    );
    let (cache, runtime) = setup_package(
        vec![InstanceFixture {
            id: "websocket-plugin",
            configuration: serde_json::json!({"websocket":true,"mode":mode}),
            bindings: vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                "websocket",
                0,
                PluginFailurePolicy::Reject,
            )],
        }],
        package,
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime.middleware_registry().resolve(&generation).unwrap();
    assert!(plan.has_websocket());
    (cache, runtime, generation, plan)
}
fn context(direction: core::Direction, sender: Arc<Sender>) -> core::Context {
    core::Context {
        plan: None,
        connection_id: "ws-fixture".into(),
        direction,
        headers: Arc::from([MiddlewareHeader::new(
            "authorization",
            Bytes::from_static(b"Bearer fixture-secret"),
        )]),
        cancellation: CancellationToken::new(),
        sender,
    }
}
#[tokio::test]
async fn real_plugin_passes_large_binary_messages_by_handle_in_both_directions() {
    let (_cache, runtime, generation, plan) = websocket_plugin("passthrough").await;
    for direction in [core::Direction::Incoming, core::Direction::Outgoing] {
        let payload = Bytes::from(vec![255; 20 * 1024 * 1024]);
        let pointer = payload.as_ptr();
        let message = core::Message {
            kind: core::Kind::Binary,
            payload,
        };
        let result = plan
            .handle_websocket(
                context(direction, Arc::default()),
                message,
                compose(Vec::new(), |message| Box::pin(async { Ok(Some(message)) })),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.kind, core::Kind::Binary);
        assert_eq!(result.payload.len(), 20 * 1024 * 1024);
        assert_eq!(
            result.payload.as_ptr(),
            pointer,
            "unread payload stays in the host"
        );
    }
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}
#[tokio::test]
async fn real_plugin_consumes_unknown_control_and_sends_a_binary_reply() {
    let (_cache, runtime, generation, plan) = websocket_plugin("control").await;
    let sender = Arc::new(Sender::default());
    let result = plan
        .handle_websocket(
            context(core::Direction::Incoming, sender.clone()),
            core::Message {
                kind: core::Kind::Text,
                payload: Bytes::from_static(b"custom.control"),
            },
            compose(Vec::new(), |_| {
                Box::pin(async { panic!("consumed control must not reach protocol parser") })
            }),
        )
        .await
        .unwrap();
    assert!(result.is_none());
    {
        let sent = sender.0.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].payload.as_ref(), &[0, 255, 1]);
    }
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}
#[tokio::test]
async fn real_plugin_rewrites_payload_before_next() {
    let (_cache, runtime, generation, plan) = websocket_plugin("map").await;
    let result = plan
        .handle_websocket(
            context(core::Direction::Outgoing, Arc::default()),
            core::Message {
                kind: core::Kind::Text,
                payload: Bytes::from_static(b"opaque output"),
            },
            compose(Vec::new(), |message: core::Message| {
                Box::pin(async {
                    assert_eq!(message.payload, "OPAQUE OUTPUT");
                    Ok(Some(message))
                })
            }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.payload, "OPAQUE OUTPUT");
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}
