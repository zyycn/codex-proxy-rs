//! 验证 OpenAI 请求等待账号容量与 Guardian 独立容量池

use futures::FutureExt;
use gateway_core::account::AccountRuntimeSignals;
use gateway_core::concurrency::ConcurrencyQueuePolicy;

use super::*;

fn queued_context(request_id: &str) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(5),
        account_policy()
            .with_switch_account_on_queue_failure(true)
            .with_queue(ConcurrencyQueuePolicy {
                max_waiting: 1,
                timeout: Duration::from_secs(2),
            }),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

fn operation(thread_id: &str) -> Operation {
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("capacity queue test")),
                ("session_id".to_owned(), json!("capacity-root")),
                ("thread_id".to_owned(), json!(thread_id)),
            ]),
        )
        .unwrap()
        .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
    ))
}

#[tokio::test]
async fn queued_root_and_new_child_both_observe_the_same_account_capacity() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(2)
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        affinity,
        server.uri(),
        leases.clone(),
    );
    let mut root = provider
        .clone()
        .execute(
            planned_request("openai", operation("capacity-root")),
            queued_context("req_capacity_root"),
        )
        .await
        .unwrap();
    while let Some(event) = root.next().await {
        event.unwrap();
    }
    drop(root);

    create_account(&store, "acct_subagent_b").await;
    leases
        .busy_accounts
        .lock()
        .unwrap()
        .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
    let mut waiting = Box::pin(provider.clone().execute(
        planned_request("openai", operation("capacity-root")),
        queued_context("req_capacity_wait"),
    ));
    assert!(waiting.as_mut().now_or_never().is_none());

    // 新子线程也服从根账号队列，队列满不能选择另一账号
    let error = provider
        .execute(
            planned_request("openai", operation("capacity-child")),
            queued_context("req_capacity_child"),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::ConcurrencyQueueFull);
    assert!(error.retry_is_prohibited());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    leases.busy_accounts.lock().unwrap().clear();
    let mut resumed = waiting.await.unwrap();
    assert_eq!(
        resumed.metadata().provider_account_id().as_str(),
        "acct_subagent_a"
    );
    while let Some(event) = resumed.next().await {
        event.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    let accounts = requests
        .iter()
        .map(|request| request.headers["chatgpt-account-id"].to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        accounts,
        ["chatgpt-acct_subagent_a", "chatgpt-acct_subagent_a"]
    );
    server.verify().await;
}

fn capacity_context(
    request_id: &str,
    reserved: u32,
    queue: ConcurrencyQueuePolicy,
) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(5),
        account_policy()
            .with_openai_guardian_reserved_concurrency(reserved)
            .with_queue(queue),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

fn subagent_operation(subagent: Option<&str>) -> Operation {
    let mut body = Map::from_iter([
        ("model".to_owned(), json!("gpt-5.4")),
        ("input".to_owned(), json!("guardian reservation test")),
    ]);
    if let Some(subagent) = subagent {
        body.insert(
            "client_metadata".to_owned(),
            json!({"x-openai-subagent": subagent}),
        );
    }
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body)
            .unwrap()
            .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
    ))
}

#[tokio::test]
async fn guardian_and_normal_requests_use_independent_capacity_and_observations() {
    use gateway_core::provider_ports::ProviderConcurrencyPool::{Reserved, Shared};

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_guardian").await;
    let account = ProviderAccountId::new("acct_guardian").unwrap();
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(4)
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        server.uri(),
        leases.clone(),
    );
    let signals = |in_flight| AccountRuntimeSignals {
        in_flight,
        last_started_at: None,
        quota_reset_at: None,
        quota_reset_windows: None,
        quota_remaining_rank: None,
        cooldown: None,
        failure_rate_basis_points: None,
        first_output_latency_ms: None,
    };
    // 普通上限为 2，审批额度独立取配置值；关闭后新审批请求回到普通池
    for (id, normal_count, reserved_count, reserve, subagent, expected) in [
        (
            "req_large_reserve_normal",
            1,
            10,
            10,
            None,
            Some((Shared, 2)),
        ),
        ("req_normal_full", 2, 0, 1, None, None),
        (
            "req_guardian_independent",
            2,
            0,
            1,
            Some("guardian"),
            Some((Reserved, 1)),
        ),
        ("req_guardian_full", 0, 1, 1, Some("guardian"), None),
        (
            "req_normal_independent",
            1,
            1,
            1,
            Some("collab_spawn"),
            Some((Shared, 2)),
        ),
        (
            "req_guardian_disabled",
            1,
            1,
            0,
            Some("guardian"),
            Some((Shared, 2)),
        ),
    ] {
        leases
            .signals
            .lock()
            .unwrap()
            .insert(account.clone(), signals(normal_count));
        leases
            .reserved_signals
            .lock()
            .unwrap()
            .insert(account.clone(), signals(reserved_count));
        leases.requests.lock().unwrap().clear();
        let result = provider
            .clone()
            .execute(
                planned_request("openai", subagent_operation(subagent)),
                capacity_context(id, reserve, ConcurrencyQueuePolicy::default()),
            )
            .await;
        let Some((pool, limit)) = expected else {
            assert!(result.is_err(), "{id} should wait for its own capacity");
            assert!(leases.requests.lock().unwrap().is_empty());
            continue;
        };
        let mut stream = result.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let capacity = stream
            .metadata()
            .selection_observation()
            .unwrap()
            .capacity()
            .unwrap();
        let count = if pool == Shared {
            normal_count
        } else {
            reserved_count
        };
        assert_eq!(
            (capacity.used_slots(), capacity.total_slots()),
            (u64::from(count) + 1, limit)
        );
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        drop(stream);
        let requests = leases.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].concurrency_pool(), pool);
        assert_eq!(u64::from(requests[0].max_concurrent().get()), limit);
    }
    server.verify().await;
}

#[tokio::test]
async fn a_full_capacity_pool_does_not_block_the_other_pools_queue() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_guardian").await;
    let account = ProviderAccountId::new("acct_guardian").unwrap();
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(4)
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        server.uri(),
        leases.clone(),
    );
    let queue = ConcurrencyQueuePolicy {
        max_waiting: 1,
        timeout: Duration::from_secs(2),
    };
    for guardian_waits in [false, true] {
        let blocked_signals = if guardian_waits {
            &leases.reserved_signals
        } else {
            &leases.signals
        };
        blocked_signals.lock().unwrap().insert(
            account.clone(),
            AccountRuntimeSignals {
                in_flight: if guardian_waits { 1 } else { 2 },
                last_started_at: None,
                quota_reset_at: None,
                quota_reset_windows: None,
                quota_remaining_rank: None,
                cooldown: None,
                failure_rate_basis_points: None,
                first_output_latency_ms: None,
            },
        );
        let mut waiting = Box::pin(provider.clone().execute(
            planned_request(
                "openai",
                subagent_operation(guardian_waits.then_some("guardian")),
            ),
            capacity_context("req_pool_waiter", 1, queue),
        ));
        assert!(waiting.as_mut().now_or_never().is_none());

        let mut available = provider
            .clone()
            .execute(
                planned_request(
                    "openai",
                    subagent_operation((!guardian_waits).then_some("guardian")),
                ),
                capacity_context("req_other_pool", 1, queue),
            )
            .await
            .expect("the other pool has independent capacity and queue position");
        while let Some(event) = available.next().await {
            event.unwrap();
        }
        drop(available);

        blocked_signals.lock().unwrap().clear();
        let mut resumed = waiting.await.expect("own pool capacity was released");
        while let Some(event) = resumed.next().await {
            event.unwrap();
        }
        drop(resumed);
    }
    server.verify().await;
}
