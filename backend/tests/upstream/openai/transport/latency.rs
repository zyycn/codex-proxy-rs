use codex_proxy_rs::upstream::openai::{
    protocol::responses::{CodexResponsesRequest, PreviousResponseScope},
    transport::{
        websocket::{CodexWebSocketExchangeError, PreviousResponseUnavailableReason},
        websocket_breaker::{
            WebSocketOriginBreaker, WebSocketOriginBreakerConfig, WebSocketOriginBreakerDecision,
        },
        websocket_pool::CodexWebSocketPool,
    },
};

use super::*;

fn new_chain_request(conversation_id: &str) -> CodexResponsesRequest {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", Vec::new());
    request.use_websocket = true;
    request.local_conversation_id = Some(conversation_id.to_string());
    request
}

#[tokio::test]
async fn cold_websocket_should_fall_back_to_same_account_http_after_fast_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stalled_websocket, _) = listener.accept().await.unwrap();
        let opening = read_http_request(&mut stalled_websocket).await;
        assert!(opening.starts_with("GET /codex/responses HTTP/1.1"));

        let (mut http, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut http).await;
        assert!(request.starts_with("POST /codex/responses HTTP/1.1"));
        write_completed_sse_response(&mut http).await;
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()))
    .with_websocket_fast_path_budget(Duration::from_millis(30));

    let response = backend
        .create_response(
            &new_chain_request("conversation-fast-budget"),
            request_context("req_fast_budget", Some("chatgpt-account")),
        )
        .await
        .expect("pre-send timeout should use HTTP");
    server.await.unwrap();

    assert_eq!(response.transport, CodexBackendTransport::HttpSse);
    assert_eq!(
        response.transport_metrics.decision,
        Some(CodexTransportDecision::Http2WebSocketSlow)
    );
    assert!(response.transport_metrics.first_event_ms.is_some());
    assert!(response.body.contains("response.completed"));
}

#[tokio::test]
async fn store_false_warmup_should_never_fall_back_to_http() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut opening, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut opening).await;
        assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
        opening
            .write_all(
                b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()));
    let mut body = serde_json::Map::new();
    body.insert("model".to_string(), json!("gpt-5.5"));
    body.insert("input".to_string(), json!([]));
    body.insert("generate".to_string(), json!(false));
    body.insert("store".to_string(), json!(false));
    let mut request = CodexResponsesRequest::from_body(body);
    request.use_websocket = true;
    request.local_conversation_id = Some("conversation-warmup-required".to_string());

    let error = backend
        .create_response(
            &request,
            request_context("req_warmup_required", Some("chatgpt-account")),
        )
        .await
        .expect_err("warmup opening failure must not use HTTP");
    server.await.unwrap();

    assert_eq!(error.transport(), Some(CodexBackendTransport::WebSocket));
    std::assert_matches!(
        error,
        CodexClientError::Upstream { status, .. }
            if status == reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn websocket_opening_upstream_error_should_not_fall_back_to_http() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut opening, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut opening).await;
        assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
        let body = r#"{"error":{"code":"token_revoked","message":"expired"}}"#;
        opening
            .write_all(
                format!(
                    "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()));

    let error = backend
        .create_response(
            &new_chain_request("conversation-opening-upstream-error"),
            request_context("req_opening_upstream_error", Some("chatgpt-account")),
        )
        .await
        .expect_err("explicit opening response should reach account classification");
    server.await.unwrap();

    std::assert_matches!(
        error,
        CodexClientError::Upstream {
            status,
            transport: CodexBackendTransport::WebSocket,
            ..
        } if status == reqwest::StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn connection_local_continuation_should_use_the_exact_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_for_server = Arc::clone(&accepted);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        accepted_for_server.fetch_add(1, Ordering::SeqCst);
        let mut websocket = accept_codex_test_websocket(stream).await;
        let first = websocket.next().await.unwrap().unwrap();
        std::assert_matches!(first, Message::Text(_));
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_exact_seed", 2, 1).into(),
            ))
            .await
            .unwrap();

        let second = websocket.next().await.unwrap().unwrap();
        let Message::Text(second) = second else {
            panic!("second request should be text");
        };
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second["previous_response_id"], "resp_exact_seed");
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_exact_second", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()));
    let first_request = new_chain_request("conversation-exact");
    let first = backend
        .create_response(
            &first_request,
            request_context("req_exact_seed", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    let mut second_request = first_request;
    second_request.set_previous_response_id(Some("resp_exact_seed".to_string()));
    second_request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
    let second = backend
        .create_response(
            &second_request,
            request_context("req_exact_second", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    server.await.unwrap();

    assert!(first.connection_local_continuation);
    assert!(first.transport_metrics.first_event_ms.is_some());
    assert!(second.connection_local_continuation);
    assert_eq!(
        second.transport_metrics.decision,
        Some(CodexTransportDecision::ExactWebSocket)
    );
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_exact_socket_should_fail_without_opening_a_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()));
    let mut request = new_chain_request("conversation-exact-missing");
    request.set_previous_response_id(Some("resp_missing".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);

    let error = backend
        .create_response(
            &request,
            request_context("req_exact_missing", Some("chatgpt-account")),
        )
        .await
        .expect_err("missing exact socket should be typed unavailable");

    std::assert_matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            reason: PreviousResponseUnavailableReason::FreshConnectionRequired
        })
    );
    assert!(
        timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn concurrent_same_key_should_singleflight_websocket_opening() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut websocket = accept_codex_test_websocket(first_stream).await;
        let _payload = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_singleflight_ws", 2, 1).into(),
            ))
            .await
            .unwrap();

        let (mut http, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut http).await;
        assert!(request.starts_with("POST /codex/responses HTTP/1.1"));
        write_completed_sse_response(&mut http).await;
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()))
    .with_websocket_fast_path_budget(Duration::from_millis(200));
    let request = new_chain_request("conversation-singleflight");
    let first_backend = backend.clone();
    let first_request = request.clone();
    let first = tokio::spawn(async move {
        first_backend
            .create_response(
                &first_request,
                request_context("req_singleflight_first", Some("chatgpt-account")),
            )
            .await
    });
    accepted_rx.await.unwrap();
    let second = backend
        .create_response(
            &request,
            request_context("req_singleflight_second", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    let first = first.await.unwrap().unwrap();
    server.await.unwrap();

    assert_eq!(first.transport, CodexBackendTransport::WebSocket);
    assert_eq!(second.transport, CodexBackendTransport::HttpSse);
    assert_eq!(
        second.transport_metrics.decision,
        Some(CodexTransportDecision::Http2PoolUnavailable)
    );
}

#[tokio::test]
async fn payload_send_failure_should_not_open_http_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _payload = websocket.next().await.unwrap().unwrap();
        websocket.close(None).await.unwrap();
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()));

    let error = backend
        .create_response(
            &new_chain_request("conversation-post-send"),
            request_context("req_post_send", Some("chatgpt-account")),
        )
        .await
        .expect_err("post-send close must not be replayed");
    server.await.unwrap();

    std::assert_matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::PostSendAmbiguous { .. })
    );
}

#[tokio::test]
async fn origin_breaker_should_open_then_allow_only_one_half_open_probe() {
    let breaker = WebSocketOriginBreaker::with_config(WebSocketOriginBreakerConfig {
        failure_threshold: 3,
        failure_window: Duration::from_secs(1),
        open_duration: Duration::from_millis(20),
    });
    for _ in 0..3 {
        let WebSocketOriginBreakerDecision::Allowed(permit) =
            breaker.try_acquire("https://example.test:443")
        else {
            panic!("closed breaker should allow a connect");
        };
        permit.fast_timeout();
    }
    assert!(matches!(
        breaker.try_acquire("https://example.test:443"),
        WebSocketOriginBreakerDecision::Open
    ));

    tokio::time::sleep(Duration::from_millis(25)).await;
    let WebSocketOriginBreakerDecision::Allowed(probe) =
        breaker.try_acquire("https://example.test:443")
    else {
        panic!("expired open state should allow one probe");
    };
    assert!(probe.is_half_open_probe());
    assert!(matches!(
        breaker.try_acquire("https://example.test:443"),
        WebSocketOriginBreakerDecision::HalfOpenBusy
    ));
    probe.succeed();
    assert!(matches!(
        breaker.try_acquire("https://example.test:443"),
        WebSocketOriginBreakerDecision::Allowed(_)
    ));
}

#[tokio::test]
async fn half_open_upstream_response_should_close_origin_breaker() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let breaker = WebSocketOriginBreaker::with_config(WebSocketOriginBreakerConfig {
        failure_threshold: 1,
        failure_window: Duration::from_secs(1),
        open_duration: Duration::from_millis(20),
    });
    let server = tokio::spawn(async move {
        let (mut stalled_websocket, _) = listener.accept().await.unwrap();
        let opening = read_http_request(&mut stalled_websocket).await;
        assert!(opening.starts_with("GET /codex/responses HTTP/1.1"));

        let (mut http, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut http).await;
        assert!(request.starts_with("POST /codex/responses HTTP/1.1"));
        write_completed_sse_response(&mut http).await;

        let (mut probe, _) = listener.accept().await.unwrap();
        let opening = read_http_request(&mut probe).await;
        assert!(opening.starts_with("GET /codex/responses HTTP/1.1"));
        probe
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()))
    .with_websocket_fast_path_budget(Duration::from_millis(100))
    .with_websocket_origin_breaker(breaker.clone());

    backend
        .create_response(
            &new_chain_request("conversation-breaker-open"),
            request_context("req_breaker_open", Some("chatgpt-account")),
        )
        .await
        .expect("first timeout should use HTTP");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let error = backend
        .create_response(
            &new_chain_request("conversation-breaker-probe"),
            request_context("req_breaker_probe", Some("chatgpt-account")),
        )
        .await
        .expect_err("half-open account response should remain explicit");
    server.await.unwrap();

    std::assert_matches!(
        error,
        CodexClientError::Upstream {
            status,
            transport: CodexBackendTransport::WebSocket,
            ..
        } if status == reqwest::StatusCode::UNAUTHORIZED
    );
    assert!(matches!(
        breaker.try_acquire(&format!("http://{addr}")),
        WebSocketOriginBreakerDecision::Allowed(_)
    ));
}
