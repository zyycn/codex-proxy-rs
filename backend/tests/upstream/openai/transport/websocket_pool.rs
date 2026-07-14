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
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.set_prompt_cache_key(Some("conversation-pool".to_string()));

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

/// idle 连接被上游静默关闭后，后台 pump 会实时把它标记为 closed。
/// 复用前的零成本 `is_closed` 检查应直接丢弃它并新建连接，不经过
/// “发请求 → 等首帧超时 → stale-reuse 重试” 的长尾（无需任何 maintenance sweep）。
#[tokio::test]
async fn codex_backend_client_should_open_fresh_socket_when_idle_pooled_websocket_died_silently() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let (first_closed_tx, first_closed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        // 第一条连接：完成一次响应后由服务端主动关闭（模拟 idle 期间被上游/中间盒断开）。
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_silent_first", 3, 1).into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();
        let _ = first_websocket.next().await;
        first_closed_tx.send(()).unwrap();

        // 第二条连接：证明复用被跳过、直接新建。
        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                completed_websocket_response("resp_silent_second", 4, 1).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    // 无 maintenance、无主动 ping：完全依赖 pump 后台读取感知连接死亡。
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(None, None, None),
    ));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.set_prompt_cache_key(Some("conversation-pool-silent".to_string()));

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_silent_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    first_closed_rx
        .await
        .expect("client pump should acknowledge the upstream close");
    tokio::task::yield_now().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_silent_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should open a fresh socket");
    server.await.unwrap();

    assert!(first.body.contains("resp_silent_first"));
    assert!(second.body.contains("resp_silent_second"));
    // 死连接在 acquire 处被零成本识别 → 直接新建，而非 stale-reuse 重试。
    assert_eq!(first.websocket_pool_decision.unwrap().kind(), "new");
    assert_eq!(second.websocket_pool_decision.unwrap().kind(), "new");
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn codex_backend_client_stream_should_keep_fresh_socket_after_structural_activity() {
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
                    "type": "response.created",
                    "response": {
                        "id": "resp_first_token_fresh_stalled",
                        "object": "response"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "fresh delayed output"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_fresh_delayed", 3, 1).into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        initial_event_timeout: Some(Duration::from_millis(30)),
        ..websocket_pool_config_for_tests(None, None, None)
    }));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let request = pooled_websocket_request("conversation-structural-fresh");

    let response = backend
        .create_response_stream(
            &request,
            request_context("req_structural_fresh", Some("chatgpt-account")),
        )
        .await
        .expect("structural activity should keep the fresh websocket open");
    let decision = response.websocket_pool_decision;
    let mut stream = response.body;
    let mut body = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("delayed fresh stream chunk should be valid");
        body.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    server.await.unwrap();

    assert!(body.contains("resp_first_token_fresh_stalled"));
    assert!(body.contains("fresh delayed output"));
    assert!(body.contains("resp_fresh_delayed"));
    assert_eq!(decision.unwrap().kind(), "new");
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn codex_backend_client_stream_should_keep_reused_socket_after_structural_activity() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _seed_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_first_token_reuse_seed", 2, 1).into(),
            ))
            .await
            .unwrap();

        let _reused_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.in_progress",
                    "response": {
                        "id": "resp_first_token_reuse_stalled",
                        "object": "response"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "reused delayed output"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_reused_delayed", 3, 1).into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        initial_event_timeout: Some(Duration::from_millis(30)),
        ..websocket_pool_config_for_tests(None, None, None)
    }));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let request = pooled_websocket_request("conversation-structural-reuse");

    let seed = backend
        .create_response(
            &request,
            request_context("req_structural_reuse_seed", Some("chatgpt-account")),
        )
        .await
        .expect("seed response should populate pool");
    tokio::time::pause();
    let response = backend
        .create_response_stream(
            &request,
            request_context("req_structural_reuse", Some("chatgpt-account")),
        )
        .await
        .expect("structural activity should keep the reused websocket open");
    let decision = response.websocket_pool_decision;
    let mut stream = response.body;
    let mut body = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("delayed reused stream chunk should be valid");
        body.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    server.await.unwrap();

    assert!(seed.body.contains("resp_first_token_reuse_seed"));
    assert!(body.contains("resp_first_token_reuse_stalled"));
    assert!(body.contains("reused delayed output"));
    assert!(body.contains("resp_reused_delayed"));
    assert_eq!(decision.unwrap().kind(), "reuse");
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn codex_backend_client_stream_should_keep_bypass_socket_after_structural_activity() {
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
                    "type": "response.created",
                    "response": {
                        "id": "resp_bypass_first_token_stalled",
                        "object": "response"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "bypass delayed output"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_bypass_delayed", 3, 1).into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        max_per_account: 0,
        initial_event_timeout: Some(Duration::from_millis(30)),
        ..websocket_pool_config_for_tests(None, None, None)
    }));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let request = pooled_websocket_request("conversation-structural-bypass");

    let response = backend
        .create_response_stream(
            &request,
            request_context("req_structural_bypass", Some("chatgpt-account")),
        )
        .await
        .expect("structural activity should keep the bypass websocket open");
    let decision = response.websocket_pool_decision;
    let mut stream = response.body;
    let mut body = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("delayed bypass stream chunk should be valid");
        body.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    server.await.unwrap();

    assert!(body.contains("resp_bypass_first_token_stalled"));
    assert!(body.contains("bypass delayed output"));
    assert!(body.contains("resp_bypass_delayed"));
    let decision = decision.expect("bypass decision");
    assert_eq!(decision.kind(), "bypass");
    assert_eq!(decision.reason(), Some("cap"));
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
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.set_prompt_cache_key(Some("conversation-pool".to_string()));

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
        crate::support::fingerprint::runtime_test_fingerprint(),
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

#[tokio::test(start_paused = true)]
async fn websocket_pool_should_release_slot_when_client_drops_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        // 第一个连接：发一帧后保持沉默（模拟上游不再发帧、也不发 terminal）。
        // slot 只能靠客户端断开来释放，隔离验证 tx.closed() 机制。
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "streaming has begun"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        // 第二个连接：客户端断开释放 slot 后，同 key 请求应新建连接。
        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                completed_websocket_response("resp_released_second", 2, 1).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
        drop(first_websocket);
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(pool);
    let request = pooled_websocket_request("conversation-drop");

    // 起流式请求：slot 变 Busy。
    let mut stream = backend
        .create_response_stream(
            &request,
            request_context("req_drop_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket stream should start")
        .body;
    let first_chunk = stream
        .next()
        .await
        .expect("stream should yield an initial chunk")
        .expect("stream chunk should be valid");
    assert!(
        std::str::from_utf8(&first_chunk)
            .unwrap()
            .contains("streaming has begun")
    );

    // 客户端断开：drop stream → rx 被 drop → tx.closed() 完成 →
    // 代理丢弃上游连接并释放 slot（不再等 idle 超时）。
    drop(stream);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 同 key 的后续请求：slot 已释放 → 新建连接（new），而非 bypass(busy)。
    let second = backend
        .create_response(
            &request,
            request_context("req_drop_second", Some("chatgpt-account")),
        )
        .await
        .expect("second request should succeed after slot release");
    server.await.unwrap();

    assert!(second.body.contains("resp_released_second"));
    let decision = second.websocket_pool_decision.unwrap();
    assert_eq!(
        decision.kind(),
        "new",
        "client-drop must release the pool slot so the next same-key request builds a fresh connection instead of bypassing as busy"
    );
}

#[tokio::test]
async fn websocket_pool_should_bypass_new_keys_after_account_cap() {
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
                completed_websocket_response("resp_cap_first", 2, 1).into(),
            ))
            .await
            .unwrap();

        for response_id in ["resp_cap_second", "resp_cap_third"] {
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
            websocket.close(None).await.unwrap();
        }
        drop(first_websocket);
    });
    let pool = Arc::new(CodexWebSocketPool::new(1, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
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
async fn websocket_pool_should_evict_idle_connection_when_liveness_lapses_despite_pings() {
    // 不变量：pump 自己发出的 keepalive ping 不算“入站活动”，因此一个只收 ping、
    // 从不回 pong / 从不发帧的静默连接，仍会被 liveness watchdog 判定失活并驱逐；
    // 复用前 acquire 读到 is_closed 即开新连接。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let first_connection = tokio::spawn(async move {
            let mut first_websocket = accept_codex_test_websocket(first_stream).await;
            let _first_message = first_websocket.next().await.unwrap().unwrap();
            first_websocket
                .send(Message::Text(
                    completed_websocket_response("resp_no_pong_first", 2, 1).into(),
                ))
                .await
                .unwrap();
            let _ = release_first_rx.await;
        });

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
        first_connection.await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        ping_interval: Some(Duration::from_millis(1)),
        liveness_timeout: Some(Duration::from_millis(20)),
        maintenance_interval: None,
        ..websocket_pool_config_for_tests(None, None, None)
    }));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
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
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(25)).await;
    tokio::task::yield_now().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_no_pong_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should use a fresh connection");
    release_first_tx.send(()).unwrap();
    server.await.unwrap();

    assert!(first.body.contains("resp_no_pong_first"));
    assert!(second.body.contains("resp_no_pong_second"));
    assert_eq!(second.websocket_pool_decision.unwrap().kind(), "new");
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
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
        std::assert_matches!(close, Message::Close(_));

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
        crate::support::fingerprint::runtime_test_fingerprint(),
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

#[tokio::test(start_paused = true)]
async fn codex_backend_client_should_keep_idle_pooled_websocket_alive_across_repeated_pings() {
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

        // pump 会在 idle 期间反复发送 keepalive ping；服务端计数并回 pong，
        // 直到下一个业务请求（response.create）到达为止。
        loop {
            let message = timeout(Duration::from_secs(1), websocket.next())
                .await
                .expect("pump keepalive / second request should arrive")
                .expect("frame should be present")
                .expect("frame should be valid");
            match message {
                Message::Ping(payload) => {
                    ping_count_for_server.fetch_add(1, Ordering::SeqCst);
                    websocket.send(Message::Pong(payload)).await.unwrap();
                }
                Message::Text(_) => break,
                other => panic!("unexpected frame while idle: {other:?}"),
            }
        }
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
        websocket_pool_config_for_tests(None, Some(Duration::from_millis(10)), None),
    ));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.set_prompt_cache_key(Some("conversation-pool-background".to_string()));

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_background_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    // 让 pump 有时间发出多轮 keepalive ping。
    tokio::time::sleep(Duration::from_millis(80)).await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_background_second", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should reuse the kept-alive socket");
    server.await.unwrap();
    pool.shutdown().await;

    assert!(first.body.contains("resp_pool_background_first"));
    assert!(second.body.contains("resp_pool_background_second"));
    assert!(ping_count.load(Ordering::SeqCst) >= 1);
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
        std::assert_matches!(close, Message::Close(_));

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
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.set_prompt_cache_key(Some("conversation-pool-evict".to_string()));

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
        std::assert_matches!(close, Message::Close(_));

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
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.set_prompt_cache_key(Some("conversation-pool-shutdown".to_string()));

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
    let (liveness_closed_tx, liveness_closed_rx) = tokio::sync::oneshot::channel();
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
        std::assert_matches!(close, Message::Close(_));
        liveness_closed_tx.send(()).unwrap();

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
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.set_prompt_cache_key(Some("conversation-pool-liveness".to_string()));

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_liveness_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    liveness_closed_rx
        .await
        .expect("liveness watchdog should close the idle connection");
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
async fn codex_backend_client_should_discard_pooled_websocket_after_error_terminal() {
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
                    "type": "error",
                    "error": {
                        "code": "rate_limit_exceeded",
                        "message": "Rate limit reached. Please try again in 1s."
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
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(pool);
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.set_prompt_cache_key(Some("conversation-pool".to_string()));

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_error", Some("chatgpt-account")),
        )
        .await
        .expect("error should be returned as a terminal SSE fact");
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_after_error", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should use a fresh connection");
    server.await.unwrap();

    assert!(first.body.contains("event: error"));
    assert!(first.body.contains("\"code\":\"rate_limit_exceeded\""));
    assert!(second.body.contains("resp_pool_after_error"));
    assert_eq!(second.websocket_pool_decision.unwrap().kind(), "new");
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_discard_pooled_websocket_after_unknown_response_failed() {
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

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_codex_test_websocket(second_stream).await;
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                completed_websocket_response("resp_pool_after_unknown_failed", 5, 2).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(pool);
    let request = pooled_websocket_request("conversation-pool-unknown-failed");

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_unknown_failed", Some("chatgpt-account")),
        )
        .await
        .expect("unknown response.failed should be returned as a terminal SSE fact");
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_after_unknown_failed", Some("chatgpt-account")),
        )
        .await
        .expect("failed websocket should be replaced");
    server.await.unwrap();

    assert!(first.body.contains("event: response.failed"));
    assert!(first.body.contains("resp_pool_model_refusal"));
    assert!(second.body.contains("resp_pool_after_unknown_failed"));
    assert_eq!(second.websocket_pool_decision.unwrap().kind(), "new");
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}
