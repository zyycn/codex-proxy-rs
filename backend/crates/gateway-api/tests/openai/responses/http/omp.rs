use super::*;
use crate::openai::responses::openai_wire_event;
use futures::SinkExt;
use tokio::sync::Mutex;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as ClientMessage, client::IntoClientRequest},
};

struct OmpObservedRequest {
    endpoint: String,
    body: Value,
    context: Value,
    previous_response_id: Option<String>,
}

struct OmpExecution {
    client: AuthenticatedClient,
    observed: Arc<Mutex<Vec<OmpObservedRequest>>>,
    sessions: Mutex<VecDeque<FakeSession>>,
}

impl ExecutionService for OmpExecution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        (plaintext == "sk_omp_compat")
            .then(|| self.client.clone())
            .ok_or(ClientAuthenticationError::InvalidKey)
    }
    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        vec![PublicModelId::new("model-a").unwrap()]
    }
    fn contains_public_model(&self, _: &AuthenticatedClient, model: &PublicModelId) -> bool {
        model.as_str() == "model-a"
    }
    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            let Operation::Generate(generate) = &request.operation else {
                return Err(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "OMP fixture requires Generate",
                ));
            };
            if request.metadata.previous_response_id.is_some() {
                assert!(
                    request
                        .operation
                        .capability_requirements()
                        .features()
                        .contains(&gateway_core::operation::Feature::NativeContinuation)
                );
            }
            let mut observed = self.observed.lock().await;
            observed.push(OmpObservedRequest {
                endpoint: request.metadata.endpoint,
                body: Value::Object(generate.protocol_payload().body().clone()),
                context: Value::Object(generate.protocol_payload().context().clone()),
                previous_response_id: request
                    .metadata
                    .previous_response_id
                    .as_ref()
                    .map(|id| id.as_str().to_owned()),
            });
            let session = self.sessions.lock().await.pop_front().ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::Internal,
                    "OMP fixture exhausted scripted sessions",
                )
            })?;
            Ok(StartedExecution {
                request_id: ModelRequestId::new(format!("req_omp_{}", observed.len())).unwrap(),
                created_at: std::time::SystemTime::now(),
                stream: request.metadata.stream,
                session: Box::new(session),
            })
        })
    }
    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "OMP fixture must not execute provider endpoints",
            ))
        })
    }
}

fn execution(sessions: Vec<FakeSession>) -> Arc<OmpExecution> {
    Arc::new(OmpExecution {
        client: authenticated_client_for_provider("sk_omp_compat", "openai"),
        observed: Arc::new(Mutex::new(Vec::new())),
        sessions: Mutex::new(sessions.into()),
    })
}

fn search_body() -> Value {
    json!({"model":"model-a","stream":true,"store":false,"include":["web_search_call.action.sources"],"parallel_tool_calls":true,"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"site:example.org Rust releases"}]}],"tools":[{"type":"web_search","search_context_size":"high"}],"tool_choice":{"type":"web_search"},"instructions":"Search and cite sources."})
}

fn message(id: &str, text: &str) -> Value {
    json!({"id":id,"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text,"annotations":[]}]})
}

fn event_step(data: Value, canonical: Vec<GatewayEvent>, first: bool) -> NextStep {
    let kind = data["type"].as_str().unwrap().to_owned();
    NextStep::Event(delivery_provider(
        openai_wire_event(canonical, &kind, data),
        if first {
            CommitRequirement::CommitBeforeDelivery
        } else {
            CommitRequirement::AlreadyCommitted
        },
    ))
}

fn created(id: &str) -> NextStep {
    event_step(
        json!({"type":"response.created","response":{"id":id,"model":"model-a","status":"in_progress","output":[]}}),
        vec![GatewayEvent::Started(ResponseMeta::new(id, "model-a"))],
        true,
    )
}

fn success_steps(id: &str, items: Vec<Value>) -> Vec<NextStep> {
    let mut steps = vec![created(id)];
    for (index, item) in items.iter().enumerate() {
        steps.push(event_step(
            json!({"type":"response.output_item.added","output_index":index,"item":item}),
            vec![],
            false,
        ));
        if item["type"] == "web_search_call" {
            steps.push(event_step(json!({"type":"response.web_search_call.searching","output_index":index,"item_id":item["id"]}), vec![], false));
        }
        steps.push(event_step(
            json!({"type":"response.output_item.done","output_index":index,"item":item}),
            vec![],
            false,
        ));
    }
    steps.push(event_step(json!({"type":"response.completed","response":{"id":id,"model":"model-a","status":"completed","output":items,"usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15,"input_tokens_details":{"cached_tokens":2}}}}), vec![GatewayEvent::Completed(ResponseMeta::new(id, "model-a"))], false));
    steps.push(NextStep::FinalizeSuccess);
    steps
}

fn session(id: &str, items: Vec<Value>) -> FakeSession {
    FakeSession::streaming(Arc::new(Trace::default()), success_steps(id, items))
}

async fn post(
    execution: Arc<OmpExecution>,
    path: &str,
    key: &str,
    body: &Value,
) -> axum::response::Response {
    api_router(execution)
        .await
        .oneshot(
            Request::post(path)
                .header(AUTHORIZATION, format!("Bearer {key}"))
                .header(CONTENT_TYPE, "application/json")
                .header("originator", "omp")
                .header("user-agent", "omp/fixture")
                .header("version", "0.155.1")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn sse(response: axum::response::Response) -> Vec<Value> {
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.ends_with("data: [DONE]\n\n"), "{text}");
    parse_sse_events(text)
        .unwrap()
        .into_iter()
        .filter(|e| e.data != "[DONE]")
        .map(|e| serde_json::from_str(&e.data).unwrap())
        .collect()
}

#[tokio::test]
async fn omp_alias_search_stream_preserves_results_and_citations() {
    let search = json!({"id":"ws_omp_1","type":"web_search_call","status":"completed","action":{"type":"search","sources":[{"url":"https://example.org/rust","title":"Rust releases"}]}});
    let mut answer = message("msg_omp_search", "Rust release details.");
    answer["content"][0]["annotations"] = json!([{"type":"url_citation","url":"https://example.org/rust","title":"Rust releases","start_index":0,"end_index":20}]);
    for path in ["/v1/codex/responses", "/v1/responses"] {
        let execution = execution(vec![session(
            "resp_omp_search",
            vec![search.clone(), answer.clone()],
        )]);
        let body = search_body();
        let events = sse(post(execution.clone(), path, "sk_omp_compat", &body).await).await;
        assert!(
            events
                .iter()
                .any(|e| e["type"] == "response.web_search_call.searching")
        );
        let done: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "response.output_item.done")
            .collect();
        assert_eq!(done[0]["item"], search);
        assert_eq!(done[0]["output_index"], 0);
        assert_eq!(done[1]["item"], answer);
        assert_eq!(done[1]["output_index"], 1);
        let terminal = &events.last().unwrap()["response"];
        assert_eq!(terminal["id"], "resp_omp_search");
        assert_eq!(terminal["model"], "model-a");
        assert_eq!(terminal["output"], json!([search, answer]));
        assert_eq!(
            terminal["usage"],
            json!({"input_tokens":10,"output_tokens":5,"total_tokens":15,"input_tokens_details":{"cached_tokens":2}})
        );
        let observed = execution.observed.lock().await;
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].endpoint, "/v1/responses");
        for key in [
            "tools",
            "tool_choice",
            "include",
            "input",
            "instructions",
            "store",
            "parallel_tool_calls",
        ] {
            assert_eq!(observed[0].body[key], body[key], "{path}: {key}");
        }
    }
}

#[tokio::test]
async fn omp_alias_search_failure_preserves_rate_limit() {
    let failure = json!({"type":"response.failed","response":{"id":"resp_omp_search","status":"failed","error":{"code":"rate_limit_exceeded","message":"search quota exhausted"}}});
    let raw = Bytes::from(format!("event: response.failed\r\ndata: {failure}\r\n\r\n"));
    let wire = ProviderEvent::wire(
        ProtocolWireEvent::json_with_raw_sse_metadata(
            "openai",
            Some("response.failed".to_owned()),
            failure.clone(),
            raw,
            None,
            None,
        )
        .unwrap(),
    );
    let execution = execution(vec![FakeSession::streaming(
        Arc::new(Trace::default()),
        vec![
            created("resp_omp_search"),
            NextStep::Event(delivery_provider(wire, CommitRequirement::AlreadyCommitted)),
            NextStep::Error(EngineError::Provider(ProviderError::new(
                ProviderErrorKind::RateLimited,
                UpstreamSendState::Sent,
            ))),
        ],
    )]);
    let events = sse(post(
        execution,
        "/v1/codex/responses",
        "sk_omp_compat",
        &search_body(),
    )
    .await)
    .await;
    let failures: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "response.failed")
        .collect();
    assert_eq!(failures, vec![&failure]);
}

#[tokio::test]
async fn omp_alias_rejects_invalid_client_key() {
    let execution = execution(vec![]);
    let response = post(
        execution.clone(),
        "/v1/codex/responses",
        "sk_invalid",
        &search_body(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let error: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(error["error"].is_object());
    assert!(execution.observed.lock().await.is_empty());
}

type OmpSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn listener(execution: Arc<OmpExecution>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let app = api_router(execution).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (address, server)
}

async fn connect(
    address: SocketAddr,
    key: &str,
) -> Result<(OmpSocket, axum::http::Response<Option<Vec<u8>>>), tokio_tungstenite::tungstenite::Error>
{
    let mut request = format!("ws://{address}/v1/codex/responses")
        .into_client_request()
        .unwrap();
    for (name, value) in [
        ("authorization", format!("Bearer {key}")),
        ("originator", "omp".to_owned()),
        ("user-agent", "omp/fixture".to_owned()),
        ("version", "0.155.1".to_owned()),
    ] {
        request.headers_mut().insert(name, value.parse().unwrap());
    }
    tokio::time::timeout(Duration::from_secs(2), connect_async(request))
        .await
        .expect("WebSocket handshake timeout")
}

async fn send(socket: &mut OmpSocket, body: Value) {
    tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(ClientMessage::Text(body.to_string().into())),
    )
    .await
    .unwrap()
    .unwrap();
}

async fn receive(socket: &mut OmpSocket) -> Value {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ClientMessage::Text(text) = socket.next().await.expect("socket open").unwrap() {
                return serde_json::from_str(&text).unwrap();
            }
        }
    })
    .await
    .expect("WebSocket response timeout")
}

async fn completed_frames(socket: &mut OmpSocket) -> Vec<Value> {
    let mut frames = Vec::new();
    loop {
        let frame = receive(socket).await;
        let done = frame["type"] == "response.completed";
        assert_ne!(frame["type"], "response.failed", "{frame}");
        frames.push(frame);
        if done {
            return frames;
        }
    }
}

#[tokio::test]
async fn omp_alias_websocket_tool_round_trip() {
    let call = json!({"type":"function_call","id":"fc_omp_1","call_id":"call_omp_1","name":"lookup","arguments":"{\"q\":\"hello\"}","status":"completed"});
    let execution = execution(vec![
        session("resp_omp_1", vec![call.clone()]),
        session(
            "resp_omp_2",
            vec![message("msg_omp_2", "tool-result received")],
        ),
    ]);
    let (address, server) = listener(execution.clone()).await;
    let error = connect(address, "sk_invalid").await.unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        panic!("expected HTTP authentication error");
    };
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let (mut socket, response) = connect(address, "sk_omp_compat").await.unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    send(&mut socket, json!({"type":"response.create","model":"model-a","store":false,"input":[{"role":"user","content":"hello"}],"tools":[{"type":"function","name":"lookup","parameters":{"type":"object","properties":{"q":{"type":"string"}}}}]})).await;
    let first = completed_frames(&mut socket).await;
    assert_eq!(first.last().unwrap()["response"]["id"], "resp_omp_1");
    assert!(
        first
            .iter()
            .any(|e| e["type"] == "response.output_item.done" && e["item"] == call)
    );
    let output =
        json!([{"type":"function_call_output","call_id":"call_omp_1","output":"lookup result"}]);
    send(&mut socket, json!({"type":"response.create","model":"model-a","store":false,"previous_response_id":"resp_omp_1","input":output})).await;
    let second = completed_frames(&mut socket).await;
    assert_eq!(second.last().unwrap()["response"]["id"], "resp_omp_2");
    assert_eq!(
        second.last().unwrap()["response"]["output"][0]["content"][0]["text"],
        "tool-result received"
    );
    {
        let observed = execution.observed.lock().await;
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[1].body["input"], output);
        assert_eq!(
            observed[1].previous_response_id.as_deref(),
            Some("resp_omp_1")
        );
        assert!(observed[0].context["downstream_websocket_connection_id"].is_string());
        assert_eq!(
            observed[0].context["downstream_websocket_connection_id"],
            observed[1].context["downstream_websocket_connection_id"]
        );
        assert!(observed.iter().all(|r| r.endpoint == "/v1/responses"));
    }
    println!("OMP WS smoke: 101; resp_omp_1 -> call_omp_1 -> resp_omp_2; tool-result received");
    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn omp_alias_steering_rejection_preserves_active_response() {
    let mut steps = success_steps(
        "resp_omp_active",
        vec![message("msg_active", "original response")],
    );
    let NextStep::Event(event) = steps.remove(1) else {
        unreachable!()
    };
    steps.insert(1, NextStep::DelayedEvent(Duration::from_millis(500), event));
    let execution = execution(vec![
        FakeSession::streaming(Arc::new(Trace::default()), steps),
        session(
            "resp_omp_next",
            vec![message("msg_next", "queued input received")],
        ),
    ]);
    let (address, server) = listener(execution.clone()).await;
    let (mut socket, _) = connect(address, "sk_omp_compat").await.unwrap();
    send(
        &mut socket,
        json!({"type":"response.create","model":"model-a","input":"start"}),
    )
    .await;
    loop {
        let frame = receive(&mut socket).await;
        if frame["type"] == "response.created" {
            break;
        }
    }
    let input =
        json!([{"role":"user","content":[{"type":"input_text","text":"also check citations"}]}]);
    send(
        &mut socket,
        json!({"type":"response.steer","previous_response_id":"resp_omp_active","input":input}),
    )
    .await;
    let ack = receive(&mut socket).await;
    assert_eq!(ack["type"], "response.steer.failed", "{ack}");
    assert_eq!(ack["steer"]["previous_response_id"], "resp_omp_active");
    assert_eq!(ack["error"]["code"], "unsupported_steering");
    let frames = completed_frames(&mut socket).await;
    assert_eq!(frames.last().unwrap()["response"]["id"], "resp_omp_active");
    assert!(
        !frames
            .iter()
            .any(|e| e["type"] == "response.steer.accepted" || e["type"] == "response.created")
    );
    assert_eq!(execution.observed.lock().await.len(), 1);
    send(&mut socket, json!({"type":"response.create","model":"model-a","previous_response_id":"resp_omp_active","input":input})).await;
    let next = completed_frames(&mut socket).await;
    assert_eq!(next.last().unwrap()["response"]["id"], "resp_omp_next");
    {
        let observed = execution.observed.lock().await;
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[1].body["input"], input);
    }
    // An idle rejection uses the same acknowledgement contract, not a queued create.
    send(
        &mut socket,
        json!({"type":"response.steer","previous_response_id":"resp_omp_next\nopaque","input":input}),
    )
    .await;
    let idle = receive(&mut socket).await;
    assert_eq!(idle["type"], "response.steer.failed");
    assert_eq!(
        idle["steer"]["previous_response_id"],
        "resp_omp_next\nopaque"
    );
    assert_eq!(execution.observed.lock().await.len(), 2);
    println!(
        "OMP steering smoke: rejected before completion; active response completed; queued input executed once"
    );
    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn omp_alias_compaction_preserves_output_and_replay() {
    // Matches buildCompactionV2Request + attemptCompactionV2Streaming in OMP.
    let compact = json!({"id":"cmp_omp_1","type":"compaction","encrypted_content":"fixture-encrypted-context"});
    let failure = json!({"type":"response.failed","response":{"id":"resp_omp_compact_fail","status":"failed","error":{"code":"invalid_request_error","message":"compaction is unavailable"}}});
    let failed = FakeSession::streaming(
        Arc::new(Trace::default()),
        vec![
            created("resp_omp_compact_fail"),
            event_step(failure.clone(), vec![], false),
            NextStep::Error(EngineError::Provider(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::Sent,
            ))),
        ],
    );
    let execution = execution(vec![
        session("resp_omp_compact", vec![compact.clone()]),
        session(
            "resp_omp_replay",
            vec![message("msg_replay", "context restored")],
        ),
        failed,
    ]);
    let body = json!({"model":"model-a","stream":true,"store":false,"instructions":"Summarize the conversation.","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"history"}]},{"type":"compaction_trigger"}],"include":["reasoning.encrypted_content"],"reasoning":{"effort":"medium","summary":"auto"},"prompt_cache_key":"omp-compaction-session"});
    let frames = sse(post(
        execution.clone(),
        "/v1/codex/responses",
        "sk_omp_compat",
        &body,
    )
    .await)
    .await;
    assert!(
        frames
            .iter()
            .any(|e| e["type"] == "response.output_item.done" && e["item"] == compact)
    );
    assert_eq!(
        frames.last().unwrap()["response"]["output"],
        json!([compact])
    );
    let replay = json!({"model":"model-a","stream":true,"store":false,"previous_response_id":"resp_omp_compact","prompt_cache_key":"omp-compaction-session","input":[compact,{"role":"user","content":"continue"}]});
    let frames = sse(post(
        execution.clone(),
        "/v1/codex/responses",
        "sk_omp_compat",
        &replay,
    )
    .await)
    .await;
    assert_eq!(
        frames.last().unwrap()["response"]["output"][0]["content"][0]["text"],
        "context restored"
    );
    let frames = sse(post(
        execution.clone(),
        "/v1/codex/responses",
        "sk_omp_compat",
        &body,
    )
    .await)
    .await;
    assert_eq!(
        frames
            .iter()
            .filter(|e| e["type"] == "response.failed")
            .collect::<Vec<_>>(),
        vec![&failure]
    );
    let observed = execution.observed.lock().await;
    assert_eq!(observed.len(), 3);
    for key in [
        "input",
        "instructions",
        "include",
        "reasoning",
        "prompt_cache_key",
        "store",
    ] {
        assert_eq!(observed[0].body[key], body[key], "{key}");
    }
    assert_eq!(observed[1].body["input"], replay["input"]);
    assert_eq!(
        observed[1].previous_response_id.as_deref(),
        Some("resp_omp_compact")
    );
    assert_eq!(
        observed[1].body["prompt_cache_key"],
        body["prompt_cache_key"]
    );
    assert!(observed.iter().all(|r| r.endpoint == "/v1/responses"));
}
