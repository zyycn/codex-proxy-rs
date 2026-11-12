//! 验证 WebSocket 冷建连时限、后台完成与续接请求的传输选择

use provider_openai::transport::{
    protocol::responses::{CodexResponsesRequest, PreviousResponseScope},
    websocket::{
        CodexWebSocketExchangeError, CodexWebSocketPool, PreviousResponseUnavailableReason,
        WebSocketOriginBreaker, WebSocketOriginBreakerConfig, WebSocketOriginBreakerDecision,
    },
};
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

use super::*;

fn new_chain_request(conversation_id: &str) -> CodexResponsesRequest {
    let mut request = codex_request("gpt-5.5", "be brief", Vec::new());
    request.use_websocket = true;
    request.local_conversation_id = Some(conversation_id.to_string());
    request
}

fn external_continuation_request(conversation_id: &str) -> CodexResponsesRequest {
    let mut request = new_chain_request(conversation_id);
    request.set_previous_response_id(Some("resp_from_another_account".to_owned()));
    request.previous_response_scope = Some(PreviousResponseScope::ExternalUnknown);
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
async fn advertised_websocket_limit_counts_final_utf8_bytes_and_allows_equality() {
    let mut payload_len = 0;
    for case in [
        "absent", "invalid", "negative", "overflow", "equal", "below", "zero", "large",
    ] {
        let mut request = codex_request(
            "gpt-5.5",
            if case == "large" {
                "x".repeat(17 * 1024 * 1024)
            } else {
                "中文\n\"\\".to_owned()
            },
            Vec::new(),
        );
        request.use_websocket = true;
        request.local_conversation_id = Some("advertised-limit".to_owned());
        let limit = match case {
            "absent" => None,
            "invalid" => Some("invalid".to_owned()),
            "negative" => Some("-1".to_owned()),
            "overflow" => Some("184467440737095516160".to_owned()),
            "equal" => Some(payload_len.to_string()),
            "below" => Some((payload_len - 1).to_string()),
            "zero" => Some("0".to_owned()),
            "large" => Some((128 * 1024 * 1024).to_string()),
            _ => unreachable!(),
        };
        let fallback = matches!(case, "below" | "zero");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut websocket = accept_codex_test_websocket_with(socket, |_, response| {
                if let Some(limit) = limit {
                    response.headers_mut().insert(
                        "x-codex-websocket-max-message-bytes",
                        limit.parse().unwrap(),
                    );
                }
            })
            .await;
            if fallback {
                while let Some(Ok(message)) = websocket.next().await {
                    assert!(!message.is_text(), "oversized payload must not be uploaded");
                }
                let (mut http, _) = listener.accept().await.unwrap();
                let request = read_http_request_with_body(&mut http).await;
                assert!(request.starts_with(b"POST /codex/responses HTTP/1.1"));
                write_completed_sse_response(&mut http).await;
                0
            } else {
                let message = websocket.next().await.unwrap().unwrap();
                let text = message.to_text().unwrap();
                let length = text.len();
                assert_eq!(
                    serde_json::from_str::<Value>(text).unwrap()["type"],
                    "response.create"
                );
                websocket
                    .send(Message::Text(
                        completed_websocket_response("resp_limit", 1, 1).into(),
                    ))
                    .await
                    .unwrap();
                length
            }
        });
        let backend = CodexBackendClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            format!("http://{addr}"),
            test_wire_profile(),
        );
        let response = timeout(
            Duration::from_secs(10),
            backend.create_response(
                &request,
                request_context("req_advertised_limit", Some("chatgpt-account")),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            response.transport,
            if fallback {
                CodexBackendTransport::HttpSse
            } else {
                CodexBackendTransport::WebSocket
            },
            "{case}"
        );
        if fallback {
            assert_eq!(
                response.transport_metrics.decision,
                Some(CodexTransportDecision::Http2WebSocketMessageTooLarge)
            );
        }
        let actual_len = server.await.unwrap();
        if case == "absent" {
            payload_len = actual_len;
        }
        if case == "equal" {
            assert_eq!(actual_len, payload_len);
        }
    }
}

#[tokio::test]
async fn advertised_websocket_limit_rejects_warmup_without_http_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket_with(socket, |_, response| {
            response
                .headers_mut()
                .insert("x-codex-websocket-max-message-bytes", "1".parse().unwrap());
        })
        .await;
        while let Some(Ok(message)) = websocket.next().await {
            assert!(!message.is_text());
        }
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
            &explicit_websocket_warmup_request("oversized-warmup"),
            request_context("req_oversized_warmup", Some("chatgpt-account")),
        )
        .await
        .unwrap_err();
    std::assert_matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::MessageTooLarge)
    );
    server.await.unwrap();
}

async fn reject_websocket_openings(listener: &TcpListener, attempts: usize) {
    for _ in 0..attempts {
        let (mut opening, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut opening).await;
        assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
        opening
            .write_all(
                b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    }
}

async fn record_websocket_origin_failures(
    backend: &CodexBackendClient,
    attempts: usize,
    scenario: &str,
) {
    for attempt in 0..attempts {
        let conversation_id = format!("conversation-{scenario}-{attempt}");
        let request_id = format!("req_{scenario}_{attempt}");
        let error = backend
            .create_response(
                &new_chain_request(&conversation_id),
                request_context(&request_id, Some("chatgpt-account")),
            )
            .await
            .expect_err("seed WebSocket opening should fail");
        match error {
            CodexClientError::Upstream { status, .. }
                if status == reqwest::StatusCode::SERVICE_UNAVAILABLE => {}
            error => panic!("{scenario} opening {attempt} failed unexpectedly: {error}"),
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
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
    .with_websocket_pool(Arc::clone(&pool));

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
async fn downstream_websocket_new_chain_should_preserve_continuation_after_slow_opening() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
            accepted = listener.accept() => {
                let (mut http, _) = accepted.unwrap();
                let request = read_http_request(&mut http).await;
                assert!(request.starts_with("POST /codex/responses HTTP/1.1"));
                write_completed_sse_response(&mut http).await;
                return;
            }
        }
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _first = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_slow_seed", 2, 1).into(),
            ))
            .await
            .unwrap();

        let second = websocket.next().await.unwrap().unwrap();
        let second: Value = serde_json::from_str(second.to_text().unwrap()).unwrap();
        assert_eq!(second["previous_response_id"], "resp_slow_seed");
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_slow_continuation", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request = new_chain_request("conversation-downstream-slow");
    request.downstream_websocket_connection_id = Some("ws_downstream_slow".to_owned());

    let first = backend
        .create_response(
            &request,
            request_context("req_slow_seed", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    assert_eq!(first.transport, CodexBackendTransport::WebSocket);
    assert!(first.connection_local_continuation);

    request.set_previous_response_id(Some("resp_slow_seed".to_owned()));
    request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
    let second = backend
        .create_response(
            &request,
            request_context("req_slow_continuation", Some("chatgpt-account")),
        )
        .await
        .expect("the next turn must use the socket that generated the previous response");
    assert_eq!(
        second.transport_metrics.decision,
        Some(CodexTransportDecision::ExactWebSocket)
    );
    assert!(second.body.contains("resp_slow_continuation"));
    server.await.unwrap();
    pool.shutdown().await;
}

#[tokio::test]
async fn external_continuation_should_wait_for_a_cold_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (websocket_stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut websocket = accept_codex_test_websocket(websocket_stream).await;
        let _payload = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_cross_account", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));

    let response = backend
        .create_response(
            &external_continuation_request("conversation-cross-account"),
            request_context("req_cross_account", Some("chatgpt-account-b")),
        )
        .await
        .expect("external continuation should complete over WebSocket");
    server.await.unwrap();

    assert_eq!(response.transport, CodexBackendTransport::WebSocket);
    pool.shutdown().await;
}

#[tokio::test]
async fn slow_pooled_openings_should_remain_healthy_and_serve_subsequent_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let openings = WebSocketOriginBreakerConfig::default().failure_threshold + 1;
    let (websocket_ready_tx, mut websocket_ready_rx) = tokio::sync::mpsc::channel(1);
    let server = tokio::spawn(async move {
        for _ in 0..openings {
            let (mut websocket_stream, _) = listener.accept().await.unwrap();
            let mut prefix = [0; 4];
            loop {
                let read = websocket_stream.peek(&mut prefix).await.unwrap();
                assert!(read > 0, "opening socket closed before the request prefix");
                if read == prefix.len() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            if prefix == *b"POST" {
                read_http_request(&mut websocket_stream).await;
                write_completed_sse_response(&mut websocket_stream).await;
                continue;
            }
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let ready = websocket_ready_tx.clone();
            let websocket_server = tokio::spawn(async move {
                release_rx.await.unwrap();
                let mut websocket = accept_codex_test_websocket(websocket_stream).await;
                ready.send(()).await.unwrap();
                // 回退的请求只发 HTTP，WS 的首个业务帧必须来自后续请求
                let payload = websocket.next().await.unwrap().unwrap();
                let payload: Value = serde_json::from_str(payload.to_text().unwrap()).unwrap();
                assert_eq!(payload["input"][0]["content"], "subsequent request");
                assert_eq!(payload["tools"][0]["type"], "web_search");
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
            release_tx.send(()).unwrap();
            websocket_server.await.unwrap();
        }
    });
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    for index in 0..openings {
        let request = new_chain_request(&format!("conversation-background-{index}"));
        let first = backend
            .create_response(
                &request,
                request_context("req_background_http", Some("chatgpt-account")),
            )
            .await
            .expect("foreground request should use HTTP after its fast-path budget");
        assert_eq!(first.transport, CodexBackendTransport::HttpSse);
        assert_eq!(first.transport_metrics.ws_connect_ms, None);
        assert_eq!(
            first.transport_metrics.decision,
            Some(CodexTransportDecision::Http2WebSocketBudgetExhausted)
        );
        websocket_ready_rx.recv().await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut body = codex_request_body(
            "gpt-5.5",
            "be brief",
            vec![json!({"role":"user","content":"subsequent request"})],
        );
        // 后台只完成握手，首次写入可以直接声明搜索，不应按无搜索首包回收
        body.insert("tools".into(), json!([{"type":"web_search"}]));
        let mut subsequent = CodexResponsesRequest::from_body(body);
        subsequent.use_websocket = true;
        subsequent.local_conversation_id = request.local_conversation_id.clone();
        let second = backend
            .create_response(
                &subsequent,
                request_context("req_background_reuse", Some("chatgpt-account")),
            )
            .await
            .expect("next request should reuse the background websocket");

        assert_eq!(second.transport, CodexBackendTransport::WebSocket);
        assert_eq!(
            second.transport_metrics.decision,
            Some(CodexTransportDecision::ReusedWebSocket)
        );
        assert!(second.body.contains("resp_background_ready"));
    }
    server.await.unwrap();
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
    .with_websocket_pool(Arc::clone(&pool));
    let request = new_chain_request("conversation-original-deadline");

    let first = backend
        .create_response(
            &request,
            request_context("req_original_deadline_first", Some("chatgpt-account")),
        )
        .await
        .expect("first request should use HTTP after the opening budget");
    let second_started_at = tokio::time::Instant::now();
    let second = backend
        .create_response(
            &request,
            request_context("req_original_deadline_second", Some("chatgpt-account")),
        )
        .await
        .expect("second request should use HTTP");
    let second_elapsed = second_started_at.elapsed();
    server.await.unwrap();

    assert_eq!(first.transport, CodexBackendTransport::HttpSse);
    assert_eq!(second.transport, CodexBackendTransport::HttpSse);
    assert!(
        second_elapsed < Duration::from_millis(400),
        "shared opening waited for a new budget: {second_elapsed:?}"
    );
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
        let read = stale_opening.read(&mut byte).await.unwrap();
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
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let request = new_chain_request("conversation-evict-opening");

    let first = backend
        .create_response(
            &request,
            request_context("req_evict_opening_first", Some("chatgpt-account")),
        )
        .await
        .expect("first request should use HTTP after the opening budget");
    pool.evict_account("chatgpt-account").await;
    let second = backend
        .clone()
        .create_response(
            &request,
            request_context("req_evict_opening_second", Some("chatgpt-account")),
        )
        .await
        .expect("the next request should build a fresh websocket");
    server.await.unwrap();

    assert_eq!(first.transport, CodexBackendTransport::HttpSse);
    assert_eq!(second.transport, CodexBackendTransport::WebSocket);
    assert!(second.body.contains("resp_after_eviction"));
    pool.shutdown().await;
}

#[tokio::test(start_paused = true)]
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
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));

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

    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
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
async fn connection_local_continuation_without_a_pool_should_not_open_http_or_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    );
    let mut request = new_chain_request("conversation-exact-no-pool");
    request.set_previous_response_id(Some("resp_no_pool".to_owned()));
    request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);

    let error = backend
        .create_response(
            &request,
            request_context("req_exact_no_pool", Some("chatgpt-account")),
        )
        .await
        .expect_err("connection-local continuation requires its owning pool");

    std::assert_matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            reason: PreviousResponseUnavailableReason::PoolUnavailable
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
        drop(websocket);
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
    let CodexClientError::WebSocket(error) = error else {
        panic!("disappeared connection should remain a typed WebSocket error");
    };
    assert_eq!(
        error.continuation_unavailable_reason(),
        Some(PreviousResponseUnavailableReason::ReusedConnectionLost)
    );
    let observation = error
        .connection_observation()
        .expect("disappeared connection should retain its lifecycle observation");
    assert_eq!(observation.exit_reason(), "reset_without_closing_handshake");
    assert!(observation.age_ms() >= observation.idle_ms());
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
    .with_websocket_pool(Arc::new(CodexWebSocketPool::default()));
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
async fn websocket_failure_before_first_delivery_should_not_replay_payload() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket_with(stream, |request, response| {
            assert_eq!(
                request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer access-token")
            );
            assert_eq!(
                request
                    .headers()
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok()),
                Some("chatgpt-account")
            );
            response.headers_mut().insert(
                "sec-websocket-extensions",
                "permessage-deflate".parse().unwrap(),
            );
        })
        .await;
        let _payload = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.created",
                    "response": {
                        "id": "resp_abandoned_websocket",
                        "model": "gpt-test"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.in_progress",
                    "response": {
                        "id": "resp_abandoned_websocket",
                        "model": "gpt-test"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .close(Some(CloseFrame {
                code: CloseCode::Size,
                reason: "message too big".into(),
            }))
            .await
            .unwrap();

        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "transport must not replay over HTTP or WS"
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
            &new_chain_request("conversation-pre-delivery-fallback"),
            request_context("req_pre_delivery_fallback", Some("chatgpt-account")),
        )
        .await
        .expect_err("sent payload must not be replayed");
    server.await.unwrap();

    assert!(matches!(error, CodexClientError::WebSocket(_)));
}

#[tokio::test]
async fn websocket_failure_after_first_delivery_should_not_open_http_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _payload = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "delivered"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .close(Some(CloseFrame {
                code: CloseCode::Size,
                reason: "message too big after output".into(),
            }))
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
            &new_chain_request("conversation-post-delivery"),
            request_context("req_post_delivery", Some("chatgpt-account")),
        )
        .await
        .expect_err("post-delivery close must not be replayed");
    server.await.unwrap();

    let CodexClientError::WebSocket(CodexWebSocketExchangeError::PostSendAmbiguous {
        source: Some(source),
        ..
    }) = error
    else {
        panic!("expected typed post-delivery WebSocket failure");
    };
    let close = source
        .close_before_terminal()
        .expect("expected upstream close frame source");
    assert!(
        close.connection_id().is_some(),
        "close failures must retain their WebSocket connection correlation"
    );
    assert_eq!(close.code(), Some(1009));
    assert_eq!(close.reason(), Some("message too big after output"));
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
async fn slow_half_open_probe_should_remain_singleflight_and_recover_on_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = WebSocketOriginBreakerConfig::default();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        reject_websocket_openings(&listener, config.failure_threshold).await;
        let (stream, _) = listener.accept().await.unwrap();
        let probe = tokio::spawn(async move {
            release_rx.await.unwrap();
            let mut websocket = accept_codex_test_websocket(stream).await;
            ready_tx.send(()).unwrap();
            let _payload = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response("resp_probe_reused", 2, 1).into(),
                ))
                .await
                .unwrap();
        });
        // 探测未完成前，预算耗尽的原请求和其他新会话都只能发送 HTTP
        for _ in 0..2 {
            let (mut http, _) = listener.accept().await.unwrap();
            assert!(
                read_http_request(&mut http)
                    .await
                    .starts_with("POST /codex/responses")
            );
            write_completed_sse_response(&mut http).await;
        }
        probe.await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _payload = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_after_recovery", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    record_websocket_origin_failures(&backend, config.failure_threshold, "slow-half-open").await;
    tokio::time::pause();
    tokio::time::advance(config.open_duration).await;
    tokio::time::resume();

    let probe_request = new_chain_request("slow-half-open-probe");
    let fallback = backend
        .create_response(
            &probe_request,
            request_context("req_slow_probe", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    assert_eq!(
        fallback.transport_metrics.decision,
        Some(CodexTransportDecision::Http2WebSocketBudgetExhausted)
    );
    let suppressed = backend
        .create_response(
            &new_chain_request("other-cold-conversation"),
            request_context("req_probe_busy", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    assert_eq!(
        suppressed.transport_metrics.decision,
        Some(CodexTransportDecision::Http2BreakerOpen)
    );
    release_tx.send(()).unwrap();
    ready_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let reused = backend
        .create_response(
            &probe_request,
            request_context("req_probe_reuse", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    assert_eq!(
        reused.transport_metrics.decision,
        Some(CodexTransportDecision::ReusedWebSocket)
    );
    let fresh = backend
        .create_response(
            &new_chain_request("after-probe-recovery"),
            request_context("req_after_probe", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    assert_eq!(fresh.transport, CodexBackendTransport::WebSocket);
    assert_eq!(
        fresh.transport_metrics.decision,
        Some(CodexTransportDecision::ConnectedWebSocket)
    );
    server.await.unwrap();
    pool.shutdown().await;
}

#[tokio::test]
async fn slow_half_open_probe_should_reopen_after_the_hard_connect_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = WebSocketOriginBreakerConfig::default();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        reject_websocket_openings(&listener, config.failure_threshold).await;
        let (mut opening, _) = listener.accept().await.unwrap();
        assert!(
            read_http_request(&mut opening)
                .await
                .starts_with("GET /codex/responses")
        );
        let stalled = tokio::spawn(async move {
            let mut byte = [0];
            assert_eq!(opening.read(&mut byte).await.unwrap(), 0);
            closed_tx.send(()).unwrap();
        });
        for _ in 0..3 {
            let (mut http, _) = listener.accept().await.unwrap();
            assert!(
                read_http_request(&mut http)
                    .await
                    .starts_with("POST /codex/responses")
            );
            write_completed_sse_response(&mut http).await;
        }
        stalled.await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    record_websocket_origin_failures(&backend, config.failure_threshold, "hard-timeout").await;
    tokio::time::pause();
    tokio::time::advance(config.open_duration).await;
    tokio::time::resume();

    let fallback = backend
        .create_response(
            &new_chain_request("hard-timeout-probe"),
            request_context("req_hard_probe", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    assert_eq!(
        fallback.transport_metrics.decision,
        Some(CodexTransportDecision::Http2WebSocketBudgetExhausted)
    );
    let busy = backend
        .create_response(
            &new_chain_request("while-hard-probe-pending"),
            request_context("req_hard_busy", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    assert_eq!(
        busy.transport_metrics.decision,
        Some(CodexTransportDecision::Http2BreakerOpen)
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;
    tokio::time::resume();
    timeout(Duration::from_secs(1), closed_rx)
        .await
        .expect("15s hard timeout must close the opening socket")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let suppressed = backend
        .create_response(
            &new_chain_request("after-hard-probe-failure"),
            request_context("req_after_hard_failure", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    assert_eq!(
        suppressed.transport_metrics.decision,
        Some(CodexTransportDecision::Http2BreakerOpen)
    );
    server.await.unwrap();
    pool.shutdown().await;
}

#[tokio::test]
async fn cancelled_half_open_opening_should_allow_another_probe() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let breaker_config = WebSocketOriginBreakerConfig::default();

    let (opening_started_tx, opening_started_rx) = tokio::sync::oneshot::channel();
    let (opening_closed_tx, opening_closed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        reject_websocket_openings(&listener, breaker_config.failure_threshold).await;

        let (mut opening, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut opening).await;
        assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
        opening_started_tx.send(()).unwrap();

        let (mut http, _) = listener.accept().await.unwrap();
        assert!(
            read_http_request(&mut http)
                .await
                .starts_with("POST /codex/responses")
        );
        write_completed_sse_response(&mut http).await;

        let mut byte = [0_u8; 1];
        let read = timeout(Duration::from_secs(1), opening.read(&mut byte))
            .await
            .expect("account eviction should close the half-open probe")
            .unwrap();
        assert_eq!(read, 0);
        opening_closed_tx.send(()).unwrap();

        let (fresh_stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(fresh_stream).await;
        let _payload = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_after_cancelled_probe", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    record_websocket_origin_failures(
        &backend,
        breaker_config.failure_threshold,
        "cancelled-half-open-seed",
    )
    .await;
    tokio::time::pause();
    tokio::time::advance(breaker_config.open_duration).await;
    tokio::time::resume();

    let probe_backend = backend.clone();
    let attempt = tokio::spawn(async move {
        probe_backend
            .create_response(
                &new_chain_request("conversation-cancelled-half-open"),
                request_context("req_cancelled_half_open", Some("chatgpt-account")),
            )
            .await
    });

    opening_started_rx.await.unwrap();
    let fallback = attempt
        .await
        .unwrap()
        .expect("slow probe should fall back to HTTP");
    assert_eq!(
        fallback.transport_metrics.decision,
        Some(CodexTransportDecision::Http2WebSocketBudgetExhausted)
    );
    pool.evict_account("chatgpt-account").await;
    opening_closed_rx.await.unwrap();

    let response = backend
        .create_response(
            &new_chain_request("conversation-next-half-open-probe"),
            request_context("req_next_half_open_probe", Some("chatgpt-account")),
        )
        .await
        .expect("cancelled half-open opening should allow another probe");
    server.await.unwrap();

    assert_eq!(response.transport, CodexBackendTransport::WebSocket);
    assert!(response.body.contains("resp_after_cancelled_probe"));
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
    let breaker_config = WebSocketOriginBreakerConfig::default();
    assert!(breaker_config.failure_threshold >= 3);
    let seed_failures = breaker_config.failure_threshold - 2;
    let (late_failure_tx, late_failure_rx) = tokio::sync::oneshot::channel();
    let (second_used_websocket_tx, second_used_websocket_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        reject_websocket_openings(&listener, seed_failures).await;

        let (mut delayed_opening, _) = listener.accept().await.unwrap();
        let opening = read_http_request(&mut delayed_opening).await;
        assert!(opening.starts_with("GET /codex/responses HTTP/1.1"));
        let delayed_failure = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
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
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    record_websocket_origin_failures(&backend, seed_failures, "late-failure-seed").await;

    let first = backend
        .create_response(
            &new_chain_request("conversation-late-failure-first"),
            request_context("req_late_failure_first", Some("chatgpt-account")),
        )
        .await
        .expect("first request should use HTTP after the opening budget");
    late_failure_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = backend
        .create_response(
            &new_chain_request("conversation-late-failure-second"),
            request_context("req_late_failure_second", Some("chatgpt-account")),
        )
        .await
        .expect("one degraded opening must not open the default breaker");
    let second_used_websocket = second_used_websocket_rx.await.unwrap();
    server.await.unwrap();

    assert_eq!(first.transport, CodexBackendTransport::HttpSse);
    assert!(second_used_websocket);
    assert_eq!(second.transport, CodexBackendTransport::WebSocket);
    assert!(second.body.contains("resp_after_late_failure"));
    pool.shutdown().await;
}

#[tokio::test]
async fn half_open_upstream_response_should_close_origin_breaker() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let breaker_config = WebSocketOriginBreakerConfig::default();

    let server = tokio::spawn(async move {
        reject_websocket_openings(&listener, breaker_config.failure_threshold).await;

        let (mut probe, _) = listener.accept().await.unwrap();
        let opening = read_http_request(&mut probe).await;
        assert!(opening.starts_with("GET /codex/responses HTTP/1.1"));
        probe
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();

        let (next_stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(next_stream).await;
        let _payload = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_after_half_open_success", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    record_websocket_origin_failures(
        &backend,
        breaker_config.failure_threshold,
        "half-open-response-seed",
    )
    .await;
    tokio::time::pause();
    tokio::time::advance(breaker_config.open_duration).await;
    tokio::time::resume();

    let error = backend
        .create_response(
            &new_chain_request("conversation-breaker-probe"),
            request_context("req_breaker_probe", Some("chatgpt-account")),
        )
        .await
        .expect_err("half-open account response should remain explicit");

    std::assert_matches!(
        error,
        CodexClientError::Upstream {
            status,
            transport: CodexBackendTransport::WebSocket,
            ..
        } if status == reqwest::StatusCode::UNAUTHORIZED
    );
    let response = backend
        .create_response(
            &new_chain_request("conversation-after-half-open-response"),
            request_context("req_after_half_open_response", Some("chatgpt-account")),
        )
        .await
        .expect("half-open upstream response should close the origin breaker");
    server.await.unwrap();

    assert_eq!(response.transport, CodexBackendTransport::WebSocket);
    assert!(response.body.contains("resp_after_half_open_success"));
    pool.shutdown().await;
}

#[tokio::test]
async fn open_fast_path_breaker_should_allow_required_warmup_and_hot_reuse() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let threshold = WebSocketOriginBreakerConfig::default().failure_threshold;
    let server = tokio::spawn(async move {
        reject_websocket_openings(&listener, threshold).await;
        let (mut http, _) = listener.accept().await.unwrap();
        assert!(
            read_http_request(&mut http)
                .await
                .starts_with("POST /codex/responses")
        );
        write_completed_sse_response(&mut http).await;
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        for id in ["resp_required_warmup", "resp_hot_after_breaker"] {
            websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(completed_websocket_response(id, 2, 1).into()))
                .await
                .unwrap();
        }
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    record_websocket_origin_failures(&backend, threshold, "required-warmup").await;
    let fallback = backend
        .create_response(
            &new_chain_request("ordinary-cold"),
            request_context("req_cold_suppressed", Some("chatgpt-account")),
        )
        .await
        .expect("open fast path selects HTTP");
    assert_eq!(fallback.transport, CodexBackendTransport::HttpSse);
    let warmup = backend
        .create_response(
            &explicit_websocket_warmup_request("required-warmup"),
            request_context("req_required_warmup", Some("chatgpt-account")),
        )
        .await
        .expect("required WS bypasses optional fast path breaker");
    assert_eq!(warmup.transport, CodexBackendTransport::WebSocket);
    let hot = backend
        .create_response(
            &new_chain_request("required-warmup"),
            request_context("req_hot_after_breaker", Some("chatgpt-account")),
        )
        .await
        .expect("hot socket remains reusable");
    assert_eq!(hot.websocket_pool_decision.unwrap().kind(), "reuse");
    server.await.unwrap();
    pool.shutdown().await;
}
