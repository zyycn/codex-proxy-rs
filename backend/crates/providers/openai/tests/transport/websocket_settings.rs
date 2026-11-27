//! 验证请求配置切换时的 WS 寿命、超时与共享建连准入

use super::*;
use gateway_core::settings::OpenAiWebSocketPoolSettings;

#[tokio::test]
async fn connection_lifetime_survives_request_settings_changes() {
    for change_max_age in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            for index in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                tasks.spawn(async move {
                    let mut websocket = accept_codex_test_websocket(stream).await;
                    while let Some(Ok(message)) = websocket.next().await {
                        match message {
                            Message::Text(_) => websocket
                                .send(Message::Text(
                                    completed_websocket_response(
                                        &format!("resp_lifetime_{index}"),
                                        1,
                                        1,
                                    )
                                    .into(),
                                ))
                                .await
                                .unwrap(),
                            Message::Close(_) => {
                                let _ = websocket.flush().await;
                                break;
                            }
                            Message::Ping(_) => websocket.flush().await.unwrap(),
                            _ => {}
                        }
                    }
                });
            }
            while let Some(result) = tasks.join_next().await {
                result.unwrap();
            }
        });
        let pool = CodexWebSocketPool::new(Duration::from_secs(60));
        let backend = CodexBackendClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            format!("http://{addr}"),
            test_wire_profile(),
        );
        let long = OpenAiWebSocketPoolSettings::default();
        let mut short = long;
        if change_max_age {
            short.max_age_ms = 20;
        } else {
            short.idle_timeout_ms = 20;
        }
        let old = backend
            .clone()
            .with_websocket_pool(Arc::new(pool.with_settings(long)));
        let new = backend.with_websocket_pool(Arc::new(pool.with_settings(short)));
        let old_request = pooled_websocket_request("old-lifetime");
        let new_request = pooled_websocket_request("new-lifetime");
        old.create_response(&old_request, request_context("old_first", Some("account")))
            .await
            .unwrap();
        new.create_response(&new_request, request_context("new_first", Some("account")))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        // 维护任务不依赖最近一次请求的配置，按每条连接自身期限回收
        pool.maintain_idle_connections().await;
        let reused = new
            .create_response(&old_request, request_context("old_next", Some("account")))
            .await
            .unwrap();
        assert!(reused.websocket_pool_decision.unwrap().is_reuse());
        let replaced = old
            .create_response(&new_request, request_context("new_next", Some("account")))
            .await
            .unwrap();
        assert!(!replaced.websocket_pool_decision.unwrap().is_reuse());
        pool.shutdown().await;
        timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn request_snapshots_share_opening_count_when_limit_changes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (accepted, first_accepted) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (first, _) = listener.accept().await.unwrap();
        accepted.send(()).unwrap();
        // 保留首条 opening，较低上限的新请求只能走 HTTP
        let (mut fallback, _) = listener.accept().await.unwrap();
        assert!(
            read_http_request_with_body(&mut fallback)
                .await
                .starts_with(b"POST ")
        );
        write_completed_sse_response(&mut fallback).await;
        let (third, _) = listener.accept().await.unwrap();
        for stream in [first, third] {
            let mut websocket = accept_codex_test_websocket(stream).await;
            websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response("resp_opened", 1, 1).into(),
                ))
                .await
                .unwrap();
        }
        // opening 结束后归还计数，原来的低上限也可以再次建连
        let (fourth, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(fourth).await;
        websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_released", 1, 1).into(),
            ))
            .await
            .unwrap();
    });
    let pool = CodexWebSocketPool::new(Duration::from_secs(60));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    );
    let client = |limit| {
        backend
            .clone()
            .with_websocket_pool(Arc::new(pool.with_settings(OpenAiWebSocketPoolSettings {
                max_connecting: limit,
                fast_path_budget_ms: 15_000,
                ..Default::default()
            })))
    };
    let first = client(2);
    let first = tokio::spawn(async move {
        first
            .create_response(
                &pooled_websocket_request("opening-one"),
                request_context("one", Some("account")),
            )
            .await
            .unwrap()
    });
    first_accepted.await.unwrap();
    let lower = client(1);
    let fallback = lower
        .create_response(
            &pooled_websocket_request("opening-two"),
            request_context("two", Some("account")),
        )
        .await
        .unwrap();
    assert_eq!(fallback.transport, CodexBackendTransport::HttpSse);
    let third = client(2)
        .create_response(
            &pooled_websocket_request("opening-three"),
            request_context("three", Some("account")),
        )
        .await
        .unwrap();
    assert_eq!(third.transport, CodexBackendTransport::WebSocket);
    assert_eq!(
        first.await.unwrap().transport,
        CodexBackendTransport::WebSocket
    );
    let fourth = lower
        .create_response(
            &pooled_websocket_request("opening-four"),
            request_context("four", Some("account")),
        )
        .await
        .unwrap();
    assert_eq!(fourth.transport, CodexBackendTransport::WebSocket);
    server.await.unwrap();
    pool.shutdown().await;
}

#[tokio::test]
async fn stream_timeout_changes_on_the_next_turn_without_replacing_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (received, started) = tokio::sync::oneshot::channel();
    let (continue_response, finish) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        websocket.next().await.unwrap().unwrap();
        received.send(()).unwrap();
        finish.await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_old_timeout", 1, 1).into(),
            ))
            .await
            .unwrap();
        websocket.next().await.unwrap().unwrap();
        // 第二轮沿用同一物理连接，但使用新的短超时
        while let Some(Ok(message)) = websocket.next().await {
            if matches!(message, Message::Close(_)) {
                let _ = websocket.flush().await;
                break;
            }
        }
    });
    let pool = CodexWebSocketPool::new(Duration::from_secs(60));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    );
    let old = backend
        .clone()
        .with_websocket_pool(Arc::new(pool.with_settings(OpenAiWebSocketPoolSettings {
            stream_idle_timeout_ms: 1_000,
            ..Default::default()
        })));
    let first = tokio::spawn(async move {
        old.create_response(
            &pooled_websocket_request("timeout-turns"),
            request_context("old_timeout", Some("account")),
        )
        .await
    });
    started.await.unwrap();
    let new =
        backend.with_websocket_pool(Arc::new(pool.with_settings(OpenAiWebSocketPoolSettings {
            stream_idle_timeout_ms: 20,
            ..Default::default()
        })));
    continue_response.send(()).unwrap();
    assert!(first.await.unwrap().is_ok());
    let error = timeout(
        Duration::from_secs(2),
        new.create_response(
            &pooled_websocket_request("timeout-turns"),
            request_context("new_timeout", Some("account")),
        ),
    )
    .await
    .expect("new turn must use the short timeout")
    .unwrap_err();
    let CodexClientError::WebSocket(CodexWebSocketExchangeError::PostSendAmbiguous {
        source: Some(source),
        ..
    }) = error
    else {
        panic!("expected a failure after sending the second turn");
    };
    let CodexWebSocketExchangeError::ConnectionObserved { source, .. } = *source else {
        panic!("expected connection diagnostics");
    };
    assert!(
        matches!(*source, CodexWebSocketExchangeError::ReceiveIdleTimeout { timeout } if timeout == Duration::from_millis(20))
    );
    pool.shutdown().await;
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}
