//! 验证 HTTP 与 WebSocket 的账号头部隔离及中间件覆盖

use super::*;
use crate::support::fedramp_secret;

const SELECTED_ACCOUNT: &str = "acct_scope_same";
const CLIENT_ACCOUNT_HEADERS: &[(&str, &[&str])] = &[
    (
        "traceparent",
        &["00-11111111111111111111111111111111-2222222222222222-01"],
    ),
    ("tracestate", &["vendor=client"]),
    ("sentry-trace", &["client-trace"]),
    ("baggage", &["environment=synthetic", "release=synthetic"]),
    ("x-openai-fedramp", &["true", "false"]),
];

fn client_account_metadata() -> Map<String, Value> {
    let mut metadata = Map::new();
    for (name, values) in CLIENT_ACCOUNT_HEADERS {
        metadata.insert((*name).to_owned(), json!(values[0]));
        metadata.insert(format!("ws_request_header_{name}"), json!(values[0]));
    }
    metadata
}

pub(super) fn isolation_history() -> Value {
    json!([
        {"role":"user", "content":"hello"},
        {"type":"function_call", "id":"fc_history", "call_id":"call_history", "name":"lookup", "arguments":"{}"},
        {
            "type":"function_call_output", "call_id":"call_history", "output":"tool result",
            "internal_chat_message_metadata_passthrough": {
                "executed_tool_calls": [{
                    "name":"lookup", "arguments":{},
                    "tool_result_metadata": {
                        "openai/resource_access":{"resource_coverage":"complete", "resources":[]},
                        "parent_response_id":"tool-business-extension"
                    }
                }]
            }
        },
        {"role":"user", "content":"continue"}
    ])
}

fn isolation_payload() -> ProtocolPayload {
    let mut turn_metadata = client_account_metadata();
    turn_metadata.insert("parent_response_id".to_owned(), json!("client-correlation"));
    turn_metadata.insert("workspace".to_owned(), json!("/工作区"));
    turn_metadata.insert("future".to_owned(), json!({"traceparent":"business-value"}));
    let mut frame_metadata = turn_metadata.clone();
    frame_metadata.insert("tool_namespaces_info".into(), json!({"workspace":{}}));
    let mut body = json!({
        "model": "gpt-5.4",
        "input": isolation_history(),
        "client_metadata": {
            "parent_response_id": "resp_parent_old",
            "guardian_credits_requested": "true",
            "x-codex-turn-metadata": Value::Object(frame_metadata).to_string(),
            "turnMetadata": turn_metadata,
            "mcp_attribution": r#"{"status":"complete","sources":[]}"#
        }
    });
    body.as_object_mut()
        .unwrap()
        .extend(client_account_metadata());
    body["client_metadata"]
        .as_object_mut()
        .unwrap()
        .extend(client_account_metadata());
    body["turnMetadata"] = json!(turn_metadata);
    body["turn_metadata"] = json!(Value::Object(turn_metadata.clone()).to_string());
    let mut headers = vec![
        json!(["X-OpenAI-Account-Routing-Override", STANDARD.encode("us")]),
        json!([
            "x-openai-account-routing-override",
            STANDARD.encode("us_cr")
        ]),
        json!(["x-codex-guardian", STANDARD.encode("true")]),
        json!(["x-business-extension", STANDARD.encode("first")]),
        json!(["x-business-extension", STANDARD.encode("second")]),
    ];
    for (name, values) in CLIENT_ACCOUNT_HEADERS {
        for value in *values {
            headers.push(json!([name.to_ascii_uppercase(), STANDARD.encode(value)]));
        }
    }
    ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
        .unwrap()
        .with_context(Map::from_iter([
            (
                gateway_protocol::openai::WEBSOCKET_FRAME_CONTEXT_KEY.into(),
                json!(true),
            ),
            ("opaque_request_headers".to_owned(), json!(headers)),
            (
                "turn_metadata".to_owned(),
                json!(Value::Object(turn_metadata).to_string()),
            ),
        ]))
}

fn isolation_context(owner: Option<&str>) -> AttemptContext {
    match owner {
        Some(owner) => context_with_state_owner("req_account_isolation", owner),
        None => context("req_account_isolation", CancellationToken::new()),
    }
}

fn assert_isolated_request(
    headers: &reqwest::header::HeaderMap,
    body: &Value,
    owner: Option<&str>,
) {
    assert!(!headers.contains_key("x-openai-account-routing-override"));
    let switched = owner.is_some_and(|owner| owner != SELECTED_ACCOUNT);
    for (name, values) in CLIENT_ACCOUNT_HEADERS {
        let actual: Vec<_> = headers
            .get_all(*name)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        let expected = if switched { &[][..] } else { *values };
        assert_eq!(actual, expected, "{name}, owner={owner:?}");
        for metadata in [body, &body["client_metadata"]] {
            let expected = (!switched).then(|| json!(values[0]));
            assert_eq!(metadata.get(*name), expected.as_ref());
            assert_eq!(
                metadata.get(format!("ws_request_header_{name}")),
                expected.as_ref()
            );
        }
    }
    assert_eq!(headers["x-codex-guardian"], "true");
    assert_eq!(
        headers
            .get_all("x-business-extension")
            .iter()
            .map(|value| value.as_bytes())
            .collect::<Vec<_>>(),
        vec![b"first".as_slice(), b"second".as_slice()],
    );
    assert_eq!(
        body.pointer("/client_metadata/parent_response_id")
            .and_then(Value::as_str),
        (owner == Some(SELECTED_ACCOUNT)).then_some("resp_parent_old"),
    );
    assert_eq!(
        body["client_metadata"]["guardian_credits_requested"],
        "true"
    );
    let mut expected_turn_metadata = if switched {
        Map::new()
    } else {
        client_account_metadata()
    };
    expected_turn_metadata.insert("parent_response_id".to_owned(), json!("client-correlation"));
    expected_turn_metadata.insert("workspace".to_owned(), json!("/工作区"));
    expected_turn_metadata.insert("future".to_owned(), json!({"traceparent":"business-value"}));
    for metadata in [
        &body["turnMetadata"],
        &body["turn_metadata"],
        &body["client_metadata"]["turnMetadata"],
    ] {
        let actual = match metadata {
            Value::String(raw) => serde_json::from_str(raw).unwrap(),
            value => value.clone(),
        };
        assert_eq!(actual, Value::Object(expected_turn_metadata.clone()));
    }
    let mut expected_frame_metadata = expected_turn_metadata.clone();
    expected_frame_metadata.insert("tool_namespaces_info".into(), json!({"workspace":{}}));
    let frame = body["client_metadata"]["x-codex-turn-metadata"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(frame).unwrap(),
        Value::Object(expected_frame_metadata)
    );
    let header = headers["x-codex-turn-metadata"].to_str().unwrap();
    assert!(header.is_ascii());
    assert_eq!(
        serde_json::from_str::<Value>(header).unwrap(),
        Value::Object(expected_turn_metadata)
    );
    assert_eq!(
        body["client_metadata"]["mcp_attribution"],
        r#"{"status":"complete","sources":[]}"#
    );
    assert_eq!(body["input"], isolation_history());
}

#[tokio::test]
async fn rotated_fedramp_claim_opens_a_new_socket_for_the_same_account_and_session() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, SELECTED_ACCOUNT).await;
    let server = RoutingUpstream::start(Some(Vec::new())).await;
    let provider = provider_with_base_url(&store, server.url.clone());
    let mut session = ProviderSessionState::new(
        "openai",
        Map::from_iter([
            ("account_id".into(), json!(SELECTED_ACCOUNT)),
            ("conversation_id".into(), json!("fedramp-session")),
            ("continuation_scope".into(), json!("connection_local")),
        ]),
    )
    .unwrap();
    for (id, exact) in [
        ("resp_fedramp_seed", false),
        ("resp_fedramp_reuse", false),
        ("resp_old_handle", true),
        ("resp_fedramp_rotated", false),
    ] {
        if exact {
            let account = store
                .get_account(&ProviderAccountId::new(SELECTED_ACCOUNT).unwrap())
                .await
                .unwrap()
                .unwrap();
            let rotated = fedramp_secret(&format!("at-{SELECTED_ACCOUNT}"));
            store
                .repository()
                .rotate_refreshed_oauth_secret(&account, rotated, None, None)
                .await
                .unwrap();
        }
        let mut body = Map::from_iter([
            ("model".into(), json!("gpt-5.4")),
            ("input".into(), json!([])),
            ("store".into(), json!(false)),
            ("client_metadata".into(), json!({"fixture_response_id": id})),
        ]);
        let context = if exact {
            body.insert("previous_response_id".into(), json!("resp_fedramp_reuse"));
            pinned_continuation_context(
                "req_fedramp_exact",
                SELECTED_ACCOUNT,
                "resp_fedramp_reuse",
                "resp_fedramp_reuse",
                1,
                ContinuationAttempt::Native,
            )
        } else {
            isolation_context(None)
        };
        let payload = ProtocolPayload::json_object("openai", body)
            .unwrap()
            .with_context(Map::from_iter([
                ("use_websocket".into(), json!(true)),
                (
                    "downstream_websocket_connection_id".into(),
                    json!("fedramp-connection"),
                ),
            ]));
        let operation = Operation::Generate(
            GenerateRequest::from_protocol_payload(payload)
                .with_provider_session_state(session.clone()),
        );
        let mut stream = Arc::clone(&provider)
            .execute(planned_request("openai", operation), context)
            .await
            .unwrap();
        if exact {
            let error = loop {
                match stream.next().await {
                    Some(Ok(event)) => assert!(!event.has_client_event()),
                    Some(Err(error)) => break error,
                    None => panic!("old route must reject the exact continuation before sending"),
                }
            };
            assert_eq!(
                error.continuation_recovery_disposition(),
                Some(ContinuationRecoveryDisposition::ClientReplayRequired)
            );
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
            assert_eq!(
                error.continuation_unavailable_reason(),
                Some("fresh_connection_required")
            );
        } else {
            while let Some(event) = stream.next().await {
                let event = event.unwrap();
                if let Some(update) = event.session_update() {
                    session = update.clone();
                }
            }
        }
    }
    let captured = server.requests.lock().unwrap();
    let [seed, reuse, fresh] = captured.as_slice() else {
        panic!("only the three compatible frames should be sent")
    };
    assert_eq!(seed.0, reuse.0);
    assert_ne!(reuse.0, fresh.0);
    assert!(!seed.1.contains_key("x-openai-fedramp"));
    assert_eq!(fresh.1["x-openai-fedramp"], "true");
}

#[tokio::test]
async fn selected_oauth_fedramp_claim_sets_a_single_header_even_after_account_switch() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: SELECTED_ACCOUNT.to_owned(),
            name: SELECTED_ACCOUNT.to_owned(),
            secret: fedramp_secret("at-fedramp"),
            verified_account: profile("chatgpt-fedramp"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(2)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    for owner in [SELECTED_ACCOUNT, "acct_scope_old"] {
        let payload = isolation_payload();
        let mut metadata = payload.context().clone();
        metadata.insert("use_websocket".into(), json!(false));
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            payload.with_context(metadata),
        ));
        let mut stream = provider
            .clone()
            .execute(
                planned_request("openai", operation),
                isolation_context(Some(owner)),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    for request in server.received_requests().await.unwrap() {
        let values: Vec<_> = request.headers.get_all("x-openai-fedramp").iter().collect();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0], "true");
    }
}

#[tokio::test]
async fn http_scopes_client_headers_and_guardian_parent_reference_to_the_account() {
    for owner in [Some(SELECTED_ACCOUNT), Some("acct_scope_old"), None] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, SELECTED_ACCOUNT).await;
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
        let mut payload = isolation_payload();
        let mut context = payload.context().clone();
        context.insert("use_websocket".to_owned(), json!(false));
        payload = payload.with_context(context);
        let mut stream = provider_with_base_url(&store, server.uri())
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
                ),
                isolation_context(owner),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_isolated_request(
            &requests[0].headers,
            &captured_request_body(&requests[0]),
            owner,
        );
    }
}

#[tokio::test]
async fn websocket_scopes_client_headers_and_guardian_parent_reference_to_the_account() {
    for owner in [Some(SELECTED_ACCOUNT), Some("acct_scope_old"), None] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, SELECTED_ACCOUNT).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut headers = None;
            let mut socket =
                crate::transport::accept_codex_test_websocket_with(socket, |request, _| {
                    headers = Some(request.headers().clone());
                })
                .await;
            let message = socket.next().await.unwrap().unwrap();
            let body: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            socket.send(Message::Text(json!({
                "type": "response.completed",
                "response": {"id":"resp_isolation", "model":"gpt-5.4", "status":"completed", "output":[]}
            }).to_string().into())).await.unwrap();
            (headers.unwrap(), body)
        });
        let mut stream = provider_with_base_url(&store, base_url)
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(
                        GenerateRequest::from_protocol_payload(isolation_payload()),
                    ),
                ),
                isolation_context(owner),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let (headers, body) = server.await.unwrap();
        assert_isolated_request(&headers, &body, owner);
    }
}

#[tokio::test]
async fn middleware_can_override_account_headers_on_http_and_websocket() {
    for websocket in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, SELECTED_ACCOUNT).await;
        let (base_url, capture): (
            String,
            futures::future::BoxFuture<'static, reqwest::header::HeaderMap>,
        ) = if websocket {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut headers = None;
                let mut socket =
                    crate::transport::accept_codex_test_websocket_with(socket, |request, _| {
                        headers = Some(request.headers().clone());
                    })
                    .await;
                socket.next().await.unwrap().unwrap();
                socket.send(Message::Text(json!({"type":"response.completed","response":{"id":"resp_plugin_headers","model":"gpt-5.4","status":"completed","output":[]}}).to_string().into())).await.unwrap();
                headers.unwrap()
            });
            (base_url, Box::pin(async { task.await.unwrap() }))
        } else {
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
            (
                server.uri(),
                Box::pin(
                    async move { server.received_requests().await.unwrap()[0].headers.clone() },
                ),
            )
        };
        let names = [
            "x-openai-account-routing-override",
            "x-openai-fedramp",
            "x-codex-installation-id",
        ];
        let mut stream = provider_with_base_url(&store, base_url)
            .execute(
                planned_request(
                    "openai",
                    if websocket {
                        generate_operation()
                    } else {
                        http_generate_operation()
                    },
                ),
                context_with_middleware(
                    "req_plugin_account_header",
                    Arc::new(RecordingMiddleware {
                        observed: Arc::default(),
                        replacement: ("service_tier".into(), json!("default")),
                        request_headers: names
                            .iter()
                            .map(|name| {
                                MiddlewareHeader::new(*name, Bytes::from_static(b"plugin-value"))
                            })
                            .collect(),
                    }),
                    FastMode::Default,
                ),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let headers = capture.await;
        for name in names {
            assert_eq!(headers[name], "plugin-value");
        }
    }
}

struct RoutingUpstream {
    url: String,
    requests: Arc<Mutex<Vec<(usize, reqwest::header::HeaderMap, Value)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for RoutingUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RoutingUpstream {
    async fn start(cookies: Option<Vec<String>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            let mut index = 0;
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let capture = Arc::clone(&capture);
                let cookies = cookies.clone().unwrap_or_else(|| {
                    vec![
                        format!("__oailb=pin-{index}; Path=/codex/responses; Max-Age=300"),
                        format!("__cflb=edge-{index}; Path=/; Max-Age=300"),
                    ]
                });
                connections.spawn(async move {
                    let mut headers = reqwest::header::HeaderMap::new();
                    let mut socket = crate::transport::accept_codex_test_websocket_with(socket, |request, response| {
                        headers = request.headers().clone();
                        for cookie in cookies {
                            response.headers_mut().append("set-cookie", cookie.parse().unwrap());
                        }
                    }).await;
                    while let Some(Ok(message)) = socket.next().await {
                        let Message::Text(text) = message else { continue };
                        let body: Value = serde_json::from_str(&text).unwrap();
                        let id = body["client_metadata"]["fixture_response_id"].as_str().unwrap().to_owned();
                        capture.lock().unwrap().push((index, headers.clone(), body));
                        for event in ["response.created", "response.completed"] {
                            socket.send(Message::Text(json!({
                                "type":event, "response":{"id":id,"model":"gpt-5.4","status":"completed","output":[]}
                            }).to_string().into())).await.unwrap();
                        }
                    }
                });
                index += 1;
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

fn routing_operation(id: &str, parent: Option<&str>, conversation: Option<&str>) -> Operation {
    let mut metadata = json!({"fixture_response_id":id});
    if let Some(parent) = parent {
        metadata["parent_response_id"] = json!(parent);
        metadata["x-openai-subagent"] = json!("guardian");
    }
    let mut protocol_context = Map::from_iter([("use_websocket".to_owned(), json!(true))]);
    if parent.is_some() {
        protocol_context.insert(
            "opaque_request_headers".to_owned(),
            json!([["x-codex-guardian", "cmV2aWV3ZXI="]]),
        );
    }
    if let Some(conversation) = conversation {
        protocol_context.insert(
            "downstream_websocket_connection_id".to_owned(),
            json!(conversation),
        );
    }
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({
            "model":"gpt-5.4", "input":"hello", "client_metadata":metadata
        })
        .as_object()
        .unwrap()
        .clone(),
    )
    .unwrap()
    .with_context(protocol_context);
    let mut generate = GenerateRequest::from_protocol_payload(payload);
    if let Some(conversation) = conversation {
        generate = generate.with_provider_session_state(ProviderSessionState::new("openai", json!({
            "account_id":SELECTED_ACCOUNT, "conversation_id":conversation, "continuation_scope":"persisted"
        }).as_object().unwrap().clone()).unwrap());
    }
    Operation::Generate(generate)
}

fn routing_context(id: &str, key: &str, account: &str) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(format!("req_{id}")).unwrap(),
            ClientApiKeyId::new(key).unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(
            BTreeSet::new(),
            Some(ProviderAccountId::new(account).unwrap()),
            None,
        )
        .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

async fn execute_routing(
    provider: &Arc<CodexProvider>,
    operation: Operation,
    context: AttemptContext,
) {
    let mut stream = Arc::clone(provider)
        .execute(planned_request("openai", operation), context)
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
}

#[tokio::test]
async fn guardian_pins_follow_parent_connection_and_separate_reviewer_pool_reuse() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, SELECTED_ACCOUNT).await;
    let server = RoutingUpstream::start(None).await;
    let provider = provider_with_base_url(&store, server.url.clone());
    for (id, parent, conversation) in [
        ("parent_a", None, "root_a"),
        ("parent_b", None, "root_b"),
        ("review_a", Some("parent_a"), "reviewer"),
        ("review_a_again", Some("parent_a"), "reviewer"),
        ("review_b", Some("parent_b"), "reviewer"),
    ] {
        execute_routing(
            &provider,
            routing_operation(id, parent, Some(conversation)),
            routing_context(id, "key_routing", SELECTED_ACCOUNT),
        )
        .await;
    }
    let payload = ProtocolPayload::json_object("openai", json!({
        "model":"gpt-5.4","input":"hello","client_metadata":{
            "fixture_response_id":"classifier","parent_response_id":"parent_a","x-openai-subagent":"guardian",
            "x-codex-turn-metadata":r#"{"thread_source":"guardian_classifier","turn_trigger":"guardian_classifier"}"#}
    }).as_object().unwrap().clone()).unwrap().with_context(Map::from_iter([("use_websocket".to_owned(),json!(true))]));
    execute_routing(
        &provider,
        Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
        routing_context("classifier", "key_routing", SELECTED_ACCOUNT),
    )
    .await;
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[5].1["x-codex-guardian"], "classifier");
    assert_eq!(
        requests[5].2["client_metadata"]["parent_response_id"],
        "parent_a"
    );
    assert!(!requests[5].1.contains_key("cookie"));
    assert_eq!(
        requests[2].0, requests[3].0,
        "same parent keeps the reviewer socket"
    );
    assert_ne!(
        requests[3].0, requests[4].0,
        "another parent needs its own routed socket"
    );
    for (review, parent) in [(2, 0), (3, 0), (4, 1)] {
        let cookie = requests[review].1["cookie"].to_str().unwrap();
        assert!(cookie.contains(&format!("__oailb=pin-{parent}")));
        assert!(cookie.contains(&format!("__cflb=edge-{parent}")));
    }
    assert!(
        !requests[1].1.contains_key("cookie"),
        "ordinary traffic cannot inherit another connection's pins"
    );
}

#[tokio::test]
async fn guardian_structured_roles_use_selected_model_and_observed_http_parent_without_cookies() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!("data: {}\n\n{}", json!({"type":"response.created","response":{"id":"resp_scope_capture","model":"gpt-5.4"}}), CAPTURE_COMPLETED_SSE),
                "text/event-stream",
            ),
        )
        .expect(5)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    for (id, model, role, parent) in [
        ("root", "gpt-5.4", None, None),
        (
            "review",
            "codex-auto-review",
            Some("guardian_review"),
            Some("resp_scope_capture"),
        ),
        (
            "unobserved",
            "codex-auto-review",
            Some("guardian_review"),
            Some("unknown"),
        ),
        (
            "classifier",
            "gpt-5.4",
            Some("guardian_classifier"),
            Some("resp_scope_capture"),
        ),
        ("custom", "gpt-5.4", Some("guardian_review"), None),
    ] {
        let mut body = json!({"model":"gpt-5.6-luna","input":[]});
        if let Some(role) = role {
            body["client_metadata"] = json!({
                "x-openai-subagent":"guardian",
                "x-codex-turn-metadata": json!({"thread_source":role}).to_string(),
                "guardian_credits_requested":"true"
            });
        }
        if let Some(parent) = parent {
            body["client_metadata"]["parent_response_id"] = json!(parent);
        }
        let payload = ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))]));
        let op = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
        let mut stream = Arc::clone(&provider)
            .execute(
                planned_request_with_presentation("openai", op, model, None),
                context(&format!("req_{id}"), CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let requests = server.received_requests().await.unwrap();
    for (index, expected_role, parent) in [
        (0, None, None),
        (1, Some("reviewer"), Some("resp_scope_capture")),
        (2, Some("reviewer"), None),
        (3, Some("classifier"), Some("resp_scope_capture")),
        (4, None, None),
    ] {
        let request = &requests[index];
        let body = captured_request_body(request);
        assert_eq!(
            request
                .headers
                .get("x-codex-guardian")
                .map(|h| h.to_str().unwrap()),
            expected_role
        );
        assert_eq!(
            request.headers.contains_key("x-codex-routing-hint"),
            expected_role.is_none()
        );
        assert_eq!(
            body.pointer("/client_metadata/parent_response_id")
                .and_then(Value::as_str),
            parent
        );
        if expected_role.is_some() {
            assert!(
                body.pointer("/client_metadata/guardian_credits_requested")
                    .is_none()
            );
        }
        assert!(!request.headers.contains_key("cookie"));
    }
}

#[tokio::test]
async fn guardian_parent_pins_require_matching_client_key_and_selected_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, SELECTED_ACCOUNT).await;
    create_account(&store, "acct_scope_old").await;
    let server = RoutingUpstream::start(None).await;
    let provider = provider_with_base_url(&store, server.url.clone());
    execute_routing(
        &provider,
        routing_operation("parent", None, None),
        routing_context("parent", "key_routing", SELECTED_ACCOUNT),
    )
    .await;
    for (id, key, account, parent) in [
        ("same", "key_routing", SELECTED_ACCOUNT, "parent"),
        ("other_key", "key_other", SELECTED_ACCOUNT, "parent"),
        ("other_account", "key_routing", "acct_scope_old", "parent"),
        ("unknown", "key_routing", SELECTED_ACCOUNT, "unknown_parent"),
    ] {
        execute_routing(
            &provider,
            routing_operation(id, Some(parent), None),
            routing_context(id, key, account),
        )
        .await;
    }
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    for (index, (_, headers, body)) in requests.iter().enumerate().skip(1) {
        assert_eq!(headers.contains_key("cookie"), index == 1);
        assert_eq!(
            body.pointer("/client_metadata/parent_response_id")
                .and_then(Value::as_str),
            (index == 1).then_some("parent")
        );
    }
}

#[tokio::test]
async fn guardian_cookie_expiry_keeps_existing_socket_but_does_not_refresh_parent_pin() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, SELECTED_ACCOUNT).await;
    let server = RoutingUpstream::start(Some(vec![
        "__oailb=short_lived; Path=/; Max-Age=1".to_owned(),
    ]))
    .await;
    let provider = provider_with_base_url(&store, server.url.clone());
    for (id, parent, conversation) in [
        ("parent_a", None, "root"),
        ("review_a", Some("parent_a"), "reviewer"),
    ] {
        execute_routing(
            &provider,
            routing_operation(id, parent, Some(conversation)),
            routing_context(id, "key_routing", SELECTED_ACCOUNT),
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    for (id, parent, conversation) in [
        ("parent_b", None, "root"),
        ("review_b", Some("parent_b"), "reviewer"),
        ("review_new", Some("parent_b"), "another_reviewer"),
    ] {
        execute_routing(
            &provider,
            routing_operation(id, parent, Some(conversation)),
            routing_context(id, "key_routing", SELECTED_ACCOUNT),
        )
        .await;
    }
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0].0, requests[2].0);
    assert_eq!(requests[1].0, requests[3].0);
    assert_ne!(requests[3].0, requests[4].0);
    assert!(requests[1].1.contains_key("cookie"));
    assert!(!requests[4].1.contains_key("cookie"));
    assert_eq!(
        requests[4].2["client_metadata"]["parent_response_id"],
        "parent_b"
    );
}

#[tokio::test]
async fn guardian_ignores_expired_ambiguous_and_wrong_path_routing_cookies() {
    for cookies in [
        vec!["__oailb=expired; Path=/; Max-Age=0"],
        vec!["__oailb=first; Path=/", "__oailb=second; Path=/"],
        vec!["__oailb=other; Path=/other"],
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, SELECTED_ACCOUNT).await;
        let server =
            RoutingUpstream::start(Some(cookies.into_iter().map(str::to_owned).collect())).await;
        let provider = provider_with_base_url(&store, server.url.clone());
        for (id, parent) in [("parent", None), ("review", Some("parent"))] {
            execute_routing(
                &provider,
                routing_operation(id, parent, None),
                routing_context(id, "key_routing", SELECTED_ACCOUNT),
            )
            .await;
        }
        let requests = server.requests.lock().unwrap();
        assert!(!requests[1].1.contains_key("cookie"));
        assert_eq!(
            requests[1].2["client_metadata"]["parent_response_id"],
            "parent"
        );
    }
}
