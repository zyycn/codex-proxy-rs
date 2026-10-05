//! 验证真实插件 WebSocket 升级、双向大帧与会话资源生命周期

use super::*;
use futures::FutureExt as _;
use gateway_core::middleware::{http::upgrade, websocket as ws};
use std::sync::atomic::AtomicBool;

struct Transport {
    incoming: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<ws::Message>>,
    outgoing: tokio::sync::mpsc::Sender<ws::Message>,
    dropped: Arc<AtomicBool>,
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
impl ws::Sender for Transport {
    fn send(&self, message: ws::Message) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async {
            self.outgoing
                .send(message)
                .await
                .map_err(|_| MiddlewareError::Fault)
        })
    }
}
impl ws::Session for Transport {
    fn receive(&self) -> BoxFuture<'_, Result<Option<ws::Message>, MiddlewareError>> {
        Box::pin(async { Ok(self.incoming.lock().await.recv().await) })
    }
}
struct Upgrade(Arc<Transport>);
impl upgrade::WebSocketUpgrade for Upgrade {
    fn accept(
        &self,
        parts: ::http::request::Parts,
        protocols: Vec<String>,
    ) -> BoxFuture<'static, Result<(core::Response, upgrade::PendingSession), MiddlewareError>>
    {
        assert_eq!(parts.uri, "/custom/session");
        assert_eq!(protocols, ["test-session"]);
        let session = self.0.clone();
        Box::pin(async move {
            let mut response = core::Response::new(core::empty_body());
            *response.status_mut() = ::http::StatusCode::SWITCHING_PROTOCOLS;
            Ok((
                response,
                async move { Ok(session as Arc<dyn ws::Session>) }
                    .boxed()
                    .shared(),
            ))
        })
    }
}

#[tokio::test]
async fn session_model_and_network_callbacks_outlive_handshake_and_each_execution_has_its_own_budget()
 {
    let Some(mut environment) = Environment::create_command().await else {
        return;
    };
    let account = environment.account(None).await;
    let key = format!("key_{}", uuid::Uuid::new_v4().simple());
    environment.client_key(&key, "sk-session-fixture").await;
    environment
        .install_plugin(serde_json::json!({"http":true,"mode":"callbacks"}))
        .await;
    let (runtime, bundle) = environment
        .runtime_with_limits(RpcLimits {
            maximum_call_timeout: Duration::from_millis(500),
            ..RpcLimits::default()
        })
        .await;
    environment.store.start_command_line_writes().unwrap();
    let snapshot = bundle.snapshots().snapshot_for_diagnostics().unwrap();
    let plan = runtime
        .middleware_registry()
        .resolve(snapshot.extensions().unwrap())
        .unwrap();
    let (send, incoming) = tokio::sync::mpsc::channel(1);
    let (outgoing, mut receive) = tokio::sync::mpsc::channel(1);
    let dropped = Arc::new(AtomicBool::new(false));
    let transport = Arc::new(Transport {
        incoming: tokio::sync::Mutex::new(incoming),
        outgoing,
        dropped: dropped.clone(),
    });
    let mut request = ::http::Request::builder()
        .uri("/custom/session")
        .body(core::empty_body())
        .unwrap();
    request
        .extensions_mut()
        .insert(Arc::new(Upgrade(transport)) as Arc<dyn upgrade::WebSocketUpgrade>);
    let cancellation = CancellationToken::new();
    let mut response = plan
        .handle_http(
            core::Context {
                plugin_instance_id: None,
                call_id: "http-call".into(),
                parent_call_id: None,
                extensions: Default::default(),
                request_id: "session-models".into(),
                plan: Some(plan.clone()),
                cancellation: cancellation.clone(),
            },
            request,
            compose(Vec::new(), |_| {
                Box::pin(async { panic!("custom session owns transport") })
            }),
        )
        .await
        .unwrap();
    let task = tokio::spawn(
        response
            .extensions_mut()
            .remove::<upgrade::Upgraded>()
            .unwrap()
            .take_task()
            .unwrap(),
    );
    drop(response);
    assert_eq!(receive.recv().await.unwrap().payload, "ready");
    tokio::time::sleep(Duration::from_millis(700)).await;
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("network reply"))
        .expect(1)
        .mount(&server)
        .await;
    let call = |command| ws::Message {
        kind: ws::Kind::Text,
        payload: Bytes::from(serde_json::to_vec(&command).unwrap()),
    };
    send.send(call(serde_json::json!({"method":"host.http.do","params":{"method":"GET","url":server.uri(),"headers":[]}}))).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), receive.recv())
        .await
        .unwrap()
        .unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&reply.payload).unwrap();
    assert_eq!(reply["result"]["status"], 200);
    assert_eq!(reply["payload_bytes"], 13);
    // 超过单个子调用图的 16 次预算，连接仍可承载新的独立执行
    let mut requests = BTreeSet::new();
    for _ in 0..18 {
        send.send(call(serde_json::json!({"method":"host.model.execute","params":{
            "client_key_id":key,"model":crate::support::native::MODEL,"protocol":"openai","operation":"generate","provider":"openai","account_id":account.as_str()
        },"body":{"model":crate::support::native::MODEL,"input":"session request"}}))).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), receive.recv())
            .await
            .unwrap()
            .expect("model callback reply");
        let reply: serde_json::Value = serde_json::from_slice(&reply.payload).unwrap();
        assert!(reply["result"]["events"].as_u64().unwrap() > 0);
        assert!(requests.insert(reply["result"]["request_id"].as_str().unwrap().to_owned()));
    }
    assert_eq!(
        environment
            .wait_for_bound_model_requests(&key, 18)
            .await
            .len(),
        18
    );
    cancellation.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(dropped.load(Ordering::SeqCst));
    drop(plan);
    drop(snapshot);
    drop(bundle);
    runtime.shutdown().await;
    environment
        .store
        .shutdown_command_line_writes()
        .await
        .unwrap();
    environment.close().await;
}

#[tokio::test]
async fn real_plugin_controls_upgrade_and_concurrent_duplex_with_large_binary_frames() {
    let (_cache, runtime, generation, plan) = http_plugin_with_limits(
        "upgrade",
        RpcLimits {
            maximum_call_timeout: Duration::from_millis(500),
            ..RpcLimits::default()
        },
    )
    .await;
    let (send, incoming) = tokio::sync::mpsc::channel(1);
    let (outgoing, mut receive) = tokio::sync::mpsc::channel(1);
    let dropped = Arc::new(AtomicBool::new(false));
    let transport = Arc::new(Transport {
        incoming: tokio::sync::Mutex::new(incoming),
        outgoing,
        dropped: dropped.clone(),
    });
    let mut request = ::http::Request::builder()
        .uri("/custom/session")
        .body(core::empty_body())
        .unwrap();
    request
        .extensions_mut()
        .insert(Arc::new(Upgrade(transport)) as Arc<dyn upgrade::WebSocketUpgrade>);
    let cancellation = CancellationToken::new();
    let mut response = plan
        .handle_http(
            core::Context {
                plugin_instance_id: None,
                call_id: "http-call".into(),
                parent_call_id: None,
                extensions: Default::default(),
                request_id: "session-test".into(),
                plan: Some(plan.clone()),
                cancellation,
            },
            request,
            compose(Vec::new(), |_| {
                Box::pin(async { panic!("custom session must not call the native router") })
            }),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 101);
    let upgraded = response
        .extensions_mut()
        .remove::<upgrade::Upgraded>()
        .unwrap();
    let task = tokio::spawn(upgraded.take_task().unwrap());
    drop(response);
    tokio::time::timeout(Duration::from_secs(5), async {
        assert_eq!(receive.recv().await.unwrap().payload, "ready");
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(
            !dropped.load(Ordering::SeqCst),
            "会话不能在初始调用期限结束时关闭"
        );
        let payload = Bytes::from(vec![0x81; 20 * 1024 * 1024]);
        send.send(ws::Message {
            kind: ws::Kind::Binary,
            payload: payload.clone(),
        })
        .await
        .unwrap();
        let echoed = receive.recv().await.unwrap();
        assert_eq!(echoed.kind, ws::Kind::Binary);
        assert_eq!(echoed.payload, payload);
        for index in 0..32 {
            let payload = Bytes::from(format!("message {index}"));
            send.send(ws::Message {
                kind: ws::Kind::Text,
                payload: payload.clone(),
            })
            .await
            .unwrap();
            assert_eq!(receive.recv().await.unwrap().payload, payload);
        }
        let close = receive.recv().await.unwrap();
        assert_eq!(close.kind, ws::Kind::Close { code: Some(1000) });
        assert_eq!(close.payload, "done");
        task.await.unwrap().unwrap();
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn dropping_an_uncommitted_upgrade_or_cancelling_a_session_reclaims_its_socket() {
    for active in [false, true] {
        let (_cache, runtime, generation, plan) = http_plugin("upgrade").await;
        let (_send, incoming) = tokio::sync::mpsc::channel(1);
        let (outgoing, mut receive) = tokio::sync::mpsc::channel(1);
        let dropped = Arc::new(AtomicBool::new(false));
        let transport = Arc::new(Transport {
            incoming: tokio::sync::Mutex::new(incoming),
            outgoing,
            dropped: dropped.clone(),
        });
        let mut request = ::http::Request::builder()
            .uri("/custom/session")
            .body(core::empty_body())
            .unwrap();
        request
            .extensions_mut()
            .insert(Arc::new(Upgrade(transport)) as Arc<dyn upgrade::WebSocketUpgrade>);
        let cancellation = CancellationToken::new();
        let mut response = plan
            .handle_http(
                core::Context {
                    plugin_instance_id: None,
                    call_id: "http-call".into(),
                    parent_call_id: None,
                    extensions: Default::default(),
                    request_id: "cancel-session".into(),
                    plan: None,
                    cancellation: cancellation.clone(),
                },
                request,
                compose(Vec::new(), |_| {
                    Box::pin(async { panic!("no native route") })
                }),
            )
            .await
            .unwrap();
        if active {
            let task = tokio::spawn(
                response
                    .extensions_mut()
                    .remove::<upgrade::Upgraded>()
                    .unwrap()
                    .take_task()
                    .unwrap(),
            );
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), receive.recv())
                    .await
                    .unwrap()
                    .unwrap()
                    .payload,
                "ready"
            );
            cancellation.cancel();
            assert!(task.await.unwrap().is_err());
        }
        drop(response);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
    }
}
