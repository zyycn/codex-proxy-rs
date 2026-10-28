//! 验证控制消息写入阻塞时，超时、取消和空闲池回收仍能释放物理连接

use super::*;
use crate::transport::accept_codex_test_websocket_with;

#[tokio::test]
async fn receive_timeout_should_abort_stalled_control_write() {
    assert_stalled_control_cleanup(false).await;
}

#[tokio::test]
async fn cancellation_should_abort_stalled_control_write() {
    assert_stalled_control_cleanup(true).await;
}

#[tokio::test]
async fn pool_shutdown_should_abort_stalled_idle_control_write() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(8192).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = socket.listen(1).unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (started, started_rx) = oneshot::channel();
    let (drain, drain_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket_with(socket, |_, response| {
            response.headers_mut().remove("sec-websocket-extensions");
        })
        .await;
        assert_eq!(next_json(&mut ws).await["type"], "response.create");
        // 未协商压缩，直接写入终态，让连接归池后再阻塞下一条控制消息
        let terminal = br#"{"type":"response.completed","response":{"id":"resp_idle_control","status":"completed","output":[]}}"#;
        assert!(terminal.len() < 126);
        ws.get_mut()
            .write_all(&[0x81, u8::try_from(terminal.len()).unwrap()])
            .await
            .unwrap();
        ws.get_mut().write_all(terminal).await.unwrap();
        ws.get_mut().read_u8().await.unwrap();
        started.send(()).unwrap();
        drain_rx.await.unwrap();
        tokio::io::copy(ws.get_mut(), &mut tokio::io::sink())
            .await
            .unwrap();
    });
    let (provider, pool) =
        provider_with_cooldowns(&store, base_url, Arc::new(MemoryCooldownPort::new()));
    let control = ResponseControl::default();
    let request = generate_with_persisted_session_context(
        "acct_provider_contract",
        "idle-control-close",
        "idle-control-session",
        "idle-control-thread",
    );
    let mut stream = provider
        .execute(
            planned_request("openai", Operation::Generate(request)),
            interrupt_context(control.clone(), None),
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(3), async {
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    })
    .await
    .expect("the response must finish and return its connection to the pool");

    let payload =
        json!({"type": "future.control", "data": "x".repeat(16 * 1024 * 1024)}).to_string();
    let pending = control.send(&payload);
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => panic!("control write should stall: {result:?}"),
        ready = started_rx => ready.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("control write should start"),
    }
    timeout(Duration::from_secs(2), pool.shutdown())
        .await
        .expect("pool shutdown must bound Close queued behind a stalled write");
    assert_eq!(
        timeout(Duration::from_secs(1), pending)
            .await
            .expect("pool shutdown must stop the pending control write"),
        Err(ResponseControlUnavailable)
    );
    drain.send(()).unwrap();
    timeout(Duration::from_secs(5), server)
        .await
        .expect("pool shutdown must release the physical socket")
        .unwrap();
}

async fn assert_stalled_control_cleanup(cancel_response: bool) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(8192).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = socket.listen(1).unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (started, started_rx) = oneshot::channel();
    let (drain, drain_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        // 不协商压缩，保证控制帧足够大，能够填满 TCP 发送缓冲
        let mut ws = accept_codex_test_websocket_with(socket, |_, response| {
            response.headers_mut().remove("sec-websocket-extensions");
        })
        .await;
        assert_eq!(next_json(&mut ws).await["type"], "response.create");
        // 直接写入未压缩的文本帧，与握手中拒绝扩展的结果一致
        let delta = br#"{"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"partial"}"#;
        assert!(delta.len() < 126);
        ws.get_mut()
            .write_all(&[0x81, u8::try_from(delta.len()).unwrap()])
            .await
            .unwrap();
        ws.get_mut().write_all(delta).await.unwrap();
        ws.get_mut().read_u8().await.unwrap();
        started.send(()).unwrap();
        drain_rx.await.unwrap();
        tokio::io::copy(ws.get_mut(), &mut tokio::io::sink())
            .await
            .unwrap();
    });
    let control = ResponseControl::default();
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", generate_operation()),
            interrupt_context(control.clone(), None),
        )
        .await
        .unwrap();
    loop {
        if stream.next().await.unwrap().unwrap().has_client_event() {
            break;
        }
    }
    let payload =
        json!({"type": "future.control", "data": "x".repeat(16 * 1024 * 1024)}).to_string();
    let pending = control.send(&payload);
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => panic!("control write should stall: {result:?}"),
        ready = started_rx => ready.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("control write should start"),
    }
    if cancel_response {
        drop(stream);
    } else {
        // 已有输出后才快进空闲时限，错误必须通过流返回，不能被 Close 发送阻塞
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(301)).await;
        let result = timeout(Duration::from_secs(1), async {
            loop {
                if let Err(error) = stream.next().await.expect("timeout should yield an error") {
                    break error;
                }
            }
        })
        .await;
        tokio::time::resume();
        let error = result.expect("receive timeout must not wait for a stalled control write");
        assert_eq!(
            error.diagnostic().unwrap().code(),
            Some("receive_idle_timeout")
        );
        assert!(error.pre_delivery_retry().is_none());
        drop(stream);
    }
    assert_eq!(
        timeout(Duration::from_secs(2), pending)
            .await
            .expect("cleanup must stop the pending control write"),
        Err(ResponseControlUnavailable)
    );
    assert_eq!(control.send("{}").await, Err(ResponseControlUnavailable));
    drain.send(()).unwrap();
    timeout(Duration::from_secs(5), server)
        .await
        .expect("cleanup must release the physical socket")
        .unwrap();
}
