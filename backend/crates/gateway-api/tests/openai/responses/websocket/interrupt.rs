use super::*;
use gateway_core::engine::response_control::{ResponseControl, ResponseInterruptError};

#[derive(Default)]
struct InterruptProvider {
    calls: AtomicUsize,
    controls: Mutex<Vec<ResponseControl>>,
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
        let id = format!("resp_interrupt_{number}");
        let control = context
            .response_control()
            .expect("root control propagated through Core")
            .clone();
        let active = control.activate(id.clone()).unwrap();
        self.controls.lock().unwrap().push(control);
        let candidate = request.candidate();
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            candidate.upstream_model().unwrap().clone(),
            ProviderAccountId::new("acct_api_test").unwrap(),
            UpstreamTransport::new("websocket").unwrap(),
        );
        let body = futures::stream::unfold(
            (0, Some(active), id),
            |(phase, mut active, id)| async move {
                let (event_type, status, event) = match phase {
                    0 => (
                        "response.created",
                        "in_progress",
                        GatewayEvent::Started(ResponseMeta::new(&id, "model-a")),
                    ),
                    1 => {
                        active.as_ref().unwrap().requested().await;
                        drop(active.take());
                        (
                            "response.incomplete",
                            "incomplete",
                            GatewayEvent::Completed(ResponseMeta::new(&id, "model-a")),
                        )
                    }
                    _ => return None,
                };
                let mut response = json!({"id":id,"model":"model-a","status":status,"output":[]});
                if phase == 1 {
                    response["incomplete_details"] = json!({"reason":"interrupted"});
                }
                let event = ProviderEvent::canonical_with_wire(
                    vec![event],
                    ProtocolWireEvent::json(
                        "openai",
                        Some(event_type.to_owned()),
                        json!({
                            "type":event_type, "response":response
                        }),
                    )
                    .unwrap(),
                );
                Some((Ok(event), (phase + 1, active, id)))
            },
        );
        Ok(ProviderStream::new(metadata, body, ()))
    }
}

#[derive(Default)]
struct InterruptAdmissions {
    active: AtomicUsize,
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

#[tokio::test]
async fn interrupt_is_live_scoped_and_keeps_queued_creates_serial() {
    let provider = Arc::new(InterruptProvider::default());
    let admissions = Arc::new(InterruptAdmissions::default());
    let execution = Arc::new(DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot("sk_ws_interrupt", "openai")),
        Arc::new(SettlementPorts::default()),
        ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).unwrap(),
        admissions.clone(),
        Arc::new(UnusedContinuation),
        Arc::new(IgnoredClientApiKeyUsage),
    ));
    let app = api_router(execution).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer sk_ws_interrupt"),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();
    let create = json!({"type":"response.create","model":"model-a","input":"hello"});
    let interrupt =
        |id: &str, mode: &str| json!({"type":"response.interrupt","response_id":id,"mode":mode});
    send(
        &mut socket,
        interrupt("resp_interrupt_0", "discard_partial_items"),
    )
    .await;
    assert_eq!(next(&mut socket).await["type"], "error");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    send(&mut socket, create.clone()).await;
    assert_eq!(next(&mut socket).await["type"], "response.created");
    send(&mut socket, create).await;
    for invalid in [
        interrupt("resp_other", "discard_partial_items"),
        interrupt("resp_interrupt_0", "unknown"),
        interrupt("", "discard_partial_items"),
    ] {
        send(&mut socket, invalid).await;
        let error = next(&mut socket).await;
        assert_eq!(error["type"], "error");
        assert_eq!(error["status"], 400);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
    send(
        &mut socket,
        interrupt("resp_interrupt_0", "discard_partial_items"),
    )
    .await;
    let terminal = next(&mut socket).await;
    assert_eq!(terminal["type"], "response.incomplete");
    assert_eq!(
        terminal["response"]["incomplete_details"]["reason"],
        "interrupted"
    );
    assert_eq!(
        next(&mut socket).await["response"]["id"],
        "resp_interrupt_1"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    send(
        &mut socket,
        interrupt("resp_interrupt_0", "discard_partial_items"),
    )
    .await;
    assert_eq!(next(&mut socket).await["type"], "error");
    assert_eq!(
        provider.controls.lock().unwrap()[0].interrupt("resp_interrupt_0"),
        Err(ResponseInterruptError::Unavailable)
    );
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while admissions.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disconnect releases active execution");
    assert_eq!(
        provider.controls.lock().unwrap()[1].interrupt("resp_interrupt_1"),
        Err(ResponseInterruptError::Unavailable)
    );
}
