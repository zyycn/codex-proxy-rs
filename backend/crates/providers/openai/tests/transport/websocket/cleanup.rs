//! 验证上游停止读请求时，发送超时仍能返回并释放连接

use super::*;
use std::assert_matches;

#[tokio::test]
async fn send_timeout_should_abort_stalled_pump_and_allow_next_request() {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(8192).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = socket.listen(2).unwrap();
    let addr = listener.local_addr().unwrap();
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let (drain, drain_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        // 不协商压缩，保证大请求填满 TCP 发送缓冲，而不是压缩后一次写完
        let mut websocket = accept_codex_test_websocket_with(stream, |_, response| {
            response.headers_mut().remove("sec-websocket-extensions");
        })
        .await;
        websocket.get_mut().read_u8().await.unwrap();
        started.send(()).unwrap();
        drain_rx.await.unwrap();
        // 只排空已进入内核的字节；必须看到 EOF，证明客户端已释放物理 socket
        tokio::io::copy(websocket.get_mut(), &mut tokio::io::sink())
            .await
            .unwrap();

        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        assert_matches!(websocket.next().await.unwrap().unwrap(), Message::Text(_));
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_after_send_timeout", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        maintenance_interval: None,
        ..CodexWebSocketPoolConfig::default()
    }));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request = codex_request_with_prompt_cache_key(
        "gpt-test",
        "be brief",
        vec![json!({"role": "user", "content": "x".repeat(16 * 1024 * 1024)})],
        "stalled-send",
    );
    request.set_previous_response_id(Some("resp_previous".to_owned()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.local_conversation_id = Some("stalled-send".to_owned());
    let error = {
        let pending = backend.create_response_stream(
            &request,
            request_context("stalled_send", Some("chatgpt-account")),
        );
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("send should stall: {}", result.is_ok()),
            ready = started_rx => ready.unwrap(),
            () = tokio::time::sleep(Duration::from_secs(5)) => panic!("payload send should start"),
        }
        // 握手和首次写入用真实时间，随后仅快进发送 deadline，避免等待五分钟
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(301)).await;
        let result = timeout(Duration::from_secs(1), &mut pending).await;
        tokio::time::resume();
        result
            .expect("send timeout must not wait for Close on the stalled pump")
            .err()
            .expect("stalled send should fail")
    };
    let CodexClientError::WebSocket(error) = error else {
        panic!("expected WebSocket send failure: {error:?}");
    };
    let CodexWebSocketExchangeError::ConnectionObserved { source, .. } = error else {
        panic!("send timeout must retain its connection observation: {error:?}");
    };
    let CodexWebSocketExchangeError::PostSendAmbiguous {
        source: Some(source),
        ..
    } = source.as_ref()
    else {
        panic!("send timeout must retain its post-send ambiguity: {source:?}");
    };
    assert_matches!(
        source.as_ref(),
        CodexWebSocketExchangeError::SendTimeout { .. }
    );
    drain.send(()).unwrap();
    let response = timeout(
        Duration::from_secs(5),
        backend.create_response(
            &pooled_websocket_request("stalled-send"),
            request_context("after_stalled_send", Some("chatgpt-account")),
        ),
    )
    .await
    .expect("failed request must release the pool reservation and socket")
    .unwrap();
    assert!(response.body.contains("resp_after_send_timeout"));
    server.await.unwrap();
    pool.shutdown().await;
}
