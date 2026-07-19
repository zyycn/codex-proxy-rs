use provider_openai::transport::{
    protocol::responses::{CodexResponsesRequest, PreviousResponseScope},
    websocket::{
        CodexWebSocketExchangeError, CodexWebSocketPool, PreviousResponseUnavailableReason,
        WebSocketOriginBreaker, WebSocketOriginBreakerConfig, WebSocketOriginBreakerDecision,
    },
};

use super::*;

fn new_chain_request(conversation_id: &str) -> CodexResponsesRequest {
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", Vec::new());
    request.use_websocket = true;
    request.local_conversation_id = Some(conversation_id.to_string());
    request
}

fn explicit_websocket_warmup_request(conversation_id: &str) -> CodexResponsesRequest {
    let mut body = serde_json::Map::new();
    body.insert("model".to_string(), json!("gpt-5.5"));
    body.insert("input".to_string(), json!([]));
    body.insert("generate".to_string(), json!(false));
    body.insert("store".to_string(), json!(false));
    let mut request = CodexResponsesRequest::from_body(body);
    request.use_websocket = true;
    request.local_conversation_id = Some(conversation_id.to_string());
    request
}

#[tokio::test]
async fn cold_websocket_should_fall_back_without_recording_a_successful_connect() {
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
    let pool = Arc::new(CodexWebSocketPool::default());
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool))
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
        Some(CodexTransportDecision::Http2WebSocketBudgetExhausted)
    );
    assert_eq!(response.transport_metrics.ws_connect_ms, None);
    assert!(response.transport_metrics.first_event_ms.is_some());
    assert!(response.body.contains("response.completed"));
    pool.shutdown().await;
}

#[tokio::test]
async fn timed_out_websocket_should_finish_in_background_and_serve_the_next_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (websocket_ready_tx, websocket_ready_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (websocket_stream, _) = listener.accept().await.unwrap();
        let websocket_server = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let mut websocket = accept_codex_test_websocket(websocket_stream).await;
            websocket_ready_tx.send(()).unwrap();
            let _payload = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response("resp_background_ready", 2, 1).into(),
                ))
                .await
                .unwrap();
        });

        let (mut http, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut http).await;
        assert!(request.starts_with("POST /codex/responses HTTP/1.1"));
        write_completed_sse_response(&mut http).await;
        websocket_server.await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool))
    .with_websocket_fast_path_budget(Duration::from_millis(30));
    let request = new_chain_request("conversation-background-ready");

    let first = backend
        .create_response(
            &request,
            request_context("req_background_http", Some("chatgpt-account")),
        )
        .await
        .expect("foreground request should use HTTP after its fast-path budget");
    websocket_ready_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = backend
        .create_response(
            &request,
            request_context("req_background_reuse", Some("chatgpt-account")),
        )
        .await
        .expect("next request should reuse the background websocket");
    server.await.unwrap();

    assert_eq!(first.transport, CodexBackendTransport::HttpSse);
    assert_eq!(first.transport_metrics.ws_connect_ms, None);
    assert_eq!(second.transport, CodexBackendTransport::WebSocket);
    assert_eq!(
        second.transport_metrics.decision,
        Some(CodexTransportDecision::ReusedWebSocket)
    );
    assert!(second.body.contains("resp_background_ready"));
    pool.shutdown().await;
}

#[tokio::test]
async fn shared_websocket_opening_should_keep_the_original_fast_path_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stalled_websocket, _) = listener.accept().await.unwrap();
        let opening = read_http_request(&mut stalled_websocket).await;
        assert!(opening.starts_with("GET /codex/responses HTTP/1.1"));

        for _ in 0..2 {
            let (mut http, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut http).await;
            assert!(request.starts_with("POST /codex/responses HTTP/1.1"));
            write_completed_sse_response(&mut http).await;
        }
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool))
    .with_websocket_fast_path_budget(Duration::from_millis(200));
    let request = new_chain_request("conversation-original-deadline");

    let first = backend
        .create_response(
            &request,
            request_context("req_original_deadline_first", Some("chatgpt-account")),
        )
        .await
        .expect("first request should use HTTP after the opening budget");
    let second = timeout(
        Duration::from_millis(100),
        backend.create_response(
            &request,
            request_context("req_original_deadline_second", Some("chatgpt-account")),
        ),
    )
    .await
    .expect("shared opening must not grant the second request another full budget")
    .expect("second request should use HTTP");
    server.await.unwrap();

    assert_eq!(first.transport, CodexBackendTransport::HttpSse);
    assert_eq!(second.transport, CodexBackendTransport::HttpSse);
    assert_eq!(
        second.transport_metrics.decision,
        Some(CodexTransportDecision::Http2WebSocketBudgetExhausted)
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn account_eviction_should_cancel_a_background_websocket_opening() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stale_opening, _) = listener.accept().await.unwrap();
        let opening = read_http_request(&mut stale_opening).await;
        assert!(opening.starts_with("GET /codex/responses HTTP/1.1"));

        let (mut http, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut http).await;
        assert!(request.starts_with("POST /codex/responses HTTP/1.1"));
        write_completed_sse_response(&mut http).await;

        let mut byte = [0_u8; 1];
        let read = timeout(Duration::from_secs(1), stale_opening.read(&mut byte))
            .await
            .expect("account eviction should close the opening socket")
            .unwrap();
        assert_eq!(read, 0);

        let (fresh_stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(fresh_stream).await;
        let _payload = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_after_eviction", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool))
    .with_websocket_fast_path_budget(Duration::from_millis(30));
    let request = new_chain_request("conversation-evict-opening");

    let first = backend
        .create_response(
            &request,
            request_context("req_evict_opening_first", Some("chatgpt-account")),
        )
        .await
        .expect("first request should use HTTP after the opening budget");
    pool.evict_account("chatgpt-account").await;
    let second = timeout(
        Duration::from_secs(2),
        backend
            .clone()
            .with_websocket_fast_path_budget(Duration::from_millis(500))
            .create_response(
                &request,
                request_context("req_evict_opening_second", Some("chatgpt-account")),
            ),
    )
    .await
    .expect("fresh websocket request should finish")
    .expect("the next request should build a fresh websocket");
    timeout(Duration::from_secs(2), server)
        .await
        .expect("test server should finish")
        .unwrap();

    assert_eq!(first.transport, CodexBackendTransport::HttpSse);
    assert_eq!(second.transport, CodexBackendTransport::WebSocket);
    assert!(second.body.contains("resp_after_eviction"));
    timeout(Duration::from_secs(2), pool.shutdown())
        .await
        .expect("pool shutdown should finish");
}

#[tokio::test]
async fn pool_shutdown_should_cancel_and_join_a_background_websocket_opening() {
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

        let mut byte = [0_u8; 1];
        let read = timeout(Duration::from_secs(1), stalled_websocket.read(&mut byte))
            .await
            .expect("pool shutdown should close the opening socket")
            .unwrap();
        assert_eq!(read, 0);
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool))
    .with_websocket_fast_path_budget(Duration::from_millis(30));

    backend
        .create_response(
            &new_chain_request("conversation-shutdown-opening"),
            request_context("req_shutdown_opening", Some("chatgpt-account")),
        )
        .await
        .expect("foreground request should use HTTP after the opening budget");
    timeout(Duration::from_secs(1), pool.shutdown())
        .await
        .expect("shutdown should join the cancelled opening task");
    server.await.unwrap();

    assert!(pool.is_shutdown().await);
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
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()));
    let request = explicit_websocket_warmup_request("conversation-warmup-required");

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
        test_wire_profile(),
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
        test_wire_profile(),
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
    // DB 只保存 upstream response ID，不保存客户端 conversation ID；续接必须仍能
    // 按 origin/account/native handle 找回原连接。
    second_request.local_conversation_id = Some("conversation-after-restart".to_string());
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

    assert!(first.connection_local_continuation_expires_at.is_some());
    assert!(first.transport_metrics.first_event_ms.is_some());
    assert!(second.connection_local_continuation_expires_at.is_some());
    assert_eq!(
        second.transport_metrics.decision,
        Some(CodexTransportDecision::ExactWebSocket)
    );
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_same_handle_should_allow_one_claim_and_advance_the_live_socket_handle() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (continuation_started_tx, continuation_started_rx) = tokio::sync::oneshot::channel();
    let (release_continuation_tx, release_continuation_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _seed = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_single_use_old", 2, 1).into(),
            ))
            .await
            .unwrap();

        let first_continuation = websocket.next().await.unwrap().unwrap();
        let Message::Text(first_continuation) = first_continuation else {
            panic!("continuation request should be text");
        };
        let first_continuation: serde_json::Value =
            serde_json::from_str(&first_continuation).unwrap();
        assert_eq!(
            first_continuation["previous_response_id"],
            "resp_single_use_old"
        );
        continuation_started_tx.send(()).unwrap();
        release_continuation_rx.await.unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_single_use_new", 2, 1).into(),
            ))
            .await
            .unwrap();

        let latest_continuation = websocket.next().await.unwrap().unwrap();
        let Message::Text(latest_continuation) = latest_continuation else {
            panic!("latest continuation request should be text");
        };
        let latest_continuation: serde_json::Value =
            serde_json::from_str(&latest_continuation).unwrap();
        assert_eq!(
            latest_continuation["previous_response_id"],
            "resp_single_use_new"
        );
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_single_use_latest", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let backend = Arc::new(
        CodexBackendClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            format!("http://{addr}"),
            test_wire_profile(),
        )
        .with_websocket_pool(Arc::new(CodexWebSocketPool::default())),
    );
    let seed = new_chain_request("conversation-single-use");
    backend
        .create_response(
            &seed,
            request_context("req_single_use_seed", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    let continuation = |response_id: &str| {
        let mut request = seed.clone();
        request.set_previous_response_id(Some(response_id.to_string()));
        request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
        request
    };
    let first_backend = Arc::clone(&backend);
    let first_request = continuation("resp_single_use_old");
    let first = tokio::spawn(async move {
        first_backend
            .create_response(
                &first_request,
                request_context("req_single_use_first", Some("chatgpt-account")),
            )
            .await
    });
    continuation_started_rx.await.unwrap();

    let concurrent_error = backend
        .create_response(
            &continuation("resp_single_use_old"),
            request_context("req_single_use_concurrent", Some("chatgpt-account")),
        )
        .await
        .expect_err("the same live handle must not be used concurrently");
    std::assert_matches!(
        concurrent_error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            reason: PreviousResponseUnavailableReason::ConnectionBusy
        })
    );

    release_continuation_tx.send(()).unwrap();
    let first = first.await.unwrap().unwrap();
    assert!(first.body.contains("resp_single_use_new"));
    let stale_error = backend
        .create_response(
            &continuation("resp_single_use_old"),
            request_context("req_single_use_stale", Some("chatgpt-account")),
        )
        .await
        .expect_err("the previous live handle must be terminal after completion");
    std::assert_matches!(
        stale_error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            reason: PreviousResponseUnavailableReason::FreshConnectionRequired
        })
    );
    let latest = backend
        .create_response(
            &continuation("resp_single_use_new"),
            request_context("req_single_use_latest", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    assert!(latest.body.contains("resp_single_use_latest"));
    server.await.unwrap();
}

#[tokio::test]
async fn concurrent_multi_conversation_continuations_should_select_each_exact_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first = accept_codex_test_websocket(first_stream).await;
        let _ = first.next().await.unwrap().unwrap();
        first
            .send(Message::Text(
                completed_websocket_response("resp_profile_a", 2, 1).into(),
            ))
            .await
            .unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second = accept_codex_test_websocket(second_stream).await;
        let _ = second.next().await.unwrap().unwrap();
        second
            .send(Message::Text(
                completed_websocket_response("resp_profile_b", 2, 1).into(),
            ))
            .await
            .unwrap();

        let first_continuation = async {
            let message = timeout(Duration::from_secs(2), first.next())
                .await
                .expect("profile A should receive its continuation")
                .unwrap()
                .unwrap();
            let Message::Text(payload) = message else {
                panic!("profile A continuation should be text");
            };
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(payload["previous_response_id"], "resp_profile_a");
            first
                .send(Message::Text(
                    completed_websocket_response("resp_profile_a_next", 2, 1).into(),
                ))
                .await
                .unwrap();
        };
        let second_continuation = async {
            let message = timeout(Duration::from_secs(2), second.next())
                .await
                .expect("profile B should receive its continuation")
                .unwrap()
                .unwrap();
            let Message::Text(payload) = message else {
                panic!("profile B continuation should be text");
            };
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(payload["previous_response_id"], "resp_profile_b");
            second
                .send(Message::Text(
                    completed_websocket_response("resp_profile_b_next", 2, 1).into(),
                ))
                .await
                .unwrap();
        };
        tokio::join!(first_continuation, second_continuation);
    });

    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let first_seed = new_chain_request("conversation-exact-a");
    let second_seed = new_chain_request("conversation-exact-b");

    backend
        .create_response(
            &first_seed,
            request_context("req_profile_a_seed", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    backend
        .create_response(
            &second_seed,
            request_context("req_profile_b_seed", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    let continuation = |seed: &CodexResponsesRequest, response_id: &str| {
        let mut continuation = seed.clone();
        continuation.set_previous_response_id(Some(response_id.to_string()));
        continuation.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
        continuation
    };
    let first_request = continuation(&first_seed, "resp_profile_a");
    let second_request = continuation(&second_seed, "resp_profile_b");
    let start = Arc::new(tokio::sync::Barrier::new(3));
    let first_backend = backend.clone();
    let first_start = Arc::clone(&start);
    let first = tokio::spawn(async move {
        first_start.wait().await;
        first_backend
            .create_response(
                &first_request,
                request_context("req_profile_a_next", Some("chatgpt-account")),
            )
            .await
    });
    let second_backend = backend.clone();
    let second_start = Arc::clone(&start);
    let second = tokio::spawn(async move {
        second_start.wait().await;
        second_backend
            .create_response(
                &second_request,
                request_context("req_profile_b_next", Some("chatgpt-account")),
            )
            .await
    });
    start.wait().await;

    let first = first.await.unwrap().unwrap();
    let second = second.await.unwrap().unwrap();
    timeout(Duration::from_secs(2), server)
        .await
        .expect("multi-profile server should finish")
        .unwrap();

    assert!(first.body.contains("resp_profile_a_next"));
    assert!(second.body.contains("resp_profile_b_next"));
    pool.shutdown().await;
}

#[tokio::test]
async fn missing_exact_socket_should_fail_without_opening_a_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
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
async fn connection_local_continuation_should_fail_after_its_live_socket_disappears() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _seed = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_disappeared", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        closed_tx.send(()).unwrap();
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()));
    let seed = new_chain_request("conversation-disappeared");
    backend
        .create_response(
            &seed,
            request_context("req_disappeared_seed", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    closed_rx.await.unwrap();
    server.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    let mut continuation = seed;
    continuation.set_previous_response_id(Some("resp_disappeared".to_string()));
    continuation.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
    let error = backend
        .create_response(
            &continuation,
            request_context("req_disappeared_next", Some("chatgpt-account")),
        )
        .await
        .expect_err("a disappeared live socket cannot be reconstructed from the database");
    std::assert_matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            reason: PreviousResponseUnavailableReason::FreshConnectionRequired
        })
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
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()))
    // 该用例验证 same-key singleflight，不验证 200ms deadline。并行全套测试下
    // 调度停顿可能让首请求也越过 200ms，制造两个 HTTP fallback 的假失败。
    .with_websocket_fast_path_budget(Duration::from_secs(2));
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
    timeout(Duration::from_secs(5), server)
        .await
        .expect("singleflight server should finish")
        .unwrap();

    assert_eq!(first.transport, CodexBackendTransport::WebSocket);
    assert_eq!(
        first.transport_metrics.decision,
        Some(CodexTransportDecision::ConnectedWebSocket)
    );
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
        test_wire_profile(),
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
async fn cancelled_half_open_opening_should_allow_another_probe() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let origin_key = format!("http://{addr}");
    let breaker = WebSocketOriginBreaker::with_config(WebSocketOriginBreakerConfig {
        failure_threshold: 1,
        failure_window: Duration::from_secs(1),
        open_duration: Duration::ZERO,
    });
    let WebSocketOriginBreakerDecision::Allowed(permit) = breaker.try_acquire(&origin_key) else {
        panic!("closed breaker should grant the initial permit");
    };
    permit.fast_timeout();

    let (opening_started_tx, opening_started_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut opening, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut opening).await;
        assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
        opening_started_tx.send(()).unwrap();

        let mut byte = [0_u8; 1];
        let read = timeout(Duration::from_secs(1), opening.read(&mut byte))
            .await
            .expect("account eviction should close the half-open probe")
            .unwrap();
        assert_eq!(read, 0);
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        origin_key.clone(),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool))
    .with_websocket_origin_breaker(breaker.clone());
    let attempt = tokio::spawn(async move {
        backend
            .create_response(
                &explicit_websocket_warmup_request("conversation-cancelled-half-open"),
                request_context("req_cancelled_half_open", Some("chatgpt-account")),
            )
            .await
    });

    opening_started_rx.await.unwrap();
    pool.evict_account("chatgpt-account").await;
    timeout(Duration::from_secs(1), attempt)
        .await
        .expect("cancelled request should finish")
        .unwrap()
        .expect_err("cancelled half-open opening should fail the request");
    server.await.unwrap();

    let WebSocketOriginBreakerDecision::Allowed(next_probe) = breaker.try_acquire(&origin_key)
    else {
        panic!("cancelled half-open probe should release its ownership");
    };
    assert!(next_probe.is_half_open_probe());
    next_probe.succeed();
    pool.shutdown().await;
}

#[test]
fn origin_breaker_should_count_hard_opening_failures() {
    let breaker = WebSocketOriginBreaker::with_config(WebSocketOriginBreakerConfig {
        failure_threshold: 1,
        failure_window: Duration::from_secs(1),
        open_duration: Duration::from_secs(1),
    });
    let WebSocketOriginBreakerDecision::Allowed(permit) =
        breaker.try_acquire("https://example.test:443")
    else {
        panic!("closed breaker should allow an opening");
    };
    permit.fail();

    assert!(matches!(
        breaker.try_acquire("https://example.test:443"),
        WebSocketOriginBreakerDecision::Open
    ));
}

#[tokio::test]
async fn fast_path_miss_and_late_failure_should_count_as_one_breaker_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (late_failure_tx, late_failure_rx) = tokio::sync::oneshot::channel();
    let (second_used_websocket_tx, second_used_websocket_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut delayed_opening, _) = listener.accept().await.unwrap();
        let opening = read_http_request(&mut delayed_opening).await;
        assert!(opening.starts_with("GET /codex/responses HTTP/1.1"));
        let delayed_failure = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            delayed_opening
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            late_failure_tx.send(()).unwrap();
        });

        let (mut first_http, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut first_http).await;
        assert!(request.starts_with("POST /codex/responses HTTP/1.1"));
        write_completed_sse_response(&mut first_http).await;
        delayed_failure.await.unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let second_is_websocket = timeout(Duration::from_secs(1), async {
            let mut prefix = [0_u8; 4];
            loop {
                let read = second_stream.peek(&mut prefix).await.unwrap();
                if read == 0 {
                    return false;
                }
                if read == prefix.len() {
                    return prefix == *b"GET ";
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second request should reach the upstream");
        if second_is_websocket {
            second_used_websocket_tx.send(true).unwrap();
            let mut websocket = accept_codex_test_websocket(second_stream).await;
            let _payload = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response("resp_after_late_failure", 2, 1).into(),
                ))
                .await
                .unwrap();
        } else {
            second_used_websocket_tx.send(false).unwrap();
            let mut second_http = second_stream;
            let request = read_http_request(&mut second_http).await;
            assert!(request.starts_with("POST /codex/responses HTTP/1.1"));
            write_completed_sse_response(&mut second_http).await;
        }
    });
    let breaker = WebSocketOriginBreaker::with_config(WebSocketOriginBreakerConfig {
        failure_threshold: 2,
        failure_window: Duration::from_secs(1),
        open_duration: Duration::from_secs(1),
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool))
    .with_websocket_fast_path_budget(Duration::from_millis(30))
    .with_websocket_origin_breaker(breaker);

    let first = backend
        .create_response(
            &new_chain_request("conversation-late-failure-first"),
            request_context("req_late_failure_first", Some("chatgpt-account")),
        )
        .await
        .expect("first request should use HTTP after the opening budget");
    timeout(Duration::from_secs(2), late_failure_rx)
        .await
        .expect("delayed opening should receive its 503 response")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = timeout(
        Duration::from_secs(2),
        backend
            .clone()
            .with_websocket_fast_path_budget(Duration::from_millis(500))
            .create_response(
                &new_chain_request("conversation-late-failure-second"),
                request_context("req_late_failure_second", Some("chatgpt-account")),
            ),
    )
    .await
    .expect("second request should finish")
    .expect("one degraded opening must not open a threshold-two breaker");
    let second_used_websocket = timeout(Duration::from_secs(2), second_used_websocket_rx)
        .await
        .expect("test server should observe the second transport")
        .unwrap();
    timeout(Duration::from_secs(2), server)
        .await
        .expect("test server should finish")
        .unwrap();

    assert_eq!(first.transport, CodexBackendTransport::HttpSse);
    assert!(second_used_websocket);
    assert_eq!(second.transport, CodexBackendTransport::WebSocket);
    assert!(second.body.contains("resp_after_late_failure"));
    timeout(Duration::from_secs(2), pool.shutdown())
        .await
        .expect("pool shutdown should finish");
}

#[tokio::test]
async fn half_open_upstream_response_should_close_origin_breaker() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let origin_key = format!("http://{addr}");
    let breaker = WebSocketOriginBreaker::with_config(WebSocketOriginBreakerConfig {
        failure_threshold: 1,
        failure_window: Duration::from_secs(1),
        open_duration: Duration::ZERO,
    });
    let WebSocketOriginBreakerDecision::Allowed(permit) = breaker.try_acquire(&origin_key) else {
        panic!("closed breaker should grant the initial permit");
    };
    permit.fast_timeout();

    let server = tokio::spawn(async move {
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
        origin_key.clone(),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()))
    .with_websocket_fast_path_budget(Duration::from_secs(1))
    .with_websocket_origin_breaker(breaker.clone());

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
        breaker.try_acquire(&origin_key),
        WebSocketOriginBreakerDecision::Allowed(_)
    ));
}
