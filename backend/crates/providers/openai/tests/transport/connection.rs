//! 在独立测试进程中验证共享建连准入，避免其他网络测试占用全局名额

use super::*;
use gateway_core::account::OutboundProxy;
use provider_openai::transport::client::build_account_http_client;

#[tokio::test]
async fn cold_connection_admission_bounds_queue_and_releases_cancelled_work() {
    const CHILD: &str = "CPR_CONNECTION_ADMISSION_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "transport::connection::cold_connection_admission_bounds_queue_and_releases_cancelled_work", "--nocapture"])
            .env(CHILD, "1")
            .status().unwrap();
        assert!(status.success());
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy =
        OutboundProxy::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let client = build_account_http_client("acct_connection_admission", Some(&proxy)).unwrap();
    let (completed, mut results) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = Vec::new();
    let spawn = |client: reqwest::Client, completed: tokio::sync::mpsc::UnboundedSender<bool>| {
        tokio::spawn(async move {
            let result = client.get("https://upstream.invalid/").send().await;
            let _ = completed.send(result.is_err_and(|error| error.is_connect()));
        })
    };
    for _ in 0..128 {
        tasks.push(spawn(client.clone(), completed.clone()));
    }
    let mut sockets = Vec::new();
    for _ in 0..128 {
        sockets.push(
            timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap()
                .0,
        );
    }
    // 128 条 CONNECT 握手保持挂起；1024 个等待者之外的一次请求应立即拒绝
    for _ in 0..1025 {
        tasks.push(spawn(client.clone(), completed.clone()));
    }
    assert_eq!(
        timeout(Duration::from_secs(3), results.recv())
            .await
            .unwrap(),
        Some(true)
    );
    assert!(
        timeout(Duration::from_millis(100), results.recv())
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
    crate::provider::assert_local_connection_capacity_is_not_an_upstream_failure().await;

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", upstream.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut http, _) = upstream.accept().await.unwrap();
            assert!(
                read_http_request(&mut http)
                    .await
                    .starts_with("POST /codex/responses")
            );
            write_completed_sse_response(&mut http).await;
        }
        let (stream, _) = upstream.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        websocket.next().await.unwrap().unwrap();
        websocket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                completed_websocket_response("resp_after_local_capacity", 2, 1).into(),
            ))
            .await
            .unwrap();
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        base_url,
        test_wire_profile(),
    );
    for attempt in 0..3 {
        let mut request = codex_request("gpt-5.5", "be brief", Vec::new());
        request.use_websocket = true;
        request.local_conversation_id = Some(format!("capacity-{attempt}"));
        let response = backend
            .create_response(
                &request,
                request_context("req_local_ws_capacity", Some("chatgpt-account")),
            )
            .await
            .expect("local WS capacity should fall back to HTTP");
        assert_eq!(response.transport, CodexBackendTransport::HttpSse);
        assert_eq!(
            response.transport_metrics.decision,
            Some(CodexTransportDecision::Http2LocalConnectionCapacity)
        );
    }

    tasks[0].abort();
    sockets.push(
        timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0,
    );
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }
    // 同时取消活动与排队请求后，新请求仍能取得名额
    let fresh = spawn(client, completed);
    let _socket = timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    fresh.abort();
    let _ = fresh.await;
    drop(sockets);

    let mut request = codex_request("gpt-5.5", "be brief", Vec::new());
    request.use_websocket = true;
    request.local_conversation_id = Some("capacity-recovered".to_owned());
    let response = timeout(
        Duration::from_secs(3),
        backend.create_response(
            &request,
            request_context("req_ws_after_capacity", Some("chatgpt-account")),
        ),
    )
    .await
    .unwrap()
    .expect("local capacity must not open the WS origin breaker");
    assert_eq!(response.transport, CodexBackendTransport::WebSocket);
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}
