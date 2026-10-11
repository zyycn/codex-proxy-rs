//! 验证未知控制消息、迟到中断、结算期间收发和创建请求的串行准入

use super::*;
use gateway_core::engine::response_control::{
    ResponseControlTransport, ResponseControlUnavailable,
};

struct ControlTransport {
    id: String,
    active: std::sync::atomic::AtomicBool,
    requests: Mutex<Vec<String>>,
    sender: tokio::sync::mpsc::Sender<Value>,
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Value>>,
}

impl ControlTransport {
    fn new(id: String) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        Self {
            id,
            active: std::sync::atomic::AtomicBool::new(true),
            requests: Mutex::default(),
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
        }
    }
}

#[async_trait]
impl ResponseControlTransport for ControlTransport {
    async fn send(&self, payload: &str) -> Result<(), ResponseControlUnavailable> {
        self.requests.lock().unwrap().push(payload.to_owned());
        let original: Value =
            serde_json::from_str(payload).unwrap_or_else(|_| Value::String(payload.to_owned()));
        let event = if original["type"] == "future.reject" {
            upstream_control_error()
        } else if original["type"] == "response.interrupt"
            && original["response_id"] == self.id
            && original["mode"] == "discard_partial_items"
            && self.active.load(Ordering::SeqCst)
        {
            json!({"type":"response.incomplete", "response":{"id":self.id,"model":"model-a","status":"incomplete","output":[],"incomplete_details":{"reason":"interrupted"}}})
        } else {
            json!({"type":"future.control.ack", "original":original})
        };
        self.sender
            .send(event)
            .await
            .map_err(|_| ResponseControlUnavailable)
    }
    async fn receive(&self) -> Result<String, ResponseControlUnavailable> {
        self.receiver
            .lock()
            .await
            .recv()
            .await
            .map(|event| event.to_string())
            .ok_or(ResponseControlUnavailable)
    }
}

#[derive(Default)]
struct InterruptProvider {
    calls: AtomicUsize,
    fail_first: bool,
    retry_started: tokio::sync::Notify,
    retry_release: tokio::sync::Notify,
    transport: Mutex<Option<Arc<ControlTransport>>>,
    complete: tokio::sync::Notify,
    initial_messages: Mutex<std::collections::VecDeque<ProtocolWireEvent>>,
}

#[async_trait]
impl Provider for InterruptProvider {
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
        let number = self.calls.fetch_add(1, Ordering::SeqCst);
        if number == 1 && self.fail_first {
            self.retry_started.notify_one();
            self.retry_release.notified().await;
            assert!(
                context
                    .excluded_accounts()
                    .contains(&ProviderAccountId::new("acct_api_test").unwrap())
            );
        }
        let id = format!("resp_interrupt_{number}");
        let transport = Arc::new(ControlTransport::new(id));
        let owner: Arc<dyn ResponseControlTransport> = transport.clone();
        context.response_control().unwrap().bind(&owner);
        *self.transport.lock().unwrap() = Some(transport.clone());
        let candidate = request.candidate();
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            candidate.upstream_model().unwrap().clone(),
            ProviderAccountId::new(if self.fail_first && number > 0 {
                "acct_api_retry"
            } else {
                "acct_api_test"
            })
            .unwrap(),
            UpstreamTransport::new("websocket").unwrap(),
        );
        let body = futures::stream::unfold(
            (0, transport, self),
            |(phase, transport, provider)| async move {
                let pending = if phase == 1 {
                    provider.initial_messages.lock().unwrap().pop_front()
                } else {
                    None
                };
                if let Some(wire) = pending {
                    return Some((Ok(ProviderEvent::wire(wire)), (phase, transport, provider)));
                }
                if phase == 1 && provider.fail_first && transport.id == "resp_interrupt_0" {
                    provider.complete.notified().await;
                    transport.active.store(false, Ordering::SeqCst);
                    return Some((
                        Err(ProviderError::new(
                            gateway_core::error::ProviderErrorKind::QuotaExhausted,
                            UpstreamSendState::Sent,
                        )
                        .with_replay_safe()),
                        (2, transport, provider),
                    ));
                }
                let (wire, events, next_phase) = match phase {
                    0 => (
                        json!({"type":"response.created","response":{"id":transport.id,"model":"model-a","status":"in_progress","output":[]}}),
                        vec![GatewayEvent::Started(ResponseMeta::new(
                            &transport.id,
                            "model-a",
                        ))],
                        1,
                    ),
                    1 => {
                        let wire = tokio::select! {
                            reply = transport.receive() => serde_json::from_str::<Value>(&reply.unwrap()).unwrap(),
                            () = provider.complete.notified() => json!({"type":"response.completed","response":{"id":transport.id,"model":"model-a","status":"completed","output":[]}}),
                        };
                        if matches!(
                            wire["type"].as_str(),
                            Some("response.completed" | "response.incomplete")
                        ) {
                            transport.active.store(false, Ordering::SeqCst);
                            (
                                wire,
                                vec![GatewayEvent::Completed(ResponseMeta::new(
                                    &transport.id,
                                    "model-a",
                                ))],
                                2,
                            )
                        } else {
                            (wire, Vec::new(), 1)
                        }
                    }
                    _ => return None,
                };
                let event_type = wire["type"].as_str().unwrap().to_owned();
                let mut event = ProviderEvent::canonical_with_wire(
                    events,
                    ProtocolWireEvent::json("openai", Some(event_type), wire).unwrap(),
                );
                if phase == 0 && provider.fail_first {
                    event = event.with_replayable_prelude();
                }
                Some((Ok(event), (next_phase, transport, provider)))
            },
        );
        Ok(ProviderStream::new(metadata, body, ()))
    }
}

#[derive(Default)]
struct InterruptAdmissions {
    active: AtomicUsize,
    release_started: tokio::sync::Notify,
    release_gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl ClientAdmissionPort for InterruptAdmissions {
    fn abandon(&self, _: &ClientApiKeyId, _: &ModelRequestId) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
    fn admit(
        &self,
        _: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>> {
        Box::pin(async {
            self.active.fetch_add(1, Ordering::SeqCst);
            Ok(ClientAdmissionDecision::Granted)
        })
    }
    fn release<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>> {
        Box::pin(async {
            let gate = self.release_gate.lock().unwrap().take();
            self.release_started.notify_one();
            if let Some(gate) = gate {
                gate.await.unwrap();
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(true)
        })
    }
    fn restore(
        &self,
        _: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        Box::pin(async { unreachable!("no recovery in live connection test") })
    }
}

type TestSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn send(socket: &mut TestSocket, value: Value) {
    socket
        .send(ClientMessage::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn next(socket: &mut TestSocket) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("response deadline")
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value["type"] != "response.metadata" {
            return value;
        }
    }
}

async fn connect(
    provider: Arc<InterruptProvider>,
    admissions: Arc<InterruptAdmissions>,
) -> (TestSocket, Server) {
    let execution = Arc::new(DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(if provider.fail_first {
            crate::openai::snapshot_with_accounts(
                "sk_ws_interrupt",
                "openai",
                "key_api_test",
                &["acct_api_test", "acct_api_retry"],
            )
        } else {
            snapshot("sk_ws_interrupt", "openai")
        }),
        Arc::new(SettlementPorts::default()),
        ProviderRegistry::new([provider as Arc<dyn Provider>]).unwrap(),
        admissions,
        Arc::new(UnusedContinuation),
        Arc::new(IgnoredClientApiKeyUsage),
        Arc::new(crate::support::RecordingDiagnostics::default()),
    ));
    let app = api_router(execution).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer sk_ws_interrupt"),
    );
    let (socket, _) = connect_async(request).await.unwrap();
    (socket, server)
}

fn upstream_control_error() -> Value {
    json!({"type":"error", "status":400, "error":{"type":"invalid_request_error","code":"future_rejected","message":"Synthetic upstream rejection","param":"type"},"vendor_extension":true})
}

fn interrupt(id: &str, mode: &str) -> Value {
    json!({"type":"response.interrupt","response_id":id,"mode":mode,"future_extension":true})
}

#[tokio::test]
async fn late_interrupt_after_terminal_is_sent_upstream_and_keeps_the_connection_reusable() {
    let provider = Arc::new(InterruptProvider::default());
    let admissions = Arc::new(InterruptAdmissions::default());
    let (mut socket, _server) = connect(provider.clone(), admissions.clone()).await;
    let create = json!({"type":"response.create","model":"model-a","input":"hello"});
    for (number, terminal) in [(0, "response.completed"), (1, "response.incomplete")] {
        let id = format!("resp_interrupt_{number}");
        send(&mut socket, create.clone()).await;
        assert_eq!(next(&mut socket).await["type"], "response.created");
        if number == 0 {
            provider.complete.notify_one();
        } else {
            send(&mut socket, interrupt(&id, "discard_partial_items")).await;
        }
        assert_eq!(next(&mut socket).await["type"], terminal);
        tokio::time::timeout(Duration::from_secs(5), async {
            while admissions.active.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for control in [
            interrupt(&id, "discard_partial_items"),
            interrupt(&id, "future_mode"),
            json!({"type":"future.control","extra":[1,2]}),
        ] {
            send(&mut socket, control.clone()).await;
            assert_eq!(
                next(&mut socket).await,
                json!({"type":"future.control.ack","original":control})
            );
        }
        send(&mut socket, json!({"type":"future.reject"})).await;
        assert_eq!(next(&mut socket).await, upstream_control_error());
        assert_eq!(provider.calls.load(Ordering::SeqCst), number + 1);
        assert_eq!(admissions.active.load(Ordering::SeqCst), 0);
    }
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn late_interrupt_during_terminal_settlement_does_not_abort_or_reorder_requests() {
    let provider = Arc::new(InterruptProvider::default());
    let (release, wait_for_release) = tokio::sync::oneshot::channel();
    let admissions = Arc::new(InterruptAdmissions {
        release_gate: Mutex::new(Some(wait_for_release)),
        ..Default::default()
    });
    let (mut socket, _server) = connect(provider.clone(), admissions.clone()).await;
    let create = json!({"type":"response.create","model":"model-a","input":"hello"});
    send(&mut socket, create.clone()).await;
    assert_eq!(next(&mut socket).await["type"], "response.created");
    provider.complete.notify_one();
    assert_eq!(next(&mut socket).await["type"], "response.completed");
    tokio::time::timeout(
        Duration::from_secs(5),
        admissions.release_started.notified(),
    )
    .await
    .unwrap();
    send(&mut socket, create).await;
    let late = interrupt("resp_interrupt_0", "discard_partial_items");
    send(&mut socket, late.clone()).await;
    assert_eq!(
        next(&mut socket).await,
        json!({"type":"future.control.ack", "original":late})
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(admissions.active.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    assert_eq!(
        next(&mut socket).await["response"]["id"],
        "resp_interrupt_1"
    );
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn unknown_controls_preserve_bytes_and_do_not_admit_queued_creates() {
    let provider = Arc::new(InterruptProvider::default());
    let admissions = Arc::new(InterruptAdmissions::default());
    let (mut socket, _server) = connect(provider.clone(), admissions.clone()).await;
    let create = json!({"type":"response.create","model":"model-a","input":"hello"});
    send(&mut socket, json!({"type":"future.control"})).await;
    let error = next(&mut socket).await;
    assert_eq!(error["type"], "error");
    assert_ne!(error["error"]["param"], "type");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    send(&mut socket, create).await;
    assert_eq!(next(&mut socket).await["type"], "response.created");
    // 重复 type 仍是创建请求，不能借投影解码失败绕过串行准入
    socket.send(ClientMessage::Text(r#"{"type":"response.create","type":"response.create","model":"model-a","input":"next"}"#.into())).await.unwrap();
    for raw in [
        r#"{ "type": "future.control", "mode": "unknown", "extension": [1, 2] }"#,
        r#"{"extension":"no local type contract"}"#,
        r#"{"type":42,"extension":true}"#,
        "not-json",
    ] {
        socket.send(ClientMessage::Text(raw.into())).await.unwrap();
        let expected =
            serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
        assert_eq!(next(&mut socket).await["original"], expected);
        assert_eq!(
            provider
                .transport
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .requests
                .lock()
                .unwrap()
                .last()
                .unwrap(),
            raw
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
    send(
        &mut socket,
        interrupt("resp_interrupt_0", "discard_partial_items"),
    )
    .await;
    assert_eq!(next(&mut socket).await["type"], "response.incomplete");
    assert_eq!(
        next(&mut socket).await["response"]["id"],
        "resp_interrupt_1"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while admissions.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disconnect releases active execution");
}

#[tokio::test]
async fn active_websocket_delivery_preserves_unparsed_messages_and_business_metadata() {
    let messages = vec![
        "{ \"type\": \"future.event\", \"number\": 1e3, \"escaped\": \"\\u0061\" }\r\n".to_owned(),
        r#"{"type":"response.metadata","metadata":{"type":"safety_buffering","use_cases":["cyber"],"reasons":["user_risk"],"future":{"keep":true}}}"#.to_owned(),
        format!("{{\"type\":\"future.deep\",\"extension\":{}0{}}}", "[".repeat(140), "]".repeat(140)),
        "future non-JSON text".to_owned(), "[DONE]".to_owned(),
    ];
    let provider = Arc::new(InterruptProvider::default());
    *provider.initial_messages.lock().unwrap() = messages
        .iter()
        .map(|raw| match serde_json::from_str::<Value>(raw) {
            Ok(value) => ProtocolWireEvent::json(
                "openai",
                value.get("type").and_then(Value::as_str).map(str::to_owned),
                value,
            )
            .unwrap()
            .with_raw_websocket_message(raw.as_str()),
            Err(_) => ProtocolWireEvent::raw_websocket("openai", raw.as_str()).unwrap(),
        })
        .collect();
    let (mut socket, _server) =
        connect(provider.clone(), Arc::new(InterruptAdmissions::default())).await;
    send(
        &mut socket,
        json!({"type":"response.create","model":"model-a","input":"synthetic"}),
    )
    .await;
    assert_eq!(next(&mut socket).await["type"], "response.created");
    for expected in &messages {
        let actual = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(actual.to_text().unwrap(), expected);
    }
    provider.complete.notify_one();
    assert_eq!(next(&mut socket).await["type"], "response.completed");
}

#[tokio::test]
async fn prelude_retry_retains_interrupt_during_account_switch() {
    for interrupt_before_new_created in [true, false] {
        let provider = Arc::new(InterruptProvider {
            fail_first: true,
            ..Default::default()
        });
        let admissions = Arc::new(InterruptAdmissions::default());
        let (mut socket, _server) = connect(provider.clone(), admissions.clone()).await;
        send(
            &mut socket,
            json!({"type":"response.create","model":"model-a","input":"hello"}),
        )
        .await;
        assert_eq!(
            next(&mut socket).await["response"]["id"],
            "resp_interrupt_0"
        );
        let old = provider.transport.lock().unwrap().clone().unwrap();
        provider.complete.notify_one();
        tokio::time::timeout(Duration::from_secs(5), provider.retry_started.notified())
            .await
            .unwrap();
        if interrupt_before_new_created {
            send(
                &mut socket,
                interrupt("resp_interrupt_0", "discard_partial_items"),
            )
            .await;
            // 等待服务端处理控制帧，随后才允许新 attempt 建立连接
            tokio::time::timeout(Duration::from_secs(5), async {
                while old.requests.lock().unwrap().is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        provider.retry_release.notify_one();
        assert_eq!(
            next(&mut socket).await["response"]["id"],
            "resp_interrupt_1"
        );
        if !interrupt_before_new_created {
            // 迟到的旧 ID 也应作用于当前逻辑响应
            send(
                &mut socket,
                interrupt("resp_interrupt_0", "discard_partial_items"),
            )
            .await;
        }
        let terminal = next(&mut socket).await;
        assert_eq!(terminal["type"], "response.incomplete");
        assert_eq!(terminal["response"]["id"], "resp_interrupt_1");
        let current = provider.transport.lock().unwrap().clone().unwrap();
        let requests = current.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&requests[0]).unwrap(),
            interrupt("resp_interrupt_1", "discard_partial_items")
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        socket.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn non_replayable_delivery_blocks_retry_even_after_a_replayable_prelude() {
    for event in [
        json!({"type":"response.output_text.delta","delta":"visible","output_index":0,"content_index":0}),
        json!({"type":"response.future_progress","marker":"preserved"}),
    ] {
        let provider = Arc::new(InterruptProvider {
            fail_first: true,
            ..Default::default()
        });
        provider.initial_messages.lock().unwrap().push_back(
            ProtocolWireEvent::json(
                "openai",
                Some(event["type"].as_str().unwrap().to_owned()),
                event.clone(),
            )
            .unwrap(),
        );
        let (mut socket, _server) = connect(provider.clone(), Arc::default()).await;
        send(
            &mut socket,
            json!({"type":"response.create","model":"model-a","input":"hello"}),
        )
        .await;
        assert_eq!(next(&mut socket).await["type"], "response.created");
        assert_eq!(next(&mut socket).await, event);
        provider.complete.notify_one();
        assert_eq!(next(&mut socket).await["type"], "error");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        socket.close(None).await.unwrap();
    }
}
