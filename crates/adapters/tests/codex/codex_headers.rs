use super::*;

#[test]
fn ordered_codex_headers_should_preserve_fingerprint_priority_and_request_fields() {
    let fingerprint = codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests();

    let headers = client::build_ordered_codex_headers(
        &fingerprint,
        "access-token",
        Some("acct-1"),
        Some("turn-1"),
        "req-1",
    );
    let keys = headers.keys().cloned().collect::<Vec<_>>();

    assert_eq!(headers["authorization"], "Bearer access-token");
    assert_eq!(headers["chatgpt-account-id"], "acct-1");
    assert_eq!(headers["x-client-request-id"], "req-1");
    assert_eq!(headers["x-codex-turn-state"], "turn-1");
    assert_eq!(headers["accept"], "text/event-stream");
    assert_eq!(keys.first().map(String::as_str), Some("authorization"));
}

#[test]
fn websocket_connection_should_preserve_endpoint_and_header_order() {
    let connection = codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::new(
        "wss://chatgpt.com/backend-api/codex",
        vec![
            ("authorization".to_string(), "Bearer token".to_string()),
            ("user-agent".to_string(), "Codex Desktop/test".to_string()),
        ],
    );

    assert_eq!(
        (
            connection.endpoint(),
            connection.opening_audit_snapshot().header_order,
        ),
        (
            "wss://chatgpt.com/backend-api/codex",
            vec!["authorization".to_string(), "user-agent".to_string()],
        )
    );
}

#[test]
fn websocket_connection_opening_audit_should_redact_sensitive_headers() {
    let connection = codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::new(
        "wss://chatgpt.com/backend-api/codex/responses?source=audit",
        vec![
            (
                "authorization".to_string(),
                "Bearer access-secret".to_string(),
            ),
            ("chatgpt-account-id".to_string(), "acct-secret".to_string()),
            ("user-agent".to_string(), "Codex Desktop/test".to_string()),
            ("x-client-request-id".to_string(), "req-secret".to_string()),
            (
                "x-codex-turn-metadata".to_string(),
                "{\"secret\":true}".to_string(),
            ),
        ],
    );

    let snapshot = connection.opening_audit_snapshot();

    assert_eq!(
        snapshot.request_line,
        "GET /backend-api/codex/responses?source=audit HTTP/1.1"
    );
    assert_eq!(
        snapshot
            .headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("authorization", "<redacted>"),
            ("chatgpt-account-id", "<redacted>"),
            ("user-agent", "Codex Desktop/test"),
            ("x-client-request-id", "<redacted>"),
            ("x-codex-turn-metadata", "<redacted>"),
        ]
    );
}

#[test]
fn websocket_connection_should_build_standard_opening_headers_around_business_headers() {
    let connection =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses(
            "https://chatgpt.com/backend-api",
            "test-websocket-key",
            vec![
                (
                    "authorization".to_string(),
                    "Bearer access-token".to_string(),
                ),
                ("user-agent".to_string(), "Codex Desktop/test".to_string()),
                (
                    "openai-beta".to_string(),
                    "responses_websockets=2026-02-06".to_string(),
                ),
            ],
        );

    let snapshot = connection.opening_audit_snapshot();

    assert_eq!(
        connection.endpoint(),
        "wss://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        snapshot.header_order,
        vec![
            "Host",
            "Connection",
            "Upgrade",
            "Sec-WebSocket-Version",
            "Sec-WebSocket-Key",
            "authorization",
            "user-agent",
            "openai-beta",
            "sec-websocket-extensions",
        ]
    );
    assert_eq!(
        snapshot
            .headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("Host", "chatgpt.com"),
            ("Connection", "Upgrade"),
            ("Upgrade", "websocket"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "test-websocket-key"),
            ("authorization", "<redacted>"),
            ("user-agent", "Codex Desktop/test"),
            ("openai-beta", "responses_websockets=2026-02-06"),
            (
                "sec-websocket-extensions",
                "permessage-deflate; client_max_window_bits"
            ),
        ]
    );
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_execute_response_create_request_should_capture_handshake_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, |_request: &WsRequest, mut response: WsResponse| {
                response
                    .headers_mut()
                    .insert("x-codex-turn-state", "turn-from-handshake".parse().unwrap());
                response.headers_mut().insert(
                    "set-cookie",
                    "cf_clearance=ws; Domain=.chatgpt.com; Path=/"
                        .parse()
                        .unwrap(),
                );
                response
                    .headers_mut()
                    .insert("x-ratelimit-remaining-requests", "41".parse().unwrap());
                Ok(response)
            })
            .await
            .unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_ws_headers",
                        "object": "response",
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
                            "total_tokens": 2
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let response =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect("websocket exchange should succeed");
    server.await.unwrap();

    assert_eq!(response.turn_state.as_deref(), Some("turn-from-handshake"));
    assert_eq!(
        response.set_cookie_headers,
        vec!["cf_clearance=ws; Domain=.chatgpt.com; Path=/".to_string()]
    );
    assert!(response
        .rate_limit_headers
        .iter()
        .any(|(name, value)| name == "x-ratelimit-remaining-requests" && value == "41"));
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn codex_backend_client_websocket_should_forward_security_chain_headers_and_payload_fields() {
    let received_headers = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let headers_for_server = Arc::clone(&received_headers);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, move |request: &WsRequest, response: WsResponse| {
                let headers = request
                    .headers()
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_string(),
                            value.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect::<Vec<_>>();
                *headers_for_server.lock().unwrap() = headers;
                Ok(response)
            })
            .await
            .unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<serde_json::Value>(&message.into_text().unwrap())
            .expect("client payload should be json");
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_ws_security", 1, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        payload
    });
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.use_websocket = true;
    request.prompt_cache_key = Some("client-thread".to_string());
    request.client_metadata = Some(json!({
        "safe": "yes",
        "x-openai-subagent": "review",
        "ignored_non_string": 42
    }));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    let response = backend
        .create_response(
            &request,
            client::CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws_security",
                turn_state: Some("turn-state"),
                turn_metadata: Some("{\"thread_source\":\"subagent\"}"),
                beta_features: Some("feature-a"),
                include_timing_metrics: Some("true"),
                version: Some("26.318.11754"),
                codex_window_id: Some("cw_derived"),
                parent_thread_id: Some("parent-456"),
                cookie_header: None,
                installation_id: Some("install-123"),
                session_id: Some("cp_derived"),
            },
        )
        .await
        .expect("websocket response should succeed");
    let payload = server.await.unwrap();

    assert!(response.body.contains("resp_ws_security"));
    assert_eq!(payload["prompt_cache_key"], "cp_derived");
    assert_eq!(
        payload["client_metadata"],
        json!({
            "safe": "yes",
            "x-openai-subagent": "review",
            "x-codex-installation-id": "install-123",
            "x-codex-window-id": "cw_derived",
            "x-codex-turn-metadata": "{\"thread_source\":\"subagent\"}",
            "x-codex-parent-thread-id": "parent-456"
        })
    );

    let headers = received_headers.lock().unwrap().clone();
    assert!(headers
        .iter()
        .any(|(name, value)| name == "x-client-request-id" && value == "cp_derived"));
    assert!(headers
        .iter()
        .any(|(name, value)| name == "x-openai-subagent" && value == "review"));
    assert!(headers
        .iter()
        .any(|(name, value)| name == "session-id" && value == "cp_derived"));
    assert!(headers
        .iter()
        .any(|(name, value)| name == "thread-id" && value == "cp_derived"));
    assert!(headers.iter().all(|(name, _)| name != "content-type"));
    assert!(headers.iter().all(|(name, _)| name != "accept"));
    assert!(headers.iter().all(|(name, _)| name != "session_id"));
}

#[tokio::test]
async fn codex_backend_client_should_send_desktop_headers_and_capture_response_metadata() {
    let server = wiremock::MockServer::start().await;
    let sse_body = concat!(
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n",
        "\n",
    );
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/codex/responses"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer access-token",
        ))
        .and(wiremock::matchers::header(
            "chatgpt-account-id",
            "chatgpt-account",
        ))
        .and(wiremock::matchers::header("originator", "Codex Desktop"))
        .and(wiremock::matchers::header("x-client-request-id", "req_1"))
        .and(wiremock::matchers::header("x-codex-turn-state", "turn_1"))
        .and(wiremock::matchers::header("cookie", "cf_clearance=old"))
        .and(wiremock::matchers::body_json(json!({
            "model": "gpt-5.5",
            "instructions": "",
            "input": [],
            "stream": true,
            "store": false
        })))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header(
                    "set-cookie",
                    "cf_clearance=new; Domain=.chatgpt.com; Path=/",
                )
                .insert_header("x-codex-turn-state", "turn_2")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        server.uri(),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "",
            Vec::new(),
        );
    request.force_http_sse = true;

    let response = client
        .create_response(
            &request,
            client::CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_1",
                turn_state: Some("turn_1"),
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: Some("cf_clearance=old"),
                installation_id: None,
                session_id: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        response.usage,
        Some(codex_proxy_core::protocol::codex::events::TokenUsage {
            input_tokens: 2,
            output_tokens: 3,
            cached_tokens: 1,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 5,
        })
    );
    assert_eq!(response.turn_state.as_deref(), Some("turn_2"));
    assert_eq!(
        response.set_cookie_headers,
        vec!["cf_clearance=new; Domain=.chatgpt.com; Path=/".to_string()]
    );
}

#[tokio::test]
async fn codex_backend_client_usage_should_use_original_auxiliary_headers() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/codex/usage"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "limit_reached": false
            }
        })))
        .mount(&server)
        .await;
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        server.uri(),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    let usage = client
        .fetch_usage(client::CodexRequestContext {
            access_token: "access-token",
            account_id: Some("chatgpt-account"),
            request_id: "req_aux",
            turn_state: Some("turn-state"),
            turn_metadata: Some("turn-meta"),
            beta_features: Some("feature-a"),
            include_timing_metrics: Some("true"),
            version: Some("26.318.11754"),
            codex_window_id: Some("cw_1"),
            parent_thread_id: Some("parent-1"),
            cookie_header: Some("cf_clearance=old"),
            installation_id: Some("install-1"),
            session_id: Some("session-1"),
        })
        .await
        .unwrap();

    assert_eq!(usage["rate_limit"]["limit_reached"], false);
    let requests = server.received_requests().await.unwrap();
    let headers = &requests[0].headers;
    assert_eq!(
        headers.get("accept").and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        headers
            .get("accept-encoding")
            .and_then(|value| value.to_str().ok()),
        Some("gzip, deflate")
    );
    assert!(headers.get("content-type").is_none());
    assert!(headers.get("openai-beta").is_none());
    assert!(headers.get("x-openai-internal-codex-residency").is_none());
    assert!(headers.get("x-client-request-id").is_none());
    assert_eq!(
        headers
            .get("x-codex-installation-id")
            .and_then(|value| value.to_str().ok()),
        Some("install-1")
    );
    assert!(headers.get("session_id").is_none());
}

#[tokio::test]
async fn codex_backend_client_models_should_use_original_auxiliary_headers() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/codex/models"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                {"slug": "gpt-5.5", "title": "GPT 5.5"}
            ]
        })))
        .mount(&server)
        .await;
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        server.uri(),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    let models = client
        .fetch_models(client::CodexRequestContext {
            access_token: "access-token",
            account_id: Some("chatgpt-account"),
            request_id: "req_models",
            turn_state: Some("turn-state"),
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: None,
            installation_id: Some("install-1"),
            session_id: Some("session-1"),
        })
        .await
        .unwrap();

    assert_eq!(models.len(), 1);
    let requests = server.received_requests().await.unwrap();
    let models_request = requests
        .iter()
        .find(|request| request.url.path() == "/codex/models")
        .unwrap();
    let headers = &models_request.headers;
    assert_eq!(
        headers.get("accept").and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert!(headers.get("content-type").is_none());
    assert!(headers.get("openai-beta").is_none());
    assert!(headers.get("x-openai-internal-codex-residency").is_none());
    assert!(headers.get("x-client-request-id").is_none());
    assert_eq!(
        headers
            .get("x-codex-installation-id")
            .and_then(|value| value.to_str().ok()),
        Some("install-1")
    );
    assert!(headers.get("session_id").is_none());
}

#[tokio::test]
async fn codex_backend_client_should_send_http_sse_headers_in_fingerprint_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        write_completed_sse_response(&mut stream).await;
        request
    });

    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "",
            Vec::new(),
        );
    request.force_http_sse = true;
    request.turn_metadata = Some("turn-meta".to_string());
    request.beta_features = Some("beta-a".to_string());
    request.include_timing_metrics = Some("true".to_string());
    request.version = Some("26.519.81530".to_string());
    request.codex_window_id = Some("cw_1".to_string());
    request.parent_thread_id = Some("parent-1".to_string());
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(true).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    client
        .create_response(
            &request,
            client::CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_order",
                turn_state: Some("turn-state"),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: request.codex_window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: Some("cf_clearance=old"),
                installation_id: Some("install-1"),
                session_id: Some("session-1"),
            },
        )
        .await
        .unwrap();

    let raw_request = server.await.unwrap();
    let header_names = read_header_names(&raw_request);
    assert_header_subsequence(
        &header_names,
        &[
            "authorization",
            "chatgpt-account-id",
            "originator",
            "user-agent",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "accept-encoding",
            "accept-language",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "content-type",
            "cookie",
            "accept",
            "openai-beta",
            "x-openai-internal-codex-residency",
            "x-client-request-id",
            "x-codex-installation-id",
            "session_id",
            "x-codex-window-id",
            "x-codex-turn-state",
            "x-codex-turn-metadata",
            "x-codex-beta-features",
            "x-responsesapi-include-timing-metrics",
            "version",
            "x-codex-parent-thread-id",
        ],
    );
}

#[tokio::test]
async fn codex_backend_client_should_send_compact_headers_in_fingerprint_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        write_compact_json_response(&mut stream).await;
        request
    });
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(true).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    client
        .create_compact_response(
            &codex_proxy_core::protocol::codex::responses::CodexCompactRequest {
                model: "gpt-5.5".to_string(),
                input: Vec::new(),
                instructions: String::new(),
                tools: None,
                parallel_tool_calls: None,
                reasoning: None,
                text: None,
            },
            client::CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_compact",
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: Some("cf_clearance=old"),
                installation_id: Some("install-1"),
                session_id: None,
            },
        )
        .await
        .unwrap();

    let raw_request = server.await.unwrap();
    let header_names = read_header_names(&raw_request);
    assert_header_subsequence(
        &header_names,
        &[
            "authorization",
            "chatgpt-account-id",
            "originator",
            "user-agent",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "accept-encoding",
            "accept-language",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "content-type",
            "cookie",
            "openai-beta",
            "x-openai-internal-codex-residency",
            "x-client-request-id",
            "x-codex-installation-id",
        ],
    );
}
