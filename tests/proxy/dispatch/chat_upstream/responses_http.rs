use super::*;

#[tokio::test]
async fn responses_should_reject_invalid_json_without_upstream_request() {
    let server = MockServer::start().await;
    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let requests = server.received_requests().await.unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
    assert!(requests.is_empty());
}

#[tokio::test]
async fn responses_should_reject_non_object_json_without_upstream_request() {
    let server = MockServer::start().await;
    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from("[]"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let requests = server.received_requests().await.unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
    assert!(requests.is_empty());
}

#[tokio::test]
async fn responses_should_honor_explicit_http_sse_transport() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: response.output_text.delta"));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "POST");
    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert!(upstream_body.get("use_websocket").is_none());
}

#[tokio::test]
async fn responses_should_stagger_same_account_requests_before_sending_upstream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let request_times_for_server = Arc::clone(&request_times);
    let (first_seen_tx, first_seen_rx) = oneshot::channel();
    let (release_first_tx, release_first_rx) = oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (mut first_socket, _) = listener.accept().await.unwrap();
        request_times_for_server
            .lock()
            .unwrap()
            .push(Instant::now());
        first_seen_tx.send(()).unwrap();
        read_http_request(&mut first_socket).await;

        let (mut second_socket, _) = listener.accept().await.unwrap();
        request_times_for_server
            .lock()
            .unwrap()
            .push(Instant::now());
        read_http_request(&mut second_socket).await;
        write_http_sse_response(&mut second_socket, RESPONSES_COMPLETED_USAGE_SSE).await;

        release_first_rx.await.unwrap();
        write_http_sse_response(&mut first_socket, RESPONSES_COMPLETED_USAGE_SSE).await;
    });

    let (app, api_key, _pool, _dir) = test_app_with_account_pool_config(base_url, |config| {
        config.auth.max_concurrent_per_account = 2;
        config.auth.request_interval_ms = 300;
    })
    .await;
    let first_app = app.clone();
    let first_api_key = api_key.clone();
    let first_response = tokio::spawn(async move {
        first_app
            .oneshot(responses_http_sse_request(
                &first_api_key,
                "req_stagger_first",
            ))
            .await
            .unwrap()
    });
    first_seen_rx.await.unwrap();

    let second = app
        .clone()
        .oneshot(responses_http_sse_request(&api_key, "req_stagger_second"))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    release_first_tx.send(()).unwrap();
    let first = first_response.await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    upstream.await.unwrap();

    let times = request_times.lock().unwrap();
    assert_eq!(times.len(), 2);
    let elapsed = times[1].duration_since(times[0]);
    assert!(
        elapsed >= StdDuration::from_millis(180),
        "second upstream request was sent too early: {elapsed:?}"
    );
}

#[tokio::test]
async fn responses_should_use_websocket_upstream_by_default_while_serving_sse() {
    let (base_url, upstream) = spawn_single_websocket_completed_upstream("resp_ws_default").await;
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "generate": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("event: response.completed"));
    assert!(body.contains("\"id\":\"resp_ws_default\""));
    assert_eq!(captured.payload["type"], "response.create");
    assert_eq!(captured.payload["model"], "gpt-5.5");
    assert!(captured.payload.get("generate").is_none());
    assert!(captured.payload.get("previous_response_id").is_none());
    assert!(captured.payload["prompt_cache_key"]
        .as_str()
        .is_some_and(|value| value.starts_with("cp_")));
    assert!(captured.payload["client_metadata"]["x-codex-installation-id"].is_string());
    assert!(captured.payload["client_metadata"]["x-codex-window-id"]
        .as_str()
        .is_some_and(|value| value.starts_with("cp_") && value.ends_with(":0")));
}

#[tokio::test]
async fn responses_non_stream_should_record_websocket_transport_metadata() {
    let (base_url, upstream) =
        spawn_single_websocket_completed_upstream("resp_ws_non_stream_log").await;
    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(base_url).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "generate": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(body["id"], "resp_ws_non_stream_log");
    assert_eq!(captured.payload["type"], "response.create");

    let event = latest_response_usage_record(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(metadata["stream"], false);
    assert_eq!(metadata["transport"], "websocket");
    assert_eq!(metadata["websocketPool"]["kind"], "new");
}

#[tokio::test]
async fn responses_should_ignore_camel_case_use_websocket_field() {
    let (base_url, upstream) =
        spawn_single_websocket_completed_upstream("resp_ws_camel_case").await;
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "useWebSocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let captured = upstream.await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(captured.payload["type"], "response.create");
    assert!(captured.payload.get("useWebSocket").is_none());
    assert!(captured.payload.get("use_websocket").is_none());
}

#[tokio::test]
async fn responses_should_convert_tuple_schema_before_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_TUPLE_OBJECT_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(tuple_response_request_body(false)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();

    assert_eq!(status, StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let schema = &upstream_body["text"]["format"]["schema"];
    assert!(schema["properties"]["point"].get("prefixItems").is_none());
    assert!(schema["properties"]["point"].get("items").is_none());
    assert_eq!(
        schema["properties"]["point"],
        json!({
            "type": "object",
            "properties": {
                "0": {"type": "number"},
                "1": {"type": "number"}
            },
            "required": ["0", "1"],
            "additionalProperties": false
        })
    );
}

#[tokio::test]
async fn responses_should_reconvert_tuple_schema_output_for_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_TUPLE_OBJECT_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(tuple_response_request_body(false)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_tuple");
    assert_eq!(body["output_text"], "{\"point\":[1,2]}");
    assert_eq!(body["output"][0]["content"][0]["text"], "{\"point\":[1,2]}");
}

#[tokio::test]
async fn responses_stream_should_reconvert_tuple_schema_output_for_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_TUPLE_OBJECT_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(tuple_response_request_body(true)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#""delta":"{\"point\":[1,2]}""#));
    assert!(body.contains(r#""output_text":"{\"point\":[1,2]}""#));
    assert!(!body.contains(r#""point\":{\"0":1,"1":2}"#));
    assert!(body.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn responses_should_forward_parity_fields_context_headers_and_account_scoped_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-codex-turn-state", "turn-header")
                .header("x-codex-beta-features", "beta-header")
                .header("x-responsesapi-include-timing-metrics", "false")
                .header("version", "header-version")
                .header("x-openai-subagent", "review")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5-high-fast",
                        "stream": false,
                        "use_websocket": false,
                        "input": [],
                        "prompt_cache_key": "pcache",
                        "client_metadata": {
                            "safe": "yes",
                            "drop": 42,
                            "x-codex-turn-metadata": "meta-metadata",
                            "x-codex-window-id": "window-metadata",
                            "x-codex-parent-thread-id": "parent-metadata"
                        },
                        "turnState": "turn-body",
                        "betaFeatures": "beta-body",
                        "includeTimingMetrics": "true",
                        "version": "2026-06-12",
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_response_1");
    let requests = server.received_requests().await.unwrap();
    let upstream = requests
        .iter()
        .find(|request| request.url.path() == "/codex/responses")
        .expect("responses upstream request should be sent");
    let upstream_body: Value = serde_json::from_slice(&upstream.body).unwrap();
    let identity =
        build_conversation_identity(Some("pcache"), Some("window-metadata"), "acct_chat");
    let conversation_id = identity
        .conversation_id
        .as_deref()
        .expect("conversation identity should be scoped");
    let window_id = identity
        .window_id
        .as_deref()
        .expect("window identity should be scoped");
    let upstream_header = |name: &str| {
        upstream
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
    };

    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert_eq!(upstream_body["service_tier"], "priority");
    assert_eq!(
        upstream_body["reasoning"],
        json!({"summary": "auto", "effort": "high"})
    );
    assert_eq!(
        upstream_body["include"],
        json!(["reasoning.encrypted_content"])
    );
    assert_eq!(upstream_body["prompt_cache_key"], conversation_id);
    assert_eq!(upstream_body["client_metadata"]["safe"], "yes");
    assert_eq!(
        upstream_body["client_metadata"]["x-openai-subagent"],
        "review"
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-installation-id"],
        TEST_INSTALLATION_ID
    );
    assert_eq!(
        upstream_body["client_metadata"]["session_id"],
        conversation_id
    );
    assert_eq!(
        upstream_body["client_metadata"]["thread_id"],
        conversation_id
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-window-id"],
        window_id
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-turn-metadata"],
        "meta-metadata"
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-parent-thread-id"],
        "parent-metadata"
    );
    assert_eq!(upstream_header("session-id"), Some(conversation_id));
    assert_eq!(upstream_header("thread-id"), Some(conversation_id));
    assert_eq!(upstream_header("session_id"), None);
    assert_eq!(upstream_header("x-codex-window-id"), Some(window_id));
    assert_eq!(upstream_header("x-codex-turn-state"), Some("turn-body"));
    assert_eq!(
        upstream_header("x-codex-turn-metadata"),
        Some("meta-metadata")
    );
    assert_eq!(upstream_header("x-codex-beta-features"), Some("beta-body"));
    assert_eq!(
        upstream_header("x-responsesapi-include-timing-metrics"),
        Some("true")
    );
    assert_eq!(upstream_header("version"), Some("2026-06-12"));
    assert_eq!(
        upstream_header("x-codex-parent-thread-id"),
        Some("parent-metadata")
    );
    assert_eq!(upstream_header("x-openai-subagent"), Some("review"));
    for local_field in [
        "turnState",
        "turnMetadata",
        "betaFeatures",
        "includeTimingMetrics",
        "version",
        "codexWindowId",
        "parentThreadId",
        "use_websocket",
    ] {
        assert!(upstream_body.get(local_field).is_none());
    }
}

#[tokio::test]
async fn responses_should_preserve_non_empty_include_when_reasoning_defaults_apply() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5-high",
                        "stream": false,
                        "use_websocket": false,
                        "input": [],
                        "include": ["file_search_call.results"],
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        upstream_body["reasoning"],
        json!({"summary": "auto", "effort": "high"})
    );
    assert_eq!(
        upstream_body["include"],
        json!(["file_search_call.results"])
    );
}

#[tokio::test]
async fn responses_should_sanitize_reasoning_and_compaction_input_before_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_COMPLETED_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": false,
                        "use_websocket": false,
                        "input": [
                            {
                                "type": "reasoning",
                                "id": "rs_1",
                                "status": "completed",
                                "summary": [
                                    {"type": "summary_text", "text": "valid summary"},
                                    {"type": "ignored", "text": "drop"}
                                ],
                                "encrypted_content": "enc_reasoning",
                                "content": [
                                    {"type": "reasoning_text", "text": "valid reasoning"},
                                    {"type": "ignored", "text": "drop"}
                                ],
                                "extra": "drop"
                            },
                            {
                                "type": "reasoning",
                                "id": "",
                                "summary": [{"type": "summary_text", "text": "drop"}]
                            },
                            {
                                "type": "compaction",
                                "id": "cmp_1",
                                "encrypted_content": "enc_compaction",
                                "extra": "drop"
                            },
                            {"type": "compaction", "id": "cmp_drop"},
                            {"type": "message", "role": "user", "content": "keep me", "extra": 42}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        upstream_body["input"],
        json!([
            {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{"type": "summary_text", "text": "valid summary"}],
                "status": "completed",
                "encrypted_content": "enc_reasoning",
                "content": [{"type": "reasoning_text", "text": "valid reasoning"}]
            },
            {
                "type": "compaction",
                "encrypted_content": "enc_compaction",
                "id": "cmp_1"
            },
            {"type": "message", "role": "user", "content": "keep me", "extra": 42}
        ])
    );
}

#[tokio::test]
async fn responses_should_reconstruct_non_stream_output_text_from_sse_deltas() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_TEXT_DELTAS_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert_eq!(body["id"], "resp_text");
    assert_eq!(body["output"][0]["role"], "assistant");
    assert_eq!(body["output"][0]["content"][0]["text"], "hello from deltas");
    assert_eq!(body["output_text"], "hello from deltas");
}

#[tokio::test]
async fn responses_should_use_done_output_items_when_completed_output_is_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_DONE_ITEM_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert_eq!(body["id"], "resp_item");
    assert_eq!(body["output"][0]["content"][0]["text"], "from done item");
    assert_eq!(body["output_text"], "from done item");
}

#[tokio::test]
async fn responses_should_scope_upstream_cookie_by_codex_response_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_COMPLETED_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let cookie_store = SqliteCookieStore::new(pool.clone());
    cookie_store
        .capture_set_cookie(
            "acct_chat",
            "cf_clearance=root; Domain=.chatgpt.com; Path=/",
        )
        .await
        .unwrap();
    cookie_store
        .capture_set_cookie(
            "acct_chat",
            "cf_clearance=codex; Domain=.chatgpt.com; Path=/codex",
        )
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let cookie_header = requests
        .iter()
        .find(|request| request.url.path() == "/codex/responses")
        .and_then(|request| request.headers.get("cookie"))
        .and_then(|value| value.to_str().ok());
    assert_eq!(cookie_header, Some("cf_clearance=codex; cf_clearance=root"));
}

#[tokio::test]
async fn responses_stream_should_close_http_sse_upstream_when_client_disconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await;
        write_chunked_http_sse_headers(&mut socket).await;
        write_http_chunk(
            &mut socket,
            include_bytes!("../../../fixtures/responses/http_sse/text_delta_hello.sse"),
        )
        .await;
        socket.flush().await.unwrap();

        timeout(
            StdDuration::from_secs(2),
            wait_for_http_sse_upstream_disconnect(&mut socket),
        )
        .await
        .is_ok()
    });

    let (app, api_key, _dir) = test_app_with_account(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first_chunk = timeout(StdDuration::from_secs(1), body.next())
        .await
        .expect("first SSE chunk should arrive before disconnect")
        .expect("stream should yield a first chunk")
        .expect("chunk should be readable");
    assert!(String::from_utf8(first_chunk.to_vec())
        .unwrap()
        .contains("event: response.output_text.delta"));

    drop(body);
    assert!(
        upstream.await.unwrap(),
        "dropping the downstream stream should close the HTTP SSE upstream socket"
    );
}

#[tokio::test]
async fn responses_stream_should_forward_first_chunk_before_upstream_completes() {
    let (base_url, first_chunk_sent, finish_upstream) = spawn_chunked_sse_upstream(
        include_str!("../../../fixtures/responses/http_sse/live_stream_hello_delta.sse"),
        include_str!("../../../fixtures/responses/http_sse/live_stream_completed.sse"),
    )
    .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response_task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });

    first_chunk_sent.await.unwrap();
    let response = timeout(StdDuration::from_millis(300), response_task)
        .await
        .expect("stream response should be returned before upstream completes")
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut body_stream = response.into_body().into_data_stream();
    let first_chunk = timeout(StdDuration::from_millis(300), body_stream.next())
        .await
        .expect("first proxied SSE chunk should arrive before upstream completes")
        .unwrap()
        .unwrap();
    let first_chunk = String::from_utf8(first_chunk.to_vec()).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(first_chunk.contains("live stream hello"));
    assert!(!first_chunk.contains("resp_live_stream"));

    finish_upstream.send(()).unwrap();
    let mut rest = Vec::new();
    while let Some(chunk) = body_stream.next().await {
        rest.extend_from_slice(&chunk.unwrap());
    }
    let rest = String::from_utf8(rest).unwrap();
    let usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(rest.contains("resp_live_stream"));
    assert!(
        rest.ends_with("data: [DONE]\n\n"),
        "stream responses should terminate clients, body was {rest:?}"
    );
    assert_eq!(usage, (1, 3, 4));
}

#[tokio::test]
async fn responses_stream_should_emit_failed_event_after_upstream_read_error_once_downstream_started(
) {
    let (base_url, first_chunk_sent, close_upstream) =
        spawn_chunked_sse_upstream_then_abrupt_close(include_str!(
            "../../../fixtures/responses/http_sse/partial_transport_failure.sse"
        ))
        .await;

    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response_task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Start then fail"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });

    first_chunk_sent.await.unwrap();
    let response = timeout(StdDuration::from_millis(300), response_task)
        .await
        .expect("stream response should be returned before upstream closes")
        .unwrap();
    let mut body_stream = response.into_body().into_data_stream();
    let first_chunk = timeout(StdDuration::from_millis(300), body_stream.next())
        .await
        .expect("first proxied SSE chunk should arrive before upstream closes")
        .unwrap()
        .unwrap();
    assert!(String::from_utf8(first_chunk.to_vec())
        .unwrap()
        .contains("partial before transport failure"));

    close_upstream.send(()).unwrap();
    let rest = collect_stream_body(body_stream).await;

    assert!(rest.contains("event: response.failed"));
    assert!(rest.contains("stream_disconnected"));
    assert!(rest.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn responses_stream_should_emit_failed_event_when_upstream_closes_without_completed() {
    let (base_url, first_chunk_sent, close_upstream) = spawn_chunked_sse_upstream_then_clean_close(
        include_str!("../../../fixtures/responses/http_sse/partial_clean_close.sse"),
    )
    .await;

    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response_task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Start then close"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });

    first_chunk_sent.await.unwrap();
    let response = timeout(StdDuration::from_millis(300), response_task)
        .await
        .expect("stream response should be returned before upstream closes")
        .unwrap();
    let mut body_stream = response.into_body().into_data_stream();
    let first_chunk = timeout(StdDuration::from_millis(300), body_stream.next())
        .await
        .expect("first proxied SSE chunk should arrive before upstream closes")
        .unwrap()
        .unwrap();
    assert!(String::from_utf8(first_chunk.to_vec())
        .unwrap()
        .contains("partial before clean close"));

    close_upstream.send(()).unwrap();
    let rest = collect_stream_body(body_stream).await;

    assert!(rest.contains("event: response.failed"));
    assert!(rest.contains("stream_disconnected"));
    assert!(rest.contains(r#""id":"resp_clean_close""#));
    assert!(rest.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn responses_should_prefer_session_affinity_account_for_previous_response() {
    let (base_url, upstream) = spawn_single_websocket_completed_upstream("resp_affinity_ws").await;
    let (app, api_key, _dir) = test_app_with_two_accounts_and_affinity(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_previous",
                        "input": [{"role": "user", "content": "Continue"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let captured = upstream.await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        captured_header(&captured.headers, "authorization"),
        Some("Bearer access-affinity")
    );
    assert_eq!(
        captured_header(&captured.headers, "chatgpt-account-id"),
        Some("chatgpt-affinity")
    );
    assert_eq!(captured.payload["previous_response_id"], "resp_previous");
}

#[tokio::test]
async fn responses_should_strip_banned_affinity_history_when_switching_to_active_account() {
    let (base_url, upstream) =
        spawn_single_websocket_completed_upstream("resp_after_banned_affinity").await;
    let (app, api_key, pool, _dir) =
        test_app_with_two_accounts_and_affinity_status(base_url, "banned").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_affinity_risk",
                        "input": [{"role": "user", "content": "Continue after ban"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let captured = upstream.await.unwrap();
    let affinity_count: (i64,) =
        sqlx::query_as("select count(*) from session_affinities where response_id = ?")
            .bind("resp_affinity_risk")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        captured_header(&captured.headers, "authorization"),
        Some("Bearer access-primary")
    );
    assert_eq!(
        captured_header(&captured.headers, "chatgpt-account-id"),
        Some("chatgpt-primary")
    );
    assert!(captured_header(&captured.headers, "x-codex-turn-state").is_none());
    assert!(captured.payload.get("previous_response_id").is_none());
    assert_eq!(affinity_count.0, 0);
}

#[tokio::test]
async fn responses_should_keep_history_when_affinity_account_is_quota_exhausted() {
    let (base_url, upstream) =
        spawn_single_websocket_completed_upstream("resp_after_quota_affinity").await;
    let (app, api_key, pool, _dir) =
        test_app_with_two_accounts_and_affinity_status(base_url, "quota_exhausted").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_affinity_risk",
                        "input": [{"role": "user", "content": "Continue after quota"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let captured = upstream.await.unwrap();
    let affinity_count: (i64,) =
        sqlx::query_as("select count(*) from session_affinities where response_id = ?")
            .bind("resp_affinity_risk")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        captured_header(&captured.headers, "authorization"),
        Some("Bearer access-primary")
    );
    assert_eq!(
        captured_header(&captured.headers, "x-codex-turn-state"),
        Some("turn_affinity_risk")
    );
    assert_eq!(
        captured.payload["previous_response_id"],
        "resp_affinity_risk"
    );
    assert_eq!(affinity_count.0, 1);
}
