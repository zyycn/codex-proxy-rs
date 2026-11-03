//! 账号追踪域的稳定性、轮次原值透传、HTTP/WS 一致性与工具端点归属回归

use super::*;

fn metadata() -> Value {
    json!({"session_id":"source-session", "thread_id":"source-thread", "turn_id":"source-turn",
        "parent_thread_id":"source-session", "forked_from_thread_id":"source-session",
        "parent_turn_id":"source-turn", "root_turn_id":"source-turn",
        "window_id":"source-window", "context_window_id":"source-window", "future":{"keep":true},
        "sentry-trace":"source-trace", "baggage":"sentry-environment=synthetic"})
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
            "ws_request_header_tracestate":"vendor=source",
            "ws_request_header_sentry-trace":"source-trace",
            "ws_request_header_baggage":"sentry-environment=synthetic"
        }
    }).as_object().unwrap().clone()).unwrap().with_context(Map::from_iter([
        ("use_websocket".into(), json!(websocket)),
        ("session_id".into(), json!("source-session")),
        ("thread_id".into(), json!("source-thread")),
        ("turn_metadata".into(), json!(metadata().to_string())),
        ("opaque_request_headers".into(), json!([
            ["x-client-turn-id",STANDARD.encode("source-turn")],
            ["traceparent",STANDARD.encode("00-11111111111111111111111111111111-2222222222222222-01")],
            ["tracestate",STANDARD.encode("vendor=source")],
            ["Sentry-Trace",STANDARD.encode("source-trace")],
            ["baggage",STANDARD.encode("sentry-environment=synthetic")],
            ["baggage",STANDARD.encode("sentry-release=synthetic")]
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
            assert_eq!(
                headers["traceparent"],
                "00-11111111111111111111111111111111-2222222222222222-01"
            );
            assert_eq!(headers["tracestate"], "vendor=source");
            assert_eq!(headers["sentry-trace"], "source-trace");
            assert_eq!(
                headers
                    .get_all("baggage")
                    .iter()
                    .map(|value| value.to_str().unwrap())
                    .collect::<Vec<_>>(),
                ["sentry-environment=synthetic", "sentry-release=synthetic"]
            );
            let flat = &body["client_metadata"];
            assert_eq!(
                flat["ws_request_header_traceparent"],
                headers["traceparent"].to_str().unwrap()
            );
            assert_eq!(flat["ws_request_header_tracestate"], "vendor=source");
            assert_eq!(flat["ws_request_header_sentry-trace"], "source-trace");
            assert_eq!(
                flat["ws_request_header_baggage"],
                "sentry-environment=synthetic"
            );
            assert_eq!(
                headers["x-codex-installation-id"].to_str().unwrap(),
                flat["x-codex-installation-id"]
            );
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
                assert_eq!(metadata["sentry-trace"], "source-trace");
                assert_eq!(metadata["baggage"], "sentry-environment=synthetic");
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
