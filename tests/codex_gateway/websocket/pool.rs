use super::*;

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_pool_should_bypass_busy_key_with_one_shot_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_first_tx, release_first_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_ws =
            accept_hdr_async(first_stream, |_request: &WsRequest, response| Ok(response))
                .await
                .unwrap();
        let _first_request = first_ws.next().await.unwrap().unwrap();
        first_ws
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
        let mut second_ws =
            accept_hdr_async(second_stream, |_request: &WsRequest, response| Ok(response))
                .await
                .unwrap();
        let _second_request = second_ws.next().await.unwrap().unwrap();
        second_ws
            .send(Message::Text(
                websocket_completed_response("resp_busy_second", 2, 1).into(),
            ))
            .await
            .unwrap();

        let third_on_new_connection = tokio::select! {
            message = second_ws.next() => {
                if matches!(message, Some(Ok(message)) if message.is_text()) {
                    false
                } else {
                    let (third_stream, _) = listener.accept().await.unwrap();
                    let mut third_ws = accept_hdr_async(
                        third_stream,
                        |_request: &WsRequest, response| Ok(response),
                    )
                    .await
                    .unwrap();
                    let _third_request = third_ws.next().await.unwrap().unwrap();
                    third_ws
                        .send(Message::Text(
                            websocket_completed_response("resp_busy_third", 2, 1).into(),
                        ))
                        .await
                        .unwrap();
                    third_ws.close(None).await.unwrap();
                    true
                }
            }
            accepted = listener.accept() => {
                let (third_stream, _) = accepted.unwrap();
                let mut third_ws = accept_hdr_async(
                    third_stream,
                    |_request: &WsRequest, response| Ok(response),
                )
                .await
                .unwrap();
                let _third_request = third_ws.next().await.unwrap().unwrap();
                third_ws
                    .send(Message::Text(
                        websocket_completed_response("resp_busy_third", 2, 1).into(),
                    ))
                    .await
                    .unwrap();
                third_ws.close(None).await.unwrap();
                true
            }
        };

        release_first_rx.await.unwrap();
        first_ws
            .send(Message::Text(
                websocket_completed_response("resp_busy_first", 2, 1).into(),
            ))
            .await
            .unwrap();
        first_ws.close(None).await.unwrap();
        third_on_new_connection
    });

    let pool = Arc::new(CodexWebSocketPool::with_default_max_age());
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(pool, "entry-a");
    let mut request = base_request();
    request.prompt_cache_key = Some("conversation-a".to_string());

    let mut first_stream = client
        .websocket_stream_response(&request, request_context("req_busy_first", Some("conv-a")))
        .await
        .unwrap()
        .body_stream;
    let first_chunk = first_stream.next().await.unwrap().unwrap();
    assert!(first_chunk.contains("first connection is still busy"));

    let second = client
        .create_response(&request, request_context("req_busy_second", Some("conv-a")))
        .await
        .unwrap();
    assert!(second.body.contains("\"id\":\"resp_busy_second\""));

    let third = client
        .create_response(&request, request_context("req_busy_third", Some("conv-a")))
        .await
        .unwrap();
    assert!(third.body.contains("\"id\":\"resp_busy_third\""));

    release_first_tx.send(()).unwrap();
    while first_stream.next().await.transpose().unwrap().is_some() {}

    assert!(
        server.await.unwrap(),
        "busy pool key reused a second pooled websocket instead of one-shot bypass"
    );
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_pool_should_bypass_new_keys_after_account_cap() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_ws =
            accept_hdr_async(first_stream, |_request: &WsRequest, response| Ok(response))
                .await
                .unwrap();
        let _first_request = first_ws.next().await.unwrap().unwrap();
        first_ws
            .send(Message::Text(
                websocket_completed_response("resp_cap_first", 2, 1).into(),
            ))
            .await
            .unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second_ws =
            accept_hdr_async(second_stream, |_request: &WsRequest, response| Ok(response))
                .await
                .unwrap();
        let _second_request = second_ws.next().await.unwrap().unwrap();
        second_ws
            .send(Message::Text(
                websocket_completed_response("resp_cap_second", 2, 1).into(),
            ))
            .await
            .unwrap();

        let third_on_new_connection = tokio::select! {
            message = second_ws.next() => {
                if matches!(message, Some(Ok(message)) if message.is_text()) {
                    false
                } else {
                    let (third_stream, _) = listener.accept().await.unwrap();
                    let mut third_ws = accept_hdr_async(
                        third_stream,
                        |_request: &WsRequest, response| Ok(response),
                    )
                    .await
                    .unwrap();
                    let _third_request = third_ws.next().await.unwrap().unwrap();
                    third_ws
                        .send(Message::Text(
                            websocket_completed_response("resp_cap_third", 2, 1).into(),
                        ))
                        .await
                        .unwrap();
                    third_ws.close(None).await.unwrap();
                    true
                }
            }
            accepted = listener.accept() => {
                let (third_stream, _) = accepted.unwrap();
                let mut third_ws = accept_hdr_async(
                    third_stream,
                    |_request: &WsRequest, response| Ok(response),
                )
                .await
                .unwrap();
                let _third_request = third_ws.next().await.unwrap().unwrap();
                third_ws
                    .send(Message::Text(
                        websocket_completed_response("resp_cap_third", 2, 1).into(),
                    ))
                    .await
                    .unwrap();
                third_ws.close(None).await.unwrap();
                true
            }
        };

        first_ws.close(None).await.unwrap();
        third_on_new_connection
    });

    let pool = Arc::new(CodexWebSocketPool::with_limits(Duration::from_secs(60), 1));
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(pool, "entry-cap");

    let mut first_request = base_request();
    first_request.prompt_cache_key = Some("conversation-one".to_string());
    let first = client
        .create_response(
            &first_request,
            request_context("req_cap_first", Some("conv-one")),
        )
        .await
        .unwrap();
    assert!(first.body.contains("\"id\":\"resp_cap_first\""));

    let mut second_request = base_request();
    second_request.prompt_cache_key = Some("conversation-two".to_string());
    let second = client
        .create_response(
            &second_request,
            request_context("req_cap_second", Some("conv-two")),
        )
        .await
        .unwrap();
    assert!(second.body.contains("\"id\":\"resp_cap_second\""));

    let third = client
        .create_response(
            &second_request,
            request_context("req_cap_third", Some("conv-two")),
        )
        .await
        .unwrap();
    assert!(third.body.contains("\"id\":\"resp_cap_third\""));

    assert!(
        server.await.unwrap(),
        "account cap bypass still allowed a capped conversation to be pooled"
    );
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_pool_keepalive_should_keep_idle_connection_when_pong_is_received() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_hdr_async(stream, |_request: &WsRequest, response| Ok(response))
            .await
            .unwrap();
        let _first_request = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_keepalive_first", 2, 1).into(),
            ))
            .await
            .unwrap();

        let ping = websocket.next().await.unwrap().unwrap();
        assert!(
            ping.is_ping(),
            "expected pool maintenance ping, got {ping:?}"
        );
        websocket
            .send(Message::Pong(Vec::new().into()))
            .await
            .unwrap();

        let reused = tokio::select! {
            message = websocket.next() => {
                let message = message.unwrap().unwrap();
                if !message.is_text() {
                    return false;
                }
                websocket
                    .send(Message::Text(
                        websocket_completed_response("resp_keepalive_second", 2, 1).into(),
                    ))
                    .await
                    .unwrap();
                true
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                let mut websocket = accept_hdr_async(
                    stream,
                    |_request: &WsRequest, response| Ok(response),
                )
                .await
                .unwrap();
                let _request = websocket.next().await.unwrap().unwrap();
                websocket
                    .send(Message::Text(
                        websocket_completed_response("resp_keepalive_second", 2, 1).into(),
                    ))
                    .await
                    .unwrap();
                false
            }
        };

        websocket.close(None).await.unwrap();
        reused
    });

    let pool = Arc::new(CodexWebSocketPool::with_config(keepalive_pool_config(
        Duration::from_millis(50),
    )));
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool), "entry-keepalive");
    let mut request = base_request();
    request.prompt_cache_key = Some("conversation-keepalive".to_string());

    let first = client
        .create_response(
            &request,
            request_context("req_keepalive_first", Some("conv-keepalive")),
        )
        .await
        .unwrap();
    assert!(first.body.contains("\"id\":\"resp_keepalive_first\""));

    pool.maintain_idle_connections().await;

    let second = client
        .create_response(
            &request,
            request_context("req_keepalive_second", Some("conv-keepalive")),
        )
        .await
        .unwrap();
    assert!(second.body.contains("\"id\":\"resp_keepalive_second\""));

    assert!(
        server.await.unwrap(),
        "keepalive pong should leave the pooled websocket reusable"
    );
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_pool_keepalive_should_evict_idle_connection_without_pong() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_ws =
            accept_hdr_async(first_stream, |_request: &WsRequest, response| Ok(response))
                .await
                .unwrap();
        let _first_request = first_ws.next().await.unwrap().unwrap();
        first_ws
            .send(Message::Text(
                websocket_completed_response("resp_no_pong_first", 2, 1).into(),
            ))
            .await
            .unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second_ws =
            accept_hdr_async(second_stream, |_request: &WsRequest, response| Ok(response))
                .await
                .unwrap();
        let _second_request = second_ws.next().await.unwrap().unwrap();
        second_ws
            .send(Message::Text(
                websocket_completed_response("resp_no_pong_second", 2, 1).into(),
            ))
            .await
            .unwrap();
        second_ws.close(None).await.unwrap();
    });

    let pool = Arc::new(CodexWebSocketPool::with_config(keepalive_pool_config(
        Duration::from_millis(20),
    )));
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool), "entry-no-pong");
    let mut request = base_request();
    request.prompt_cache_key = Some("conversation-no-pong".to_string());

    let first = client
        .create_response(
            &request,
            request_context("req_no_pong_first", Some("conv-no-pong")),
        )
        .await
        .unwrap();
    assert!(first.body.contains("\"id\":\"resp_no_pong_first\""));

    pool.maintain_idle_connections().await;

    let second = client
        .create_response(
            &request,
            request_context("req_no_pong_second", Some("conv-no-pong")),
        )
        .await
        .unwrap();
    assert!(second.body.contains("\"id\":\"resp_no_pong_second\""));

    server.await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_pool_gc_sweep_should_close_expired_idle_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for response_id in ["resp_gc_first", "resp_gc_second"] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket =
                accept_hdr_async(stream, |_request: &WsRequest, response| Ok(response))
                    .await
                    .unwrap();
            let _request = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    websocket_completed_response(response_id, 2, 1).into(),
                ))
                .await
                .unwrap();
            websocket.close(None).await.unwrap();
        }
    });

    let pool = Arc::new(CodexWebSocketPool::with_config(manual_pool_config(
        Duration::from_millis(5),
        8,
    )));
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool), "entry-gc");
    let mut request = base_request();
    request.prompt_cache_key = Some("conversation-gc".to_string());

    let first = client
        .create_response(&request, request_context("req_gc_first", Some("conv-gc")))
        .await
        .unwrap();
    assert!(first.body.contains("\"id\":\"resp_gc_first\""));

    sleep(Duration::from_millis(15)).await;
    pool.gc_sweep().await;

    let second = client
        .create_response(&request, request_context("req_gc_second", Some("conv-gc")))
        .await
        .unwrap();
    assert!(second.body.contains("\"id\":\"resp_gc_second\""));

    server.await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_pool_shutdown_should_close_idle_connections_and_disable_pooling() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for response_id in ["resp_shutdown_first", "resp_shutdown_second"] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket =
                accept_hdr_async(stream, |_request: &WsRequest, response| Ok(response))
                    .await
                    .unwrap();
            let _request = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    websocket_completed_response(response_id, 2, 1).into(),
                ))
                .await
                .unwrap();
            websocket.close(None).await.unwrap();
        }
    });

    let pool = Arc::new(CodexWebSocketPool::with_config(manual_pool_config(
        Duration::from_secs(60),
        8,
    )));
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool), "entry-shutdown");
    let mut request = base_request();
    request.prompt_cache_key = Some("conversation-shutdown".to_string());

    let first = client
        .create_response(
            &request,
            request_context("req_shutdown_first", Some("conv-shutdown")),
        )
        .await
        .unwrap();
    assert!(first.body.contains("\"id\":\"resp_shutdown_first\""));

    pool.shutdown().await;

    let second = client
        .create_response(
            &request,
            request_context("req_shutdown_second", Some("conv-shutdown")),
        )
        .await
        .unwrap();
    assert!(second.body.contains("\"id\":\"resp_shutdown_second\""));

    server.await.unwrap();
}
