//! 验证真实插件 WebSocket 消息转发、改写与控制消息处理

mod contract;
use super::*;
use gateway_core::{engine::middleware::websocket as core, middleware::compose};

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
    let plan = runtime
        .execution_registry()
        .middleware(&generation)
        .unwrap();
    assert!(plan.has_websocket());
    (cache, runtime, generation, plan)
}
fn context(direction: core::Direction, sender: Arc<Sender>) -> core::Context {
    core::Context {
        extensions: Default::default(),
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
