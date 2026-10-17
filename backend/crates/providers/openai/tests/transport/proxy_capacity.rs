//! 使用受限 CONNECT 代理验证跨账号 WebSocket 容量、回收与续接恢复

use super::*;
use gateway_core::{
    account::{CredentialRevision, OutboundProxy, ProviderAccount, ProviderAccountId},
    routing::ProviderKind,
};
use provider_openai::transport::websocket::PreviousResponseUnavailableReason;

fn limited_pool() -> Arc<CodexWebSocketPool> {
    Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        max_age: Duration::from_mins(55),
        ..websocket_pool_config_for_tests(None, None, None)
    }))
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("proxy state should progress");
}

async fn respond(client: &CodexBackendClient, conversation: &str) {
    client
        .create_response(
            &pooled_websocket_request(conversation),
            request_context("capacity", Some("test-upstream")),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn proxy_capacity_shares_budget_across_accounts() {
    for budget in [8, 16, 24, 28] {
        let proxy = LimitedProxy::start(32).await;
        let pool = limited_pool();
        let first = proxy.client(pool.clone(), "acct_a", budget);
        let second = proxy.client(pool.clone(), "acct_b", budget);
        for index in 0..64 {
            respond(
                if index % 2 == 0 { &first } else { &second },
                &format!("shared-{index}"),
            )
            .await;
        }
        wait_until(|| proxy.active.load(Ordering::SeqCst) == budget as usize).await;
        assert_eq!(proxy.opened.load(Ordering::SeqCst), 64);
        assert!(
            proxy.peak.load(Ordering::SeqCst) <= budget as usize,
            "physical connections must stay within budget {budget}"
        );
        pool.shutdown().await;
    }
}

#[tokio::test]
async fn proxy_capacity_reuses_and_evicts_lru_with_original_replay_contract() {
    let proxy = LimitedProxy::start(32).await;
    let pool = limited_pool();
    let client = proxy.client(pool.clone(), "acct_a", 2);
    respond(&client, "a").await;
    respond(&client, "b").await;
    respond(&client, "a").await;
    assert_eq!(
        proxy.opened.load(Ordering::SeqCst),
        2,
        "reuse while capacity is available"
    );
    respond(&client, "c").await;
    respond(&client, "a").await;
    assert_eq!(
        proxy.opened.load(Ordering::SeqCst),
        3,
        "recently used a must survive"
    );
    let mut continuation = pooled_websocket_request("b");
    continuation.set_previous_response_id(Some("resp_proxy_1".to_owned()));
    continuation.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
    let error = client
        .create_response(
            &continuation,
            request_context("continuation", Some("test-upstream")),
        )
        .await
        .unwrap_err();
    let CodexClientError::WebSocket(error) = error else {
        panic!("expected existing replay error");
    };
    assert_eq!(
        error.continuation_unavailable_reason(),
        Some(PreviousResponseUnavailableReason::ReusedConnectionLost)
    );
    assert_eq!(
        error.connection_observation().unwrap().exit_reason(),
        "proxy_capacity_evicted"
    );
    assert_eq!(
        proxy.opened.load(Ordering::SeqCst),
        3,
        "do not retry the evicted socket"
    );
    let mut replay = codex_request(
        "gpt-test",
        "be brief",
        vec![json!({"role":"user","content":"full history"})],
    );
    replay.local_conversation_id = Some("b".to_owned());
    client
        .create_response(&replay, request_context("replay", Some("test-upstream")))
        .await
        .unwrap();
    {
        let requests = proxy.requests.lock().unwrap();
        let replayed = requests.last().unwrap();
        assert!(replayed.get("previous_response_id").is_none());
        assert_eq!(replayed["input"][0]["content"], "full history");
    }
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 4);
    pool.shutdown().await;
}

#[tokio::test]
async fn proxy_capacity_preserves_idle_reuse_beyond_two_minutes_without_pressure() {
    let proxy = LimitedProxy::start(32).await;
    let pool = limited_pool();
    let client = proxy.client(pool.clone(), "acct_a", 1);
    respond(&client, "idle").await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(121)).await;
    tokio::time::resume();
    respond(&client, "idle").await;
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 1);
    pool.shutdown().await;
}

#[tokio::test]
async fn proxy_capacity_does_not_limit_direct_connections() {
    let pool = limited_pool();
    let proxy = LimitedProxy::start(32).await;
    respond(&proxy.client(pool.clone(), "acct_proxied", 1), "proxied").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut sockets = tokio::task::JoinSet::new();
        for _ in 0..3 {
            let (stream, _) = listener.accept().await.unwrap();
            sockets.spawn(async move {
                let mut ws = accept_codex_test_websocket(stream).await;
                ws.next().await.unwrap().unwrap();
                ws.send(Message::Text(
                    completed_websocket_response("resp_direct", 1, 1).into(),
                ))
                .await
                .unwrap();
                std::assert_matches!(ws.next().await.unwrap().unwrap(), Message::Close(_));
            });
        }
        while let Some(result) = sockets.join_next().await {
            result.unwrap();
        }
    });
    let direct = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{address}"),
        test_wire_profile(),
    )
    .with_websocket_pool(pool.clone());
    for index in 0..3 {
        timeout(
            Duration::from_secs(3),
            respond(&direct, &format!("direct-{index}")),
        )
        .await
        .unwrap();
    }
    assert_eq!(proxy.active.load(Ordering::SeqCst), 1);
    pool.shutdown().await;
    server.await.unwrap();
}

#[tokio::test]
async fn proxy_capacity_releases_failed_handshake_before_the_next_connection() {
    let proxy = LimitedProxy::start(32).await;
    let pool = limited_pool();
    let client = proxy.client(pool.clone(), "acct_a", 1);
    proxy.reject_connects.store(1, Ordering::SeqCst);
    let mut request = pooled_websocket_request("rejected");
    request.downstream_websocket_connection_id = Some("downstream-rejected".to_owned());
    request.set_previous_response_id(None);
    request.previous_response_scope = None;
    assert!(
        client
            .create_response(&request, request_context("rejected", Some("test-upstream")))
            .await
            .is_err()
    );
    timeout(Duration::from_secs(3), respond(&client, "after-failure"))
        .await
        .unwrap();
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 1);
    pool.shutdown().await;
}

#[tokio::test]
async fn proxy_capacity_protects_downstream_bindings_and_blocks_unpooled_bypass() {
    let proxy = LimitedProxy::start(32).await;
    let pool = limited_pool();
    let client = proxy.client(pool.clone(), "acct_a", 1);
    let mut pinned = pooled_websocket_request("pinned");
    pinned.downstream_websocket_connection_id = Some("downstream-a".to_owned());
    client
        .create_response(&pinned, request_context("pinned", Some("test-upstream")))
        .await
        .unwrap();
    let unpooled = codex_request("gpt-test", "be brief", Vec::new());
    assert!(
        timeout(
            Duration::from_millis(1100),
            client.create_response(
                &unpooled,
                request_context("unpooled", Some("test-upstream"))
            )
        )
        .await
        .is_err()
    );
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 1);
    client
        .create_response(
            &pinned,
            request_context("pinned-reuse", Some("test-upstream")),
        )
        .await
        .unwrap();
    assert_eq!(
        proxy.opened.load(Ordering::SeqCst),
        1,
        "capacity cancellation must not remove pinned idle connection"
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn proxy_capacity_waits_for_busy_connections_then_reclaims_returned_idle() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let proxy = LimitedProxy::with_gates(32, None, Some(gate.clone())).await;
    let pool = limited_pool();
    let client = proxy.client(pool.clone(), "acct_a", 24);
    let mut requests = Vec::new();
    for index in 0..25 {
        let client = client.clone();
        requests.push(tokio::spawn(async move {
            respond(&client, &format!("busy-{index}")).await
        }));
        if index < 24 {
            wait_until(|| proxy.requests.lock().unwrap().len() == index + 1).await;
        }
    }
    wait_until(|| proxy.requests.lock().unwrap().len() == 24).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        proxy.opened.load(Ordering::SeqCst),
        24,
        "all busy must wait beyond the HTTP fast-path budget"
    );
    gate.add_permits(1);
    wait_until(|| proxy.requests.lock().unwrap().len() == 25).await;
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 25);
    gate.add_permits(24);
    for task in requests {
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }
    pool.shutdown().await;
}

#[tokio::test]
async fn proxy_capacity_counts_connecting_and_shutdown_cancels_waiters() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let proxy = LimitedProxy::with_gates(32, Some(gate), None).await;
    let pool = limited_pool();
    let client = proxy.client(pool.clone(), "acct_a", 2);
    let mut requests = Vec::new();
    for index in 0..3 {
        let client = client.clone();
        requests.push(tokio::spawn(async move {
            let mut request = pooled_websocket_request(&format!("connecting-{index}"));
            request.downstream_websocket_connection_id = Some(format!("downstream-{index}"));
            request.set_previous_response_id(None);
            request.previous_response_scope = None;
            client
                .create_response(
                    &request,
                    request_context("connecting", Some("test-upstream")),
                )
                .await
        }));
    }
    wait_until(|| proxy.active.load(Ordering::SeqCst) == 2).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(proxy.active.load(Ordering::SeqCst), 2);
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 0);
    timeout(Duration::from_secs(3), pool.shutdown())
        .await
        .unwrap();
    for task in requests {
        assert!(
            timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
}

#[tokio::test]
async fn proxy_capacity_isolates_different_proxy_endpoints() {
    let first = LimitedProxy::start(32).await;
    let second = LimitedProxy::start(32).await;
    let pool = limited_pool();
    respond(&first.client(pool.clone(), "acct_a", 1), "first").await;
    respond(&second.client(pool.clone(), "acct_a", 2), "second").await;
    respond(&second.client(pool.clone(), "acct_a", 2), "second-extra").await;
    assert_eq!(first.active.load(Ordering::SeqCst), 1);
    assert_eq!(second.active.load(Ordering::SeqCst), 2);
    pool.shutdown().await;
}

#[tokio::test]
async fn proxy_capacity_configured_24_leaves_eight_http_tunnels_on_a_32_slot_proxy() {
    let proxy = LimitedProxy::start(32).await;
    let pool = limited_pool();
    let client = proxy.client(pool.clone(), "acct_a", 24);
    for index in 0..24 {
        respond(&client, &format!("headroom-{index}")).await;
    }
    let mut http_tunnels = Vec::new();
    for _ in 0..8 {
        let mut stream = TcpStream::connect(proxy.url.trim_start_matches("http://"))
            .await
            .unwrap();
        stream.write_all(b"CONNECT upstream.invalid:80 HTTP/1.1\r\nHost: upstream.invalid:80\r\nX-Capacity-Probe: health\r\n\r\n").await.unwrap();
        let response = read_http_request(&mut stream).await;
        assert!(response.starts_with("HTTP/1.1 200"));
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: upstream.invalid\r\n\r\n")
            .await
            .unwrap();
        let response = read_http_request(&mut stream).await;
        assert!(response.starts_with("HTTP/1.1 200"));
        http_tunnels.push(stream);
    }
    assert_eq!(proxy.active.load(Ordering::SeqCst), 32);
    timeout(Duration::from_secs(3), respond(&client, "headroom-new"))
        .await
        .unwrap();
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 25);
    assert_eq!(proxy.peak.load(Ordering::SeqCst), 32);
    drop(http_tunnels);
    pool.shutdown().await;
}

struct LimitedProxy {
    url: String,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    opened: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Value>>>,
    reject_connects: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

struct ActiveTunnel(Arc<AtomicUsize>);

impl Drop for ActiveTunnel {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for LimitedProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl LimitedProxy {
    async fn start(limit: usize) -> Self {
        Self::with_gates(limit, None, None).await
    }

    async fn with_gates(
        limit: usize,
        connect_gate: Option<Arc<tokio::sync::Semaphore>>,
        response_gate: Option<Arc<tokio::sync::Semaphore>>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let opened = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reject_connects = Arc::new(AtomicUsize::new(0));
        let rejects = reject_connects.clone();
        let recorded = requests.clone();
        let (a, p, o) = (active.clone(), peak.clone(), opened.clone());
        let task = tokio::spawn(async move {
            let slots = Arc::new(tokio::sync::Semaphore::new(limit));
            let mut tunnels = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut stream, _) = accepted.unwrap();
                        let (slots, active, peak, opened) = (slots.clone(), a.clone(), p.clone(), o.clone());
                        let (connect_gate, response_gate, requests) = (connect_gate.clone(), response_gate.clone(), recorded.clone());
                        let reject_connects = rejects.clone();
                        tunnels.spawn(async move {
                            let _slot = slots.acquire_owned().await.unwrap();
                            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                            let _active = ActiveTunnel(active);
                            peak.fetch_max(count, Ordering::SeqCst);
                            let request = read_http_request(&mut stream).await;
                            assert!(request.starts_with("CONNECT upstream.invalid:80 "));
                            if reject_connects.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| remaining.checked_sub(1)).is_ok() {
                                stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").await.unwrap();
                                return;
                            }
                            if let Some(gate) = connect_gate { gate.acquire().await.unwrap().forget(); }
                            stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();
                            if request.contains("X-Capacity-Probe: health") {
                                let health = read_http_request(&mut stream).await;
                                assert!(health.starts_with("GET /health HTTP/1.1"));
                                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await.unwrap();
                                let mut byte = [0];
                                let _ = stream.read(&mut byte).await;
                                return;
                            }
                            let Ok(mut ws) = accept_hdr_async_with_config(stream, TestWebSocketCallback(|_request: &WebSocketRequest, response: &mut WebSocketResponse| { response.headers_mut().insert("sec-websocket-extensions", "permessage-deflate".parse().unwrap()); }), Some(websocket_accept_config())).await else { return; };
                            let id = opened.fetch_add(1, Ordering::SeqCst);
                            while let Some(Ok(message)) = ws.next().await {
                                match message {
                                    Message::Text(payload) => {
                                        requests.lock().unwrap().push(serde_json::from_str(&payload).unwrap());
                                        if let Some(gate) = &response_gate { gate.acquire().await.unwrap().forget(); }
                                        if ws.send(Message::Text(completed_websocket_response(&format!("resp_proxy_{id}"), 1, 1).into())).await.is_err() { break; }
                                    }
                                    Message::Ping(payload) => { if ws.send(Message::Pong(payload)).await.is_err() { break; } }
                                    Message::Close(_) => { let _ = ws.flush().await; break; }
                                    _ => {}
                                }
                            }
                        });
                    }
                    _ = tunnels.join_next(), if !tunnels.is_empty() => {}
                }
            }
        });
        Self {
            url,
            active,
            peak,
            opened,
            requests,
            reject_connects,
            task,
        }
    }

    fn client(
        &self,
        pool: Arc<CodexWebSocketPool>,
        account_id: &str,
        limit: u32,
    ) -> CodexBackendClient {
        self.client_with_proxy_user(pool, account_id, limit, "proxy-user")
    }

    fn client_with_proxy_user(
        &self,
        pool: Arc<CodexWebSocketPool>,
        account_id: &str,
        limit: u32,
        proxy_user: &str,
    ) -> CodexBackendClient {
        let account = ProviderAccount::new(
            ProviderAccountId::new(account_id).unwrap(),
            ProviderKind::new("openai").unwrap(),
            account_id.to_owned(),
            Some(account_id.to_owned()),
            "oauth".to_owned(),
            CredentialRevision::new(1).unwrap(),
            None,
        )
        .with_outbound_proxy(Some(
            OutboundProxy::parse(
                &self
                    .url
                    .replace("http://", &format!("http://{proxy_user}:test@")),
            )
            .unwrap()
            .with_max_websocket_connections(limit),
        ));
        CodexBackendClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            "http://upstream.invalid",
            test_wire_profile(),
        )
        .with_websocket_pool(pool)
        .for_account(&account)
        .unwrap()
    }
}

#[tokio::test]
async fn proxy_capacity_unconfigured_proxy_keeps_existing_unbounded_behavior() {
    let proxy = LimitedProxy::start(32).await;
    let pool = limited_pool();
    let client = proxy.client(pool.clone(), "acct_a", 0);
    for index in 0..32 {
        client
            .create_response(
                &pooled_websocket_request(&format!("baseline-{index}")),
                request_context("baseline", Some("acct_a")),
            )
            .await
            .unwrap();
    }
    let next = pooled_websocket_request("baseline-33");
    assert!(
        timeout(
            Duration::from_millis(300),
            client.create_response(&next, request_context("baseline-33", Some("acct_a")))
        )
        .await
        .is_err()
    );
    assert_eq!(proxy.active.load(Ordering::SeqCst), 32);
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 32);
    assert_eq!(proxy.peak.load(Ordering::SeqCst), 32);
    pool.shutdown().await;
}

#[tokio::test]
async fn proxy_capacity_enabling_and_lowering_limits_keeps_existing_socket_count() {
    let proxy = LimitedProxy::start(32).await;
    let pool = limited_pool();
    let unlimited = proxy.client(pool.clone(), "acct_a", 0);
    for index in 0..3 {
        respond(&unlimited, &format!("unlimited-{index}")).await;
    }
    assert_eq!(proxy.active.load(Ordering::SeqCst), 3);
    let limited = proxy.client(pool.clone(), "acct_a", 1);
    respond(&limited, "unlimited-2").await;
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 3);
    respond(&limited, "enabled-limit").await;
    assert_eq!(proxy.active.load(Ordering::SeqCst), 1);
    assert_eq!(proxy.peak.load(Ordering::SeqCst), 3);
    let raised = proxy.client(pool.clone(), "acct_a", 2);
    respond(&raised, "raised-limit").await;
    assert_eq!(proxy.active.load(Ordering::SeqCst), 2);
    respond(&limited, "lowered-again").await;
    assert_eq!(proxy.active.load(Ordering::SeqCst), 1);
    pool.shutdown().await;
}

#[tokio::test]
async fn proxy_capacity_enabling_limit_preserves_existing_downstream_bindings() {
    let proxy = LimitedProxy::start(32).await;
    let pool = limited_pool();
    let unlimited = proxy.client(pool.clone(), "acct_a", 0);
    for index in 0..2 {
        let mut request = pooled_websocket_request(&format!("pinned-{index}"));
        request.downstream_websocket_connection_id = Some(format!("downstream-{index}"));
        unlimited
            .create_response(&request, request_context("pinned", Some("test-upstream")))
            .await
            .unwrap();
    }
    let limited = proxy.client(pool.clone(), "acct_a", 1);
    let mut retained = pooled_websocket_request("pinned-0");
    retained.downstream_websocket_connection_id = Some("downstream-0".to_owned());
    limited
        .create_response(
            &retained,
            request_context("pinned-reused", Some("test-upstream")),
        )
        .await
        .unwrap();
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 2);
    assert!(
        timeout(Duration::from_millis(100), respond(&limited, "must-wait"))
            .await
            .is_err()
    );
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 2);
    let raised = proxy.client(pool.clone(), "acct_a", 3);
    respond(&raised, "raised-limit").await;
    assert_eq!(proxy.active.load(Ordering::SeqCst), 3);
    pool.shutdown().await;
}

#[tokio::test]
async fn proxy_capacity_keeps_separate_proxy_credentials_independent() {
    let proxy = LimitedProxy::start(32).await;
    let pool = limited_pool();
    let first = proxy.client(pool.clone(), "acct_a", 1);
    let second = proxy.client_with_proxy_user(pool.clone(), "acct_b", 2, "other-proxy-user");
    respond(&first, "first").await;
    respond(&second, "second").await;
    respond(&second, "second-extra").await;
    assert_eq!(proxy.active.load(Ordering::SeqCst), 3);
    respond(&first, "first-new").await;
    assert_eq!(proxy.active.load(Ordering::SeqCst), 3);
    respond(&second, "second").await;
    assert_eq!(proxy.opened.load(Ordering::SeqCst), 4);
    pool.shutdown().await;
}
