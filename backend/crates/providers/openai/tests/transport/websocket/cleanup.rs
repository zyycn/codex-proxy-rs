//! 验证上下游背压、取消与终态下的连接和任务回收

use super::*;
use gateway_core::{
    diagnostics::TraceContext, engine::response_control::ResponseControl,
    lifecycle::CancellationToken,
};
use std::assert_matches;

async fn wait_for_forwarded_frames(trace: &TraceContext, stage: &str, count: u64) {
    timeout(Duration::from_secs(2), async {
        while trace.snapshot().unwrap()["stageCounts"][stage]
            .as_u64()
            .unwrap_or_default()
            < count
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("forwarder should reach the backpressured frame");
}

fn response_tail(kind: &str) -> Message {
    match kind {
        "binary" => Message::Binary(vec![0].into()),
        "delta" => Message::Text(
            json!({"type":"response.output_text.delta", "delta":"x"})
                .to_string()
                .into(),
        ),
        "completed" => {
            Message::Text(completed_websocket_response("resp_backpressure", 2, 1).into())
        }
        kind => Message::Text(
            json!({"type":format!("response.{kind}"), "response":{
                "id":"resp_backpressure", "status":kind, "output":[]
            }})
            .to_string()
            .into(),
        ),
    }
}

async fn send_backpressured_response(
    websocket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    tail: &str,
) {
    // 先填满交付队列，后续帧由诊断计数确认已到达 forwarder，不靠 sleep 猜测调度
    for _ in 0..16 {
        websocket.send(response_tail("delta")).await.unwrap();
    }
    websocket.send(response_tail(tail)).await.unwrap();
}

async fn expect_upstream_socket_closed(
    websocket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
) {
    let close = timeout(Duration::from_secs(2), websocket.next())
        .await
        .expect("cancellation must release the socket while the receiver remains alive");
    assert_matches!(
        close,
        None | Some(Ok(Message::Close(_)))
            | Some(Err(tungstenite::Error::Protocol(
                tungstenite::error::ProtocolError::ResetWithoutClosingHandshake
            )))
    );
}

#[tokio::test]
async fn backpressured_delivery_should_stop_on_downstream_cancel_or_pool_shutdown() {
    for cancel_downstream in [true, false] {
        for tail in ["delta", "binary", "completed", "failed", "incomplete"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let mut server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut websocket = accept_codex_test_websocket(socket).await;
                websocket.next().await.unwrap().unwrap();
                send_backpressured_response(&mut websocket, tail).await;
                expect_upstream_socket_closed(&mut websocket).await;
            });
            let pool = Arc::new(CodexWebSocketPool::default());
            let backend = CodexBackendClient::new(
                reqwest::Client::builder().no_proxy().build().unwrap(),
                format!("http://{addr}"),
                test_wire_profile(),
            )
            .with_websocket_pool(Arc::clone(&pool));
            let cancellation = CancellationToken::new();
            let control =
                ResponseControl::default().with_connection_cancellation(cancellation.clone());
            let trace = TraceContext::new("backpressured_delivery");
            let mut context = request_context("backpressured_delivery", Some("chatgpt-account"))
                .with_response_control(Some(&control));
            context.trace = Some(&trace);
            let mut request = pooled_websocket_request("backpressured_delivery");
            request.downstream_websocket_connection_id = Some("downstream_backpressure".into());
            let mut response = backend
                .create_response_stream(&request, context)
                .await
                .unwrap();
            let (stage, count) = if tail == "binary" {
                ("upstream.binary", 1)
            } else {
                ("upstream.event", 17)
            };
            wait_for_forwarded_frames(&trace, stage, count).await;

            if cancel_downstream {
                cancellation.cancel();
                // 必须在 shutdown 前看到物理连接关闭，避免停机掩盖下游取消失效
                (&mut server).await.unwrap();
            }
            timeout(Duration::from_secs(2), pool.shutdown())
                .await
                .expect("shutdown must join delivery even with a full receiver");
            if !cancel_downstream {
                (&mut server).await.unwrap();
            }
            // 收尾前不消费也不丢弃 body，已交付的事件仍可正常读完
            let mut buffered = 0;
            while response.body.next().await.transpose().unwrap().is_some() {
                buffered += 1;
            }
            assert_eq!(
                buffered, 16,
                "tail={tail}, cancel_downstream={cancel_downstream}"
            );
        }
    }
}

#[tokio::test]
async fn terminal_should_release_or_reuse_socket_before_delivery() {
    for tail in ["completed", "failed", "incomplete"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut websocket = accept_codex_test_websocket(socket).await;
            websocket.next().await.unwrap().unwrap();
            send_backpressured_response(&mut websocket, tail).await;
            if tail == "completed" {
                let next = timeout(Duration::from_secs(2), websocket.next())
                    .await
                    .expect("continuation should reuse the completed socket")
                    .unwrap()
                    .unwrap();
                let Message::Text(next) = next else {
                    panic!("expected continuation request");
                };
                assert_eq!(
                    serde_json::from_str::<Value>(&next).unwrap()["previous_response_id"],
                    "resp_backpressure"
                );
                websocket
                    .send(Message::Text(
                        completed_websocket_response("resp_next", 2, 1).into(),
                    ))
                    .await
                    .unwrap();
            }
            expect_upstream_socket_closed(&mut websocket).await;
        });
        let pool = Arc::new(CodexWebSocketPool::default());
        let backend = CodexBackendClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            format!("http://{addr}"),
            test_wire_profile(),
        )
        .with_websocket_pool(Arc::clone(&pool));
        let trace = TraceContext::new("terminal_delivery");
        let mut context = request_context("terminal_delivery", Some("chatgpt-account"));
        context.trace = Some(&trace);
        let mut request = pooled_websocket_request("terminal_delivery");
        let mut response = backend
            .create_response_stream(&request, context)
            .await
            .unwrap();
        wait_for_forwarded_frames(&trace, "upstream.event", 17).await;
        if tail == "completed" {
            request.set_previous_response_id(Some("resp_backpressure".into()));
            request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
            let next = backend
                .create_response(
                    &request,
                    request_context("terminal_next", Some("chatgpt-account")),
                )
                .await
                .expect("completed continuation should be reusable before terminal consumption");
            assert!(next.websocket_pool_decision.unwrap().is_reuse());
        } else {
            // 失败终态必须释放 socket，不能依赖客户端读走终态
            (&mut server).await.unwrap();
        }
        let mut events = Vec::new();
        while let Some(chunk) = response.body.next().await {
            events.push(serde_json::from_slice::<Value>(&chunk.unwrap()).unwrap());
        }
        assert_eq!(events.len(), 17);
        assert_eq!(events.last().unwrap()["type"], format!("response.{tail}"));
        pool.shutdown().await;
        if tail == "completed" {
            (&mut server).await.unwrap();
        }
    }
}

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
