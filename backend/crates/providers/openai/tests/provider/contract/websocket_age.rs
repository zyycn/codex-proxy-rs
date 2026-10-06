//! 验证账号 WebSocket 复用寿命的原生传递、空闲回收与续接恢复

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{Notify, mpsc};
use tokio::task::{JoinHandle, JoinSet};

use super::*;
use provider_openai::credential::ResponsesTransport;

const ACCOUNT: &str = "acct_provider_contract";
const DEFAULT_ACCOUNT: &str = "acct_affinity";
const ACCOUNT_AGE: Duration = Duration::from_secs(30);

struct WireRequest {
    connection: usize,
    body: Value,
}

struct LoopbackUpstream {
    base_url: String,
    requests: mpsc::UnboundedReceiver<WireRequest>,
    closed: mpsc::UnboundedReceiver<usize>,
    release: Arc<Notify>,
    accepted: Arc<AtomicUsize>,
    server: JoinHandle<()>,
}

impl LoopbackUpstream {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (requests_tx, requests) = mpsc::unbounded_channel();
        let (closed_tx, closed) = mpsc::unbounded_channel();
        let accepted = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());
        let server_accepted = Arc::clone(&accepted);
        let server_release = Arc::clone(&release);
        let server = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        let (stream, _) = result.unwrap();
                        let connection = server_accepted.fetch_add(1, Ordering::SeqCst) + 1;
                        let requests = requests_tx.clone();
                        let closed = closed_tx.clone();
                        let release = Arc::clone(&server_release);
                        connections.spawn(async move {
                            let mut websocket = accept_codex_test_websocket(stream).await;
                            let mut sequence = 0;
                            while let Some(message) = websocket.next().await {
                                let Ok(message) = message else { break };
                                match message {
                                    Message::Text(text) => {
                                        sequence += 1;
                                        let body: Value = serde_json::from_str(&text).unwrap();
                                        let held = body.get("test_hold") == Some(&json!(true));
                                        requests.send(WireRequest { connection, body }).unwrap();
                                        if held {
                                            release.notified().await;
                                        }
                                        let response = json!({
                                            "type": "response.completed",
                                            "response": {
                                                "id": format!("resp_age_{connection}_{sequence}"),
                                                "object": "response",
                                                "output": [],
                                                "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
                                            }
                                        });
                                        websocket.send(Message::Text(response.to_string().into())).await.unwrap();
                                    }
                                    Message::Close(_) => break,
                                    Message::Ping(payload) => {
                                        websocket.send(Message::Pong(payload)).await.unwrap();
                                    }
                                    _ => {}
                                }
                            }
                            let _ = closed.send(connection);
                        });
                    }
                    result = connections.join_next(), if !connections.is_empty() => {
                        result.unwrap().unwrap();
                    }
                }
            }
        });
        Self {
            base_url,
            requests,
            closed,
            release,
            accepted,
            server,
        }
    }

    async fn request(&mut self) -> WireRequest {
        timeout(Duration::from_secs(2), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn closed(&mut self) -> usize {
        timeout(Duration::from_secs(2), self.closed.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

impl Drop for LoopbackUpstream {
    fn drop(&mut self) {
        self.server.abort();
    }
}

struct AgeFixture {
    store: Arc<MemoryAccountStore>,
    provider: Arc<CodexProvider>,
    pool: Arc<CodexWebSocketPool>,
    upstream: LoopbackUpstream,
}

impl AgeFixture {
    async fn new(api_key: bool, global_age: Duration) -> Self {
        let upstream = LoopbackUpstream::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        for account in [ACCOUNT, DEFAULT_ACCOUNT] {
            if api_key {
                store
                    .seed_api_key(
                        account,
                        upstream.base_url.clone(),
                        ResponsesTransport::PreferWebsocket,
                    )
                    .await;
            } else {
                create_account(&store, account).await;
            }
        }
        let pool = Arc::new(CodexWebSocketPool::new(global_age));
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let profile = wire_profile();
        let leases = Arc::new(TestLeaseCoordinator::default());
        let feedback = Arc::new(AccountFeedbackStats::default());
        let quota = Arc::new(CodexCredentialQuotaService::new(
            store.repository(),
            profile.clone(),
            http.clone(),
            upstream.base_url.clone(),
            Arc::new(MemoryCooldownPort::default()),
            Arc::clone(&leases) as Arc<dyn ProviderLeasePort>,
            crate::support::runtime_policy(),
        ));
        let catalog = Arc::new(CodexCredentialCatalogService::new(
            store.repository(),
            profile.clone(),
            http.clone(),
            upstream.base_url.clone(),
            catalog_cache(),
        ));
        let selector = Arc::new(CodexCredentialSelector::new(
            ProviderKind::new("openai").unwrap(),
            store.repository(),
            leases,
            Arc::new(MemorySessionAffinity::default()),
            Arc::new(MemorySessionExclusions::default()),
            Arc::clone(&quota),
            Arc::clone(&feedback),
            CodexCookiePolicy::official().unwrap(),
        ));
        let provider = Arc::new(
            CodexProvider::new(
                selector,
                catalog,
                quota,
                feedback,
                http,
                profile,
                upstream.base_url.clone(),
                Arc::clone(&pool),
                u32::try_from(DEFAULT_STREAM_MAX_RETRIES).unwrap(),
            )
            .unwrap(),
        );
        Self {
            store,
            provider,
            pool,
            upstream,
        }
    }
}

fn age_operation(account: &str, conversation: &str, extra: Map<String, Value>) -> Operation {
    let mut body = Map::from_iter([
        ("model".to_owned(), json!("gpt-5.4")),
        ("input".to_owned(), json!([])),
        ("store".to_owned(), json!(false)),
        ("session_id".to_owned(), json!(conversation)),
        ("thread_id".to_owned(), json!(conversation)),
    ]);
    body.extend(extra);
    let payload = ProtocolPayload::json_object("openai", body)
        .unwrap()
        .with_context(Map::from_iter([
            ("use_websocket".to_owned(), json!(true)),
            ("session_id".to_owned(), json!(conversation)),
        ]));
    Operation::Generate(
        GenerateRequest::from_protocol_payload(payload).with_provider_session_state(
            ProviderSessionState::new(
                "openai",
                Map::from_iter([
                    ("account_id".to_owned(), json!(account)),
                    ("conversation_id".to_owned(), json!(conversation)),
                    ("continuation_scope".to_owned(), json!("connection_local")),
                ]),
            )
            .unwrap(),
        ),
    )
}

async fn age_response(
    provider: &Arc<CodexProvider>,
    account: &str,
    conversation: &str,
    extra: Map<String, Value>,
) -> WebSocketPoolKind {
    let account_scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new(account).unwrap(),
            RuntimeAccount::new(ProviderKind::new("openai").unwrap(), BTreeSet::new()),
        )]))),
        ClientRoutingScope::all_accounts(),
    ));
    let context = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_websocket_age").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        ),
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(account_scope),
        None,
        CancellationToken::new(),
    );
    let mut stream = Arc::clone(provider)
        .execute(
            planned_request("openai", age_operation(account, conversation, extra)),
            context,
        )
        .await
        .unwrap();
    let mut decision = None;
    let mut completed = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("account reuse age must not interrupt an active response");
        if let Some(observation) = event.response_observation() {
            decision = observation.websocket_pool().or(decision);
        }
        if let Some(wire) = event.wire_event() {
            completed |= wire.event_type() == Some("response.completed");
        }
    }
    assert!(completed);
    decision.expect("native WebSocket pool observation")
}

async fn advance_connection_age(age: Duration) {
    tokio::time::pause();
    tokio::time::advance(age).await;
    tokio::time::resume();
}

async fn assert_expired_local_continuation(
    provider: &Arc<CodexProvider>,
    conversation: &str,
    response_id: &str,
) {
    let mut stream = Arc::clone(provider)
        .execute(
            planned_request(
                "openai",
                age_operation(
                    ACCOUNT,
                    conversation,
                    Map::from_iter([("previous_response_id".to_owned(), json!(response_id))]),
                ),
            ),
            pinned_continuation_context(
                "req_age_continuation",
                ACCOUNT,
                response_id,
                response_id,
                1,
                ContinuationAttempt::Native,
            ),
        )
        .await
        .unwrap();
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("expired local continuation requires recovery before send"),
        }
    };
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert_eq!(
        error.continuation_failure(),
        Some(ContinuationFailure::HistoryUnavailable)
    );
    assert_eq!(
        error.continuation_recovery_disposition(),
        Some(ContinuationRecoveryDisposition::ClientReplayRequired)
    );
    assert_eq!(
        error.client_visible_upstream_error().unwrap().code(),
        Some("previous_response_not_found")
    );
    assert_eq!(
        error.connection_observation().unwrap().exit_reason(),
        "max_age_expired"
    );
}

#[tokio::test]
async fn websocket_max_age_override_retires_only_the_selected_oauth_or_api_key_account() {
    for api_key in [false, true] {
        let mut fixture = AgeFixture::new(api_key, Duration::from_secs(600)).await;
        fixture.store.set_websocket_max_age(ACCOUNT, 30_000);
        assert_eq!(
            age_response(&fixture.provider, ACCOUNT, "age-account", Map::new()).await,
            WebSocketPoolKind::New
        );
        let first = fixture.upstream.request().await;
        assert!(first.body.get("websocket_max_age_limit").is_none());
        assert!(first.body.get("websocket_max_age_ms").is_none());
        assert_eq!(
            age_response(
                &fixture.provider,
                DEFAULT_ACCOUNT,
                "age-default",
                Map::new()
            )
            .await,
            WebSocketPoolKind::New
        );
        let default = fixture.upstream.request().await;
        advance_connection_age(ACCOUNT_AGE).await;
        assert_eq!(
            age_response(&fixture.provider, ACCOUNT, "age-account", Map::new()).await,
            WebSocketPoolKind::New
        );
        assert_ne!(
            fixture.upstream.request().await.connection,
            first.connection
        );
        assert_eq!(
            age_response(
                &fixture.provider,
                DEFAULT_ACCOUNT,
                "age-default",
                Map::new()
            )
            .await,
            WebSocketPoolKind::Reuse
        );
        assert_eq!(
            fixture.upstream.request().await.connection,
            default.connection
        );
        assert_eq!(fixture.upstream.accepted.load(Ordering::SeqCst), 3);
        fixture.pool.shutdown().await;
    }
}

#[tokio::test]
async fn websocket_max_age_account_override_cannot_raise_the_global_cap() {
    let mut fixture = AgeFixture::new(false, ACCOUNT_AGE).await;
    fixture.store.set_websocket_max_age(ACCOUNT, 120_000);
    assert_eq!(
        age_response(&fixture.provider, ACCOUNT, "age-global", Map::new()).await,
        WebSocketPoolKind::New
    );
    let first = fixture.upstream.request().await;
    advance_connection_age(ACCOUNT_AGE).await;
    assert_eq!(
        age_response(&fixture.provider, ACCOUNT, "age-global", Map::new()).await,
        WebSocketPoolKind::New
    );
    assert_ne!(
        fixture.upstream.request().await.connection,
        first.connection
    );
    fixture.pool.shutdown().await;
}

#[tokio::test]
async fn websocket_max_age_tightens_an_idle_socket_without_extending_its_frozen_lifespan() {
    let mut fixture = AgeFixture::new(false, Duration::from_secs(600)).await;
    age_response(&fixture.provider, ACCOUNT, "age-tightened", Map::new()).await;
    let first = fixture.upstream.request().await;
    advance_connection_age(Duration::from_secs(10)).await;
    fixture.store.set_websocket_max_age(ACCOUNT, 30_000);
    assert_eq!(
        age_response(&fixture.provider, ACCOUNT, "age-tightened", Map::new()).await,
        WebSocketPoolKind::Reuse
    );
    assert_eq!(
        fixture.upstream.request().await.connection,
        first.connection
    );
    fixture.store.set_websocket_max_age(ACCOUNT, 120_000);
    assert_eq!(
        age_response(&fixture.provider, ACCOUNT, "age-tightened", Map::new()).await,
        WebSocketPoolKind::Reuse
    );
    fixture.upstream.request().await;
    fixture.store.set_websocket_max_age(ACCOUNT, 0);
    advance_connection_age(Duration::from_secs(20)).await;
    assert_eq!(
        age_response(&fixture.provider, ACCOUNT, "age-tightened", Map::new()).await,
        WebSocketPoolKind::New
    );
    assert_ne!(
        fixture.upstream.request().await.connection,
        first.connection
    );
    fixture.pool.shutdown().await;
}

#[tokio::test]
async fn websocket_max_age_tighter_update_rejects_an_already_expired_idle_socket_before_send() {
    let mut fixture = AgeFixture::new(false, Duration::from_secs(600)).await;
    age_response(&fixture.provider, ACCOUNT, "age-borrow", Map::new()).await;
    let first = fixture.upstream.request().await;
    advance_connection_age(Duration::from_secs(40)).await;
    fixture.store.set_websocket_max_age(ACCOUNT, 30_000);
    assert_eq!(
        age_response(&fixture.provider, ACCOUNT, "age-borrow", Map::new()).await,
        WebSocketPoolKind::New
    );
    assert_ne!(
        fixture.upstream.request().await.connection,
        first.connection
    );
    fixture.pool.shutdown().await;
}

#[tokio::test]
async fn websocket_max_age_maintenance_closes_idle_but_active_stream_finishes_without_repooling() {
    let mut fixture = AgeFixture::new(false, Duration::from_secs(600)).await;
    fixture.store.set_websocket_max_age(ACCOUNT, 30_000);
    age_response(&fixture.provider, ACCOUNT, "age-idle", Map::new()).await;
    let idle = fixture.upstream.request().await;
    let provider = Arc::clone(&fixture.provider);
    let active = tokio::spawn(async move {
        age_response(
            &provider,
            ACCOUNT,
            "age-active",
            Map::from_iter([("test_hold".to_owned(), json!(true))]),
        )
        .await
    });
    let busy = fixture.upstream.request().await;
    fixture.store.set_websocket_max_age(ACCOUNT, 0);
    advance_connection_age(ACCOUNT_AGE).await;
    fixture.pool.maintain_idle_connections().await;
    assert_eq!(fixture.upstream.closed().await, idle.connection);
    assert!(!active.is_finished());
    assert!(fixture.upstream.closed.try_recv().is_err());
    fixture.upstream.release.notify_one();
    assert_eq!(active.await.unwrap(), WebSocketPoolKind::New);
    assert_eq!(fixture.upstream.closed().await, busy.connection);
    assert_expired_local_continuation(
        &fixture.provider,
        "age-active",
        &format!("resp_age_{}_1", busy.connection),
    )
    .await;
    assert!(fixture.upstream.requests.try_recv().is_err());
    assert_eq!(
        age_response(&fixture.provider, ACCOUNT, "age-active", Map::new()).await,
        WebSocketPoolKind::New
    );
    assert_ne!(fixture.upstream.request().await.connection, busy.connection);
    fixture.pool.shutdown().await;
}

#[tokio::test]
async fn websocket_max_age_expired_local_continuation_keeps_recovery_and_tombstones_before_send() {
    for maintenance in [false, true] {
        let mut fixture = AgeFixture::new(false, Duration::from_secs(600)).await;
        fixture.store.set_websocket_max_age(ACCOUNT, 30_000);
        age_response(&fixture.provider, ACCOUNT, "age-continuation", Map::new()).await;
        let first = fixture.upstream.request().await;
        let response_id = format!("resp_age_{}_1", first.connection);
        advance_connection_age(ACCOUNT_AGE).await;
        if maintenance {
            fixture.pool.maintain_idle_connections().await;
        }
        for _ in 0..2 {
            assert_expired_local_continuation(&fixture.provider, "age-continuation", &response_id)
                .await;
        }
        assert!(fixture.upstream.requests.try_recv().is_err());
        assert_eq!(fixture.upstream.accepted.load(Ordering::SeqCst), 1);
        fixture.pool.shutdown().await;
    }
}

#[tokio::test]
async fn websocket_max_age_client_body_cannot_configure_the_internal_limit() {
    let mut fixture = AgeFixture::new(false, Duration::from_secs(600)).await;
    let injected = Map::from_iter([
        ("websocket_max_age_limit".to_owned(), json!(1)),
        ("websocket_max_age_ms".to_owned(), json!(1)),
        ("websocketMaxAgeMs".to_owned(), json!(1)),
    ]);
    age_response(&fixture.provider, ACCOUNT, "age-client", injected.clone()).await;
    let first = fixture.upstream.request().await;
    advance_connection_age(ACCOUNT_AGE).await;
    assert_eq!(
        age_response(&fixture.provider, ACCOUNT, "age-client", injected.clone()).await,
        WebSocketPoolKind::Reuse
    );
    let second = fixture.upstream.request().await;
    assert_eq!(second.connection, first.connection);
    for (name, value) in injected {
        assert_eq!(second.body[&name], value);
    }
    fixture.pool.shutdown().await;
}
