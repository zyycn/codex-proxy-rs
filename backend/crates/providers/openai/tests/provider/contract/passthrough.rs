//! 透传请求参数、独立端点业务头和原始 WebSocket 响应的网络回归

use super::*;

#[tokio::test]
async fn responses_options_follow_the_selected_upstream_on_both_transports() {
    let oauth_omissions = json!({
        "frequency_penalty": 0, "presence_penalty": 0, "top_p": 1,
        "chat_template_kwargs": {"enable_thinking": false}, "user": "example",
        "metadata": {"source": "example"}, "safety_identifier": "example",
        "truncation": "disabled", "stop_sequences": [], "prompt_cache_options": {}
    });
    let mut body = json!({
        "model": "gpt-5.4", "input": "hello", "max_tokens": 16,
        "stream_options": {"include_usage": true, "reasoning_summary_delivery": "sequential_cutoff"},
        "client_metadata": {"user": "keep"},
        "tools": [{"type": "function", "name": "sample", "parameters": {
            "type": "object", "properties": {"top_p": {"type": "number"}}
        }}]
    });
    let omissions = oauth_omissions.as_object().unwrap();
    body.as_object_mut().unwrap().extend(omissions.clone());
    for api_key in [false, true] {
        for websocket in [false, true] {
            let sent = capture_downstream_request(body.clone(), api_key, websocket, false).await;
            for (field, value) in omissions {
                assert_eq!(
                    sent.get(field),
                    api_key.then_some(value),
                    "{field}, api_key={api_key}, websocket={websocket}"
                );
            }
            if api_key {
                assert_eq!(sent["stream_options"], body["stream_options"]);
                assert_eq!(sent["max_tokens"], 16);
                assert!(sent.get("max_output_tokens").is_none());
            } else {
                assert_eq!(
                    sent["stream_options"],
                    json!({"reasoning_summary_delivery": "sequential_cutoff"})
                );
                assert_eq!(sent["max_output_tokens"], 16);
                assert!(sent.get("max_tokens").is_none());
            }
            assert_eq!(sent["client_metadata"]["user"], "keep");
            assert_eq!(sent["tools"], body["tools"]);
        }
    }
}

#[tokio::test]
async fn codex_oauth_keeps_explicit_limits_and_context_options() {
    for (native, completion, expected) in [
        (json!(16), json!(32), json!(16)),
        (Value::Null, json!(32), json!(32)),
        (Value::Null, Value::Null, json!(64)),
    ] {
        let body = json!({
            "model": "gpt-5.4", "input": "hello", "max_output_tokens": native,
            "max_completion_tokens": completion, "max_tokens": 64,
            "truncation": "auto", "stop_sequences": ["end"],
            "prompt_cache_options": {"future_option": true}
        });
        let sent = capture_downstream_request(body.clone(), false, false, false).await;
        assert_eq!(sent["max_output_tokens"], expected);
        assert!(sent.get("max_completion_tokens").is_none());
        assert!(sent.get("max_tokens").is_none());
        for key in ["truncation", "stop_sequences", "prompt_cache_options"] {
            assert_eq!(sent[key], body[key]);
        }
    }
}

#[tokio::test]
async fn effort_values_reach_upstream_before_and_after_middleware() {
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
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let efforts = [
        json!(64),
        json!(0),
        json!("future-effort"),
        json!({"future": [1, true]}),
        json!(null),
        json!("x".repeat(65)),
    ];
    for rewritten in [false, true] {
        for effort in &efforts {
            let reasoning = json!({"effort":effort,"future_extension":true});
            let initial_reasoning = if rewritten {
                json!({"effort":"high"})
            } else {
                reasoning.clone()
            };
            let body = json!({"model":"gpt-5.4","input":"synthetic","reasoning":initial_reasoning});
            let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
                    .unwrap()
                    .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
            ));
            let context = if rewritten {
                context_with_middleware(
                    "req_effort",
                    Arc::new(RecordingMiddleware {
                        observed: Arc::default(),
                        replacement: ("reasoning".to_owned(), reasoning.clone()),
                        request_headers: Vec::new(),
                    }),
                    FastMode::Default,
                )
            } else {
                context("req_effort", CancellationToken::new())
            };
            let mut stream = Arc::clone(&provider)
                .execute(planned_request("openai", operation), context)
                .await
                .unwrap();
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
            let requests = server.received_requests().await.unwrap();
            assert_eq!(
                captured_request_body(requests.last().unwrap())["reasoning"],
                reasoning
            );
        }
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        efforts.len() * 2
    );
}

#[tokio::test]
async fn independent_endpoints_send_opaque_headers_without_client_identity() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":"ok"})))
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let entries: Vec<_> = [
        ("x-future-business", "first"),
        ("x-future-business", "second"),
        ("x-codex-installation-id", "client-installation"),
        ("x-openai-account-routing-override", "client-route"),
        ("x-openai-fedramp", "true"),
        ("traceparent", "client-trace"),
        ("tracestate", "vendor=client"),
        ("sentry-trace", "sentry-client-trace"),
        ("baggage", "environment=synthetic"),
        ("authorization", "Bearer client-secret"),
        ("chatgpt-account-id", "client-account"),
        ("x-stainless-runtime", "client-runtime"),
        ("connection", "close"),
        ("x-codex-turn-state", "untrusted-state"),
        ("x-codex-turn-metadata", "untrusted-metadata"),
    ]
    .into_iter()
    .map(|(name, value)| json!([name, STANDARD.encode(value)]))
    .collect();
    for endpoint in [
        "/codex/images/generations",
        "/codex/images/edits",
        "/codex/alpha/search",
    ] {
        let raw = Bytes::from_static(
            br#"{ "id":"test-search", "prompt":"synthetic", "future":1, "future":2 }"#,
        );
        let payload = RawJsonPayload::new("openai", raw.clone())
            .unwrap()
            .with_context(Map::from_iter([(
                "opaque_request_headers".to_owned(),
                json!(entries),
            )]));
        let operation = if endpoint.ends_with("search") {
            Operation::Search(StandaloneSearchRequest::from_raw_json(payload))
        } else {
            Operation::GenerateImage(ImageRequest::from_raw_json(
                if endpoint.ends_with("edits") {
                    ImageRequestKind::Edit
                } else {
                    ImageRequestKind::Generation
                },
                payload,
            ))
        };
        let mut stream = Arc::clone(&provider)
            .execute(
                planned_provider_endpoint_request("openai", operation),
                context("req_opaque_headers", CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        let sent = requests.last().unwrap();
        assert_eq!(sent.url.path(), endpoint);
        uuid::Uuid::parse_str(sent.headers["x-codex-installation-id"].to_str().unwrap()).unwrap();
        assert!(
            !sent
                .headers
                .contains_key("x-openai-account-routing-override")
        );
        for (name, expected) in [
            ("x-openai-fedramp", "true"),
            ("traceparent", "client-trace"),
            ("tracestate", "vendor=client"),
            ("sentry-trace", "sentry-client-trace"),
            ("baggage", "environment=synthetic"),
        ] {
            assert_eq!(sent.headers[name], expected);
        }
        if endpoint.ends_with("search") {
            assert_scoped_search_body(&sent.body, &raw);
        } else {
            assert_eq!(sent.body, raw);
        }
        assert_eq!(
            captured_header_values(sent, "x-future-business"),
            vec![b"first".to_vec(), b"second".to_vec()]
        );
        assert!(
            !captured_header_values(sent, "authorization")
                .contains(&b"Bearer client-secret".to_vec())
        );
        assert!(
            !captured_header_values(sent, "chatgpt-account-id")
                .contains(&b"client-account".to_vec())
        );
        assert!(captured_header_values(sent, "x-stainless-runtime").is_empty());
        assert!(captured_header_values(sent, "x-codex-turn-state").is_empty());
        assert!(captured_header_values(sent, "x-codex-turn-metadata").is_empty());
        assert!(!captured_header_values(sent, "connection").contains(&b"close".to_vec()));
    }
}

#[tokio::test]
async fn websocket_messages_preserve_text_and_metadata_despite_unrecognized_json() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider =
        provider_with_base_url(&store, format!("http://{}", listener.local_addr().unwrap()));
    let messages = vec![
        "{ \"type\" : \"future.event\", \"extension\" : { \"escaped\" : \"\\u0061\", \"number\" : 1e3 } }\r\n".to_owned(),
        json!({"type":"response.metadata","headers":{"x-codex-turn-state":"synthetic-state"},"metadata":{"type":"safety_buffering","use_cases":["cyber"],"reasons":["user_risk"],"future":true}}).to_string(),
        format!("{{\"type\":\"future.deep\",\"extension\":{}0{}}}", "[".repeat(140), "]".repeat(140)),
        "future non-JSON text".to_owned(), "[DONE]".to_owned(),
        r#"{"type":42,"future":true}"#.to_owned(),
    ];
    let sent = messages.clone();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = accept_codex_test_websocket(socket).await;
        socket.next().await.unwrap().unwrap();
        socket.send(Message::Text(json!({"type":"response.created","response":{"id":"resp_raw","model":"gpt-5.4","status":"in_progress","output":[]}}).to_string().into())).await.unwrap();
        for message in sent {
            socket.send(Message::Text(message.into())).await.unwrap();
        }
        socket.send(Message::Text(json!({"type":"response.completed","response":{"id":"resp_raw","model":"gpt-5.4","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}).to_string().into())).await.unwrap();
    });
    let mut stream = provider
        .execute(
            planned_request("openai", generate_operation()),
            context("req_raw_ws", CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut delivered = Vec::new();
    let mut completed = false;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        completed |= event
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
        if let Some(raw) = event
            .wire_event()
            .and_then(|wire| wire.raw_websocket_message())
        {
            delivered.push(raw.to_owned());
        }
    }
    server.await.unwrap();
    assert!(completed);
    assert_eq!(&delivered[1..delivered.len() - 1], messages);
}
