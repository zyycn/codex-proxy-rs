use super::*;

#[tokio::test]
async fn codex_backend_client_should_reuse_pooled_websocket_for_same_account_and_conversation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut websocket = accept_codex_test_websocket(stream).await;
        for response_id in ["resp_pool_first", "resp_pool_second"] {
            let _message = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": response_id,
                            "object": "response",
                            "output": [],
                            "usage": {
                                "input_tokens": 3,
                                "output_tokens": 1,
                                "total_tokens": 4
                            }
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }
        websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_second", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should succeed");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_first"));
    assert!(second.body.contains("resp_pool_second"));
    assert_eq!(first.websocket_pool_decision.unwrap().kind(), "new");
    assert_eq!(second.websocket_pool_decision.unwrap().kind(), "reuse");
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn codex_backend_client_should_not_reuse_pooled_websocket_across_local_accounts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        for response_id in ["resp_local_a", "resp_local_b"] {
            let (stream, _) = listener.accept().await.unwrap();
            accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
            let mut websocket = accept_codex_test_websocket(stream).await;
            let _message = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response(response_id, 3, 1).into(),
                ))
                .await
                .unwrap();
            websocket.close(None).await.unwrap();
        }
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool".to_string());

    let first = backend
        .create_response_with_pool_account_started_at(
            &request,
            request_context("req_pool_local_a", Some("same-chatgpt-account")),
            Some("acct_local_a"),
            std::time::Instant::now(),
        )
        .await
        .expect("first local account websocket response should succeed");
    let second = backend
        .create_response_with_pool_account_started_at(
            &request,
            request_context("req_pool_local_b", Some("same-chatgpt-account")),
            Some("acct_local_b"),
            std::time::Instant::now(),
        )
        .await
        .expect("second local account websocket response should succeed");
    server.await.unwrap();

    assert!(first.body.contains("resp_local_a"));
    assert!(second.body.contains("resp_local_b"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn websocket_pool_should_bypass_busy_key_with_one_shot_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "first connection is still busy"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                completed_websocket_response("resp_busy_second", 2, 1).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();

        let (third_stream, _) = listener.accept().await.unwrap();
        let mut third_websocket = accept_codex_test_websocket(third_stream).await;
        let _third_message = third_websocket.next().await.unwrap().unwrap();
        third_websocket
            .send(Message::Text(
                completed_websocket_response("resp_busy_third", 2, 1).into(),
            ))
            .await
            .unwrap();
        third_websocket.close(None).await.unwrap();

        release_first_rx.await.unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_busy_first", 2, 1).into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(pool);
    let request = pooled_websocket_request("conversation-busy");

    let mut first = backend
        .create_response_stream(
            &request,
            request_context("req_busy_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket stream should start")
        .body;
    let first_chunk = first
        .next()
        .await
        .expect("first stream should yield an initial chunk")
        .expect("first stream chunk should be valid");
    let first_chunk = std::str::from_utf8(&first_chunk).unwrap();
    assert!(first_chunk.contains("first connection is still busy"));

    let second = backend
        .create_response(
            &request,
            request_context("req_busy_second", Some("chatgpt-account")),
        )
        .await
        .expect("busy key should bypass with a one-shot second connection");
    let third = backend
        .create_response(
            &request,
            request_context("req_busy_third", Some("chatgpt-account")),
        )
        .await
        .expect("busy key should bypass with a one-shot third connection");

    release_first_tx.send(()).unwrap();
    while first.next().await.transpose().unwrap().is_some() {}
    server.await.unwrap();

    assert!(second.body.contains("resp_busy_second"));
    assert!(third.body.contains("resp_busy_third"));
    assert_eq!(second.websocket_pool_decision.unwrap().kind(), "bypass");
    assert_eq!(
        second.websocket_pool_decision.unwrap().reason(),
        Some("busy")
    );
    assert_eq!(third.websocket_pool_decision.unwrap().kind(), "bypass");
    assert_eq!(
        third.websocket_pool_decision.unwrap().reason(),
        Some("busy")
    );
}

#[tokio::test]
async fn websocket_pool_should_bypass_new_keys_after_account_cap() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        for response_id in ["resp_cap_first", "resp_cap_second", "resp_cap_third"] {
            let (stream, _) = listener.accept().await.unwrap();
            accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
            let mut websocket = accept_codex_test_websocket(stream).await;
            let _message = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response(response_id, 2, 1).into(),
                ))
                .await
                .unwrap();
            if response_id == "resp_cap_third" {
                websocket.close(None).await.unwrap();
            }
        }
    });
    let pool = Arc::new(CodexWebSocketPool::new(1, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(pool);
    let first_request = pooled_websocket_request("conversation-cap-one");
    let second_request = pooled_websocket_request("conversation-cap-two");

    let first = backend
        .create_response(
            &first_request,
            request_context("req_cap_first", Some("chatgpt-account")),
        )
        .await
        .expect("first capped websocket response should succeed");
    let second = backend
        .create_response(
            &second_request,
            request_context("req_cap_second", Some("chatgpt-account")),
        )
        .await
        .expect("new key over account cap should use one-shot connection");
    let third = backend
        .create_response(
            &second_request,
            request_context("req_cap_third", Some("chatgpt-account")),
        )
        .await
        .expect("capped key should keep bypassing instead of entering the pool");
    server.await.unwrap();

    assert!(first.body.contains("resp_cap_first"));
    assert!(second.body.contains("resp_cap_second"));
    assert!(third.body.contains("resp_cap_third"));
    assert_eq!(first.websocket_pool_decision.unwrap().kind(), "new");
    assert_eq!(second.websocket_pool_decision.unwrap().kind(), "bypass");
    assert_eq!(
        second.websocket_pool_decision.unwrap().reason(),
        Some("cap")
    );
    assert_eq!(third.websocket_pool_decision.unwrap().kind(), "bypass");
    assert_eq!(third.websocket_pool_decision.unwrap().reason(), Some("cap"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn codex_backend_client_should_ping_idle_pooled_websocket_during_maintenance() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ping_count = Arc::new(AtomicUsize::new(0));
    let ping_count_for_server = Arc::clone(&ping_count);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _first_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_keepalive_first",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 1,
                            "total_tokens": 4
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        let ping = timeout(Duration::from_secs(1), websocket.next())
            .await
            .expect("pool maintenance should probe the idle websocket")
            .expect("pool maintenance should send a websocket frame")
            .expect("pool maintenance frame should be valid");
        let Message::Ping(payload) = ping else {
            panic!("expected pool maintenance ping frame, got {ping:?}");
        };
        ping_count_for_server.fetch_add(1, Ordering::SeqCst);
        websocket.send(Message::Pong(payload)).await.unwrap();

        let _second_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_keepalive_second",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 1,
                            "total_tokens": 4
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
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(None, Some(Duration::from_millis(1)), None),
    ));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-keepalive".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_keepalive_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    pool.maintain_idle_connections().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_keepalive_second", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should reuse the probed socket");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_keepalive_first"));
    assert!(second.body.contains("resp_pool_keepalive_second"));
    assert_eq!(ping_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn websocket_pool_should_evict_idle_connection_when_ping_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_no_pong_first", 2, 1).into(),
            ))
            .await
            .unwrap();
        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                completed_websocket_response("resp_no_pong_second", 2, 1).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
        drop(first_websocket);
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        ping_interval: Some(Duration::from_millis(1)),
        ping_timeout: Duration::from_millis(20),
        maintenance_interval: None,
        ..websocket_pool_config_for_tests(None, None, None)
    }));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let request = pooled_websocket_request("conversation-no-pong");

    let first = backend
        .create_response(
            &request,
            request_context("req_no_pong_first", Some("chatgpt-account")),
        )
        .await
        .expect("first websocket response should succeed");
    pool.maintain_idle_connections().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_no_pong_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should use a fresh connection");
    server.await.unwrap();

    assert!(first.body.contains("resp_no_pong_first"));
    assert!(second.body.contains("resp_no_pong_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn websocket_pool_should_gc_expired_idle_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_gc_first", 2, 1).into(),
            ))
            .await
            .unwrap();
        let close = timeout(Duration::from_secs(1), first_websocket.next())
            .await
            .expect("gc sweep should close the expired idle websocket")
            .expect("gc sweep should send a close frame")
            .expect("close frame should be valid");
        assert!(matches!(close, Message::Close(_)));

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                completed_websocket_response("resp_gc_second", 2, 1).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        max_age: Duration::from_millis(5),
        maintenance_interval: None,
        ping_interval: None,
        liveness_timeout: None,
        ..CodexWebSocketPoolConfig::default()
    }));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let request = pooled_websocket_request("conversation-gc");

    let first = backend
        .create_response(
            &request,
            request_context("req_gc_first", Some("chatgpt-account")),
        )
        .await
        .expect("first websocket response should succeed");
    tokio::time::sleep(Duration::from_millis(15)).await;
    pool.maintain_idle_connections().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_gc_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should use a fresh connection after gc");
    server.await.unwrap();

    assert!(first.body.contains("resp_gc_first"));
    assert!(second.body.contains("resp_gc_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_ping_idle_pooled_websocket_from_background_maintenance() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ping_count = Arc::new(AtomicUsize::new(0));
    let ping_count_for_server = Arc::clone(&ping_count);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _first_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_background_first",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 1,
                            "total_tokens": 4
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        let ping = timeout(Duration::from_secs(1), websocket.next())
            .await
            .expect("background maintenance should probe the idle websocket")
            .expect("background maintenance should send a websocket frame")
            .expect("background maintenance frame should be valid");
        let Message::Ping(payload) = ping else {
            panic!("expected background maintenance ping frame, got {ping:?}");
        };
        ping_count_for_server.fetch_add(1, Ordering::SeqCst);
        websocket.send(Message::Pong(payload)).await.unwrap();

        let _second_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_background_second",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 1,
                            "total_tokens": 4
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
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(
            Some(Duration::from_millis(20)),
            Some(Duration::from_mins(1)),
            None,
        ),
    ));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-background".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_background_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    tokio::time::sleep(Duration::from_millis(80)).await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_background_second", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should reuse the background-probed socket");
    server.await.unwrap();
    pool.shutdown().await;

    assert!(first.body.contains("resp_pool_background_first"));
    assert!(second.body.contains("resp_pool_background_second"));
    assert_eq!(ping_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn codex_backend_client_should_close_idle_pooled_websocket_when_account_is_evicted() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_evict_first",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 1,
                            "total_tokens": 4
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let close = timeout(Duration::from_secs(1), first_websocket.next())
            .await
            .expect("evict_account should close the idle websocket")
            .expect("evict_account should send a close frame")
            .expect("close frame should be valid");
        assert!(matches!(close, Message::Close(_)));

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_evict_second",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 4,
                            "output_tokens": 1,
                            "total_tokens": 5
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(None, None, None),
    ));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-evict".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_evict_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    pool.evict_account("chatgpt-account").await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_evict_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should open a fresh socket after eviction");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_evict_first"));
    assert!(second.body.contains("resp_pool_evict_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_stop_reusing_pooled_websockets_after_shutdown() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_shutdown_first",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 1,
                            "total_tokens": 4
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let close = timeout(Duration::from_secs(1), first_websocket.next())
            .await
            .expect("shutdown should close the idle websocket")
            .expect("shutdown should send a close frame")
            .expect("close frame should be valid");
        assert!(matches!(close, Message::Close(_)));

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_shutdown_second",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 4,
                            "output_tokens": 1,
                            "total_tokens": 5
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(None, None, None),
    ));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-shutdown".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_shutdown_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    pool.shutdown().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_shutdown_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should bypass the shut down pool");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_shutdown_first"));
    assert!(second.body.contains("resp_pool_shutdown_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_close_idle_pooled_websocket_after_liveness_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_liveness_first",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 1,
                            "total_tokens": 4
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let close = timeout(Duration::from_secs(1), first_websocket.next())
            .await
            .expect("liveness timeout should close the idle websocket")
            .expect("liveness timeout should send a close frame")
            .expect("close frame should be valid");
        assert!(matches!(close, Message::Close(_)));

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_liveness_second",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 4,
                            "output_tokens": 1,
                            "total_tokens": 5
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(None, None, Some(Duration::from_millis(1))),
    ));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-liveness".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_liveness_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    tokio::time::sleep(Duration::from_millis(10)).await;
    pool.maintain_idle_connections().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_liveness_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should open a fresh socket after liveness close");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_liveness_first"));
    assert!(second.body.contains("resp_pool_liveness_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_discard_pooled_websocket_after_upstream_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_pool_rate_limit",
                        "error": {
                            "code": "rate_limit_exceeded",
                            "message": "Rate limit reached. Please try again in 1s."
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_after_error",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 5,
                            "output_tokens": 2,
                            "total_tokens": 7
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(pool);
    let mut request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool".to_string());

    let first_error = backend
        .create_response(
            &request,
            request_context("req_pool_error", Some("chatgpt-account")),
        )
        .await
        .expect_err("first pooled websocket response should surface upstream error");
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_after_error", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should use a fresh connection");
    server.await.unwrap();

    let CodexClientError::Upstream { status, body, .. } = first_error else {
        panic!("expected upstream error from first pooled websocket response");
    };
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert!(body.contains("rate_limit_exceeded"));
    assert!(second.body.contains("resp_pool_after_error"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_reuse_pooled_websocket_after_unmapped_response_failed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _first_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_pool_model_refusal",
                        "status": "failed",
                        "error": {
                            "code": "model_refusal",
                            "message": "The model refused the request"
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        let _second_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_pool_after_unmapped_failed", 5, 2).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::test_fingerprint(),
    )
    .with_websocket_pool(pool);
    let request = pooled_websocket_request("conversation-pool-unmapped-failed");

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_unmapped_failed", Some("chatgpt-account")),
        )
        .await
        .expect("unmapped response.failed should be returned as terminal SSE");
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_after_unmapped_failed", Some("chatgpt-account")),
        )
        .await
        .expect("terminal failed websocket should be reusable");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_model_refusal"));
    assert!(second.body.contains("resp_pool_after_unmapped_failed"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 1);
}
