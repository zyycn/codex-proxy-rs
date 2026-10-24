//! 账号追踪域的稳定性、轮次原值透传、HTTP/WS 一致性与工具端点归属回归

use super::*;

#[derive(Debug)]
struct ParametersMiddleware(Map<String, Value>);

impl MiddlewarePlan for ParametersMiddleware {
    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        assert_eq!(context.mount(), MiddlewareMount::Attempt);
        assert!(request.provider_parameters().unwrap()["installation_id"].is_string());
        next.run(request.with_provider_parameters(self.0.clone()))
    }
}

#[tokio::test]
async fn request_parameters_drive_native_identity_and_derivation_across_http_and_websocket() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let identities = [
        "f77b9b8e-563a-4d84-a66f-f2a0c98d5a8e",
        "f6e974b5-708d-4940-b619-1ab2c58c2f2c",
    ];
    let mut captures = Vec::new();
    for identity in identities {
        for websocket in [false, true] {
            let (base_url, capture): (
                String,
                BoxFuture<'static, (reqwest::header::HeaderMap, Value)>,
            ) = if websocket {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let task = tokio::spawn(async move {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut headers = None;
                    let mut socket =
                        crate::transport::accept_codex_test_websocket_with(socket, |request, _| {
                            headers = Some(request.headers().clone())
                        })
                        .await;
                    let frame = socket.next().await.unwrap().unwrap();
                    let body = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    socket.send(Message::Text(json!({"type":"response.completed", "response":{
                            "id":"resp_parameters", "model":"gpt-5.4", "status":"completed", "output":[]
                        }}).to_string().into())).await.unwrap();
                    (headers.unwrap(), body)
                });
                (url, Box::pin(async { task.await.unwrap() }))
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
                    Box::pin(async move {
                        let requests = server.received_requests().await.unwrap();
                        (
                            requests[0].headers.clone(),
                            captured_request_body(&requests[0]),
                        )
                    }),
                )
            };
            let payload = payload(websocket);
            let mut request_context = payload.context().clone();
            let mut turn_metadata = metadata();
            turn_metadata["installation_id"] = json!("client-installation");
            request_context.insert("turn_metadata".into(), json!(turn_metadata.to_string()));
            let mut stream = provider_with_base_url(&store, base_url)
                .execute(
                    planned_request(
                        "openai",
                        Operation::Generate(GenerateRequest::from_protocol_payload(
                            payload.with_context(request_context),
                        )),
                    ),
                    context_with_middleware(
                        "req_parameters",
                        Arc::new(ParametersMiddleware(Map::from_iter([(
                            "installation_id".into(),
                            json!(identity),
                        )]))),
                        FastMode::Default,
                    ),
                )
                .await
                .unwrap();
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                "acct_provider_contract"
            );
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
            let (headers, body) = capture.await;
            assert_eq!(body["client_metadata"]["x-codex-installation-id"], identity);
            let metadata: Value =
                serde_json::from_str(headers["x-codex-turn-metadata"].to_str().unwrap()).unwrap();
            assert_eq!(metadata["installation_id"], identity);
            assert_eq!(
                headers["session-id"].to_str().unwrap(),
                body["client_metadata"]["session_id"]
            );
            captures.push((
                body["client_metadata"]["session_id"].clone(),
                body["prompt_cache_key"].clone(),
            ));
        }
    }
    assert_eq!(captures[0], captures[1]);
    assert_eq!(captures[2], captures[3]);
    assert_ne!(captures[0].0, captures[2].0);
    assert_ne!(captures[0].1, captures[2].1);
}

#[tokio::test]
async fn request_parameters_partition_new_websocket_chains_without_preventing_reuse() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let identities = ["parameters-first", "parameters-second"];
    let server = tokio::spawn(async move {
        let mut retained = Vec::new();
        for (index, identity) in identities.into_iter().enumerate() {
            let (socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut socket =
                crate::transport::accept_codex_test_websocket_with(socket, |request, _| {
                    let metadata: Value = serde_json::from_str(
                        request.headers()["x-codex-turn-metadata"].to_str().unwrap(),
                    )
                    .unwrap();
                    assert_eq!(metadata["installation_id"], identity);
                })
                .await;
            for turn in 0..=index {
                let text = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        match socket.next().await.unwrap().unwrap() {
                            Message::Text(text) => break text,
                            Message::Ping(payload) => {
                                socket.send(Message::Pong(payload)).await.unwrap()
                            }
                            message => panic!("unexpected websocket frame: {message:?}"),
                        }
                    }
                })
                .await
                .unwrap();
                let body: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(body["client_metadata"]["x-codex-installation-id"], identity);
                socket.send(Message::Text(json!({"type":"response.completed", "response":{
                    "id":format!("resp_parameters_{index}_{turn}"), "model":"gpt-5.4", "status":"completed", "output":[]
                }}).to_string().into())).await.unwrap();
            }
            // 保持旧连接可复用，证明新身份的隔离不是由测试服务端断开触发
            retained.push(socket);
        }
    });
    let provider = provider_with_base_url(&store, url);
    for identity in [identities[0], identities[1], identities[1]] {
        let payload = payload(true);
        let mut request_context = payload.context().clone();
        request_context.insert(
            "turn_metadata".into(),
            json!(json!({"installation_id":"client-installation"}).to_string()),
        );
        let mut stream = Arc::clone(&provider)
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(
                        GenerateRequest::from_protocol_payload(
                            payload.with_context(request_context),
                        )
                        .with_provider_session_state(
                            ProviderSessionState::new(
                                "openai",
                                Map::from_iter([
                                    ("account_id".into(), json!("acct_provider_contract")),
                                    ("conversation_id".into(), json!("parameters-conversation")),
                                    ("continuation_scope".into(), json!("persisted")),
                                ]),
                            )
                            .unwrap(),
                        ),
                    ),
                ),
                context_with_middleware(
                    "req_parameters_pool",
                    Arc::new(ParametersMiddleware(Map::from_iter([(
                        "installation_id".into(),
                        json!(identity),
                    )]))),
                    FastMode::Default,
                ),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
        })
        .await
        .unwrap();
    }
    server.await.unwrap();
}

#[tokio::test]
async fn request_parameters_apply_to_images_and_search_without_reencoding_opaque_bodies() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
        .expect(3)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let body = Bytes::from_static(br#"{ "future":1, "future":2 }"#);
    let raw = RawJsonPayload::new("openai", body.clone())
        .unwrap()
        .with_context(Map::from_iter([(
            "turn_metadata".into(),
            json!(json!({"installation_id":"client-installation"}).to_string()),
        )]));
    for operation in [
        Operation::GenerateImage(ImageRequest::from_raw_json(
            ImageRequestKind::Generation,
            raw.clone(),
        )),
        Operation::GenerateImage(ImageRequest::from_raw_json(
            ImageRequestKind::Edit,
            raw.clone(),
        )),
        Operation::Search(StandaloneSearchRequest::from_raw_json(raw)),
    ] {
        let mut stream = Arc::clone(&provider)
            .execute(
                planned_provider_endpoint_request("openai", operation),
                context_with_middleware(
                    "req_parameters_json",
                    Arc::new(ParametersMiddleware(Map::from_iter([(
                        "installation_id".into(),
                        json!("plugin-installation"),
                    )]))),
                    FastMode::Default,
                ),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    for request in server.received_requests().await.unwrap() {
        assert_eq!(request.body.as_slice(), body.as_ref());
        let metadata: Value =
            serde_json::from_str(request.headers["x-codex-turn-metadata"].to_str().unwrap())
                .unwrap();
        assert_eq!(metadata["installation_id"], "plugin-installation");
    }
}

#[tokio::test]
async fn invalid_request_parameters_fail_without_account_identity_fallback_or_send() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let provider = provider_with_base_url(&store, server.uri());
    for parameters in [
        json!({}),
        json!({"installation_id":null}),
        json!({"installation_id":42}),
    ] {
        let error = provider
            .clone()
            .execute(
                planned_request("openai", http_generate_operation()),
                context_with_middleware(
                    "req_invalid_parameters",
                    Arc::new(ParametersMiddleware(
                        parameters.as_object().unwrap().clone(),
                    )),
                    FastMode::Default,
                ),
            )
            .await
            .err()
            .expect("invalid parameters must fail");
        assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        assert!(error.retry_is_prohibited());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
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
    let mut stream = provider
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_after_invalid_parameters", CancellationToken::new()),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
}

fn metadata() -> Value {
    json!({"session_id":"source-session", "thread_id":"source-thread", "turn_id":"source-turn",
        "parent_thread_id":"source-session", "forked_from_thread_id":"source-session",
        "parent_turn_id":"source-turn", "root_turn_id":"source-turn",
        "window_id":"source-window", "context_window_id":"source-window", "future":{"keep":true}})
}

fn payload(websocket: bool) -> ProtocolPayload {
    let mut full = metadata();
    full["tool_namespaces_info"] = json!({"inventory":"body-only"});
    let mut input = account_isolation::isolation_history()
        .as_array()
        .unwrap()
        .clone();
    for (index, item) in input.iter_mut().enumerate() {
        let metadata = item
            .as_object_mut()
            .unwrap()
            .entry("internal_chat_message_metadata_passthrough")
            .or_insert_with(|| json!({}));
        metadata["turn_id"] = json!(if index == 3 {
            "source-turn"
        } else {
            "source-previous-turn"
        });
    }
    input.insert(0, json!({"role":"user", "content":[{"type":"input_text", "text":concat!(
        "<external_codex_git_state><environment_context><git>",
        r#"{"sessionId":"source-session","threadId":"source-thread","clientUserMessageId":"source-turn","path":"/workspace/project"}"#,
        "</git></environment_context></external_codex_git_state>"
    )}], "internal_chat_message_metadata_passthrough":{
        "turn_id":"source-turn", "content_item_kinds":["additional_content.codex_git_state"]
    }}));
    ProtocolPayload::json_object("openai", json!({
        "model":"gpt-5.4", "input":input,
        "prompt_cache_key":"custom-cache-key", "client_metadata":{
            "session_id":"source-session", "thread_id":"source-thread", "turn_id":"source-turn",
            "x-client-turn-id":"source-turn",
            "x-codex-window-id":"source-window", "x-codex-parent-thread-id":"source-session",
            "x-codex-turn-metadata":full.to_string(),
            "ws_request_header_traceparent":"00-11111111111111111111111111111111-2222222222222222-01",
            "ws_request_header_tracestate":"vendor=source"
        }
    }).as_object().unwrap().clone()).unwrap().with_context(Map::from_iter([
        ("use_websocket".into(), json!(websocket)),
        ("session_id".into(), json!("source-session")),
        ("thread_id".into(), json!("source-thread")),
        ("turn_metadata".into(), json!(metadata().to_string())),
        ("opaque_request_headers".into(), json!([
            ["x-client-turn-id",STANDARD.encode("source-turn")],
            ["traceparent",STANDARD.encode("00-11111111111111111111111111111111-2222222222222222-01")],
            ["tracestate",STANDARD.encode("vendor=source")]
        ])),
    ]))
}

#[tokio::test]
async fn account_correlation_preserves_turns_across_transports_and_account_switches() {
    let stores = [
        Arc::new(MemoryAccountStore::default()),
        Arc::new(MemoryAccountStore::default()),
    ];
    create_account(&stores[0], "acct_scope_same").await;
    create_account(&stores[1], "acct_scope_new").await;
    let mut captures = Vec::new();
    for account in [0, 0, 1, 0] {
        for websocket in [false, true] {
            let (base_url, capture): (
                String,
                BoxFuture<'static, (reqwest::header::HeaderMap, Value)>,
            ) = if websocket {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let task = tokio::spawn(async move {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut headers = None;
                    let mut socket =
                        crate::transport::accept_codex_test_websocket_with(socket, |request, _| {
                            headers = Some(request.headers().clone());
                        })
                        .await;
                    let frame = socket.next().await.unwrap().unwrap();
                    let body = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    socket.send(Message::Text(json!({"type":"response.completed", "response":{
                        "id":"resp_scoped", "model":"gpt-5.4", "status":"completed", "output":[]
                    }}).to_string().into())).await.unwrap();
                    (headers.unwrap(), body)
                });
                (url, Box::pin(async { task.await.unwrap() }))
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
                    Box::pin(async move {
                        let requests = server.received_requests().await.unwrap();
                        (
                            requests[0].headers.clone(),
                            captured_request_body(&requests[0]),
                        )
                    }),
                )
            };
            let mut stream = provider_with_base_url(&stores[account], base_url)
                .execute(
                    planned_request(
                        "openai",
                        Operation::Generate(GenerateRequest::from_protocol_payload(payload(
                            websocket,
                        ))),
                    ),
                    context("req_correlation", CancellationToken::new()),
                )
                .await
                .unwrap();
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
            let (headers, body) = capture.await;
            assert!(!headers.contains_key("traceparent"));
            assert!(!headers.contains_key("tracestate"));
            let flat = &body["client_metadata"];
            assert!(flat.get("ws_request_header_traceparent").is_none());
            assert!(flat.get("ws_request_header_tracestate").is_none());
            let header: Value =
                serde_json::from_str(headers["x-codex-turn-metadata"].to_str().unwrap()).unwrap();
            let full: Value =
                serde_json::from_str(flat["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
            for (field, original) in [
                ("session_id", "source-session"),
                ("thread_id", "source-thread"),
            ] {
                assert_scoped_id(flat[field].as_str().unwrap(), original);
                assert_eq!(flat[field], header[field]);
                assert_eq!(flat[field], full[field]);
            }
            assert_eq!(headers["session-id"].to_str().unwrap(), flat["session_id"]);
            assert_eq!(headers["thread-id"].to_str().unwrap(), flat["thread_id"]);
            assert_eq!(headers["x-client-turn-id"], "source-turn");
            assert_eq!(flat["x-client-turn-id"], "source-turn");
            assert_eq!(flat["turn_id"], "source-turn");
            for metadata in [&header, &full] {
                assert_eq!(metadata["turn_id"], "source-turn");
                assert_eq!(metadata["parent_turn_id"], "source-turn");
                assert_eq!(metadata["root_turn_id"], "source-turn");
            }
            assert_eq!(full["parent_thread_id"], flat["session_id"]);
            assert_eq!(full["forked_from_thread_id"], flat["session_id"]);
            assert_eq!(full["window_id"], full["context_window_id"]);
            assert!(header.get("tool_namespaces_info").is_none());
            assert_eq!(full["tool_namespaces_info"]["inventory"], "body-only");
            assert_eq!(full["future"], metadata()["future"]);
            let input = body["input"].as_array().unwrap();
            assert_eq!(
                input[0]["internal_chat_message_metadata_passthrough"],
                json!({"turn_id":flat["turn_id"], "content_item_kinds":["additional_content.codex_git_state"]}),
            );
            let previous_turn = &input[1]["internal_chat_message_metadata_passthrough"]["turn_id"];
            assert_eq!(previous_turn, "source-previous-turn");
            assert_ne!(previous_turn, &flat["turn_id"]);
            let mut expected_history = account_isolation::isolation_history();
            for (index, item) in expected_history
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .enumerate()
            {
                let metadata = item
                    .as_object_mut()
                    .unwrap()
                    .entry("internal_chat_message_metadata_passthrough")
                    .or_insert_with(|| json!({}));
                metadata["turn_id"] = json!(if index == 3 {
                    "source-turn"
                } else {
                    "source-previous-turn"
                });
            }
            assert_eq!(json!(&input[1..]), expected_history);
            let document =
                roxmltree::Document::parse(input[0]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            let git: Value = serde_json::from_str(
                document
                    .descendants()
                    .find(|node| node.has_tag_name("git"))
                    .unwrap()
                    .text()
                    .unwrap(),
            )
            .unwrap();
            for (desktop, wire) in [
                ("sessionId", "session_id"),
                ("threadId", "thread_id"),
                ("clientUserMessageId", "turn_id"),
            ] {
                assert_eq!(git[desktop], flat[wire]);
            }
            assert_eq!(git["path"], "/workspace/project");
            assert_scoped_id(
                body["prompt_cache_key"].as_str().unwrap(),
                "custom-cache-key",
            );
            captures.push((
                [
                    flat["session_id"].clone(),
                    flat["thread_id"].clone(),
                    flat["turn_id"].clone(),
                ],
                body["prompt_cache_key"].clone(),
                previous_turn.clone(),
            ));
        }
    }
    for index in [1, 2, 3, 6, 7] {
        assert_eq!(captures[0], captures[index]);
    }
    assert_eq!(captures[4], captures[5]);
    assert_ne!(captures[0].0, captures[4].0);
    assert_ne!(captures[0].1, captures[4].1);
    assert_eq!(captures[0].2, captures[4].2);
}

#[tokio::test]
async fn image_generation_and_edit_preserve_the_client_turn_identity() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
        .expect(2)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let body = Bytes::from_static(br#"{ "prompt":"test", "future":1, "future":2 }"#);
    for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
        let operation = Operation::GenerateImage(ImageRequest::from_raw_json(
            kind,
            RawJsonPayload::new("openai", body.clone())
                .unwrap()
                .with_context(Map::from_iter([
                    ("image_turn_id".into(), json!("source-turn")),
                    ("turn_metadata".into(), json!(metadata().to_string())),
                ])),
        ));
        let mut stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request("openai", operation),
                context("req_image_correlation", CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let requests = server.received_requests().await.unwrap();
    let first = &requests[0].headers["x-codex-image-turn-id"];
    assert_eq!(first, "source-turn");
    for request in &requests {
        assert_eq!(request.body.as_slice(), body.as_ref());
        assert_eq!(&request.headers["x-codex-image-turn-id"], first);
        let metadata: Value =
            serde_json::from_str(request.headers["x-codex-turn-metadata"].to_str().unwrap())
                .unwrap();
        assert_eq!(metadata["turn_id"], first.to_str().unwrap());
        assert_eq!(metadata["parent_turn_id"], metadata["turn_id"]);
    }
}

#[tokio::test]
async fn cache_key_preserves_explicit_sharing_and_omission_within_an_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(4)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    for (index, cache) in [Some("shared"), Some("shared"), None, Some("different")]
        .into_iter()
        .enumerate()
    {
        let mut body =
            json!({"model":"gpt-5.4", "input":[], "session_id":format!("session-{index}")});
        if let Some(cache) = cache {
            body["prompt_cache_key"] = json!(cache);
        }
        let payload = ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([("use_websocket".into(), json!(false))]));
        let mut stream = Arc::clone(&provider)
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
                ),
                context("req_cache_scope", CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let bodies: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(captured_request_body)
        .collect();
    assert_scoped_id(bodies[0]["prompt_cache_key"].as_str().unwrap(), "shared");
    assert_eq!(bodies[0]["prompt_cache_key"], bodies[1]["prompt_cache_key"]);
    assert_ne!(bodies[0]["session_id"], bodies[1]["session_id"]);
    assert!(bodies[2].get("prompt_cache_key").is_none());
    assert_ne!(bodies[0]["prompt_cache_key"], bodies[3]["prompt_cache_key"]);
}
