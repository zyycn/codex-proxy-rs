//! 验证 OpenAI 请求等待账号容量与 Guardian 预留槽位

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
        account_policy().with_queue(ConcurrencyQueuePolicy {
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
async fn queued_session_sends_only_after_capacity_recovers_while_a_new_child_can_use_another_account()
 {
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
        .expect(3)
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

    // 子线程还没有自己的绑定，父账号只是默认偏好，不能被强制挤进父账号队列
    let mut child = provider
        .execute(
            planned_request("openai", operation("capacity-child")),
            queued_context("req_capacity_child"),
        )
        .await
        .unwrap();
    assert_eq!(
        child.metadata().provider_account_id().as_str(),
        "acct_subagent_b"
    );
    while let Some(event) = child.next().await {
        event.unwrap();
    }
    drop(child);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

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
        [
            "chatgpt-acct_subagent_a",
            "chatgpt-acct_subagent_b",
            "chatgpt-acct_subagent_a"
        ]
    );
    server.verify().await;
}

fn unqueued_context(request_id: &str, reserved: u32) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(5),
        account_policy().with_openai_guardian_reserved_concurrency(reserved),
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
async fn guardian_requests_can_use_the_reserved_slot_that_normal_requests_cannot() {
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
        .expect(3)
        .mount(&server)
        .await;
    // 默认账号上限为 2、预留 1：普通请求只能使用 1 个名额，Guardian 可以用满 2 个
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        server.uri(),
        leases.clone(),
    );

    let mut normal = provider
        .clone()
        .execute(
            planned_request("openai", subagent_operation(Some("collab_spawn"))),
            unqueued_context("req_guardian_normal_idle", 1),
        )
        .await
        .unwrap();
    while let Some(event) = normal.next().await {
        event.unwrap();
    }
    drop(normal);
    let limits = |leases: &TestLeaseCoordinator| {
        leases
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.max_concurrent().get())
            .collect::<Vec<_>>()
    };
    assert_eq!(limits(&leases), [1]);

    leases.signals.lock().unwrap().insert(
        account.clone(),
        AccountRuntimeSignals {
            in_flight: 1,
            last_started_at: None,
            quota_reset_at: None,
            quota_remaining_rank: None,
            cooldown: None,
            failure_rate_basis_points: None,
            first_output_latency_ms: None,
        },
    );
    assert!(
        provider
            .clone()
            .execute(
                planned_request("openai", subagent_operation(None)),
                unqueued_context("req_guardian_normal_busy", 1),
            )
            .await
            .is_err()
    );
    assert_eq!(limits(&leases), [1]);

    let mut guardian = provider
        .clone()
        .execute(
            planned_request("openai", subagent_operation(Some("guardian"))),
            unqueued_context("req_guardian_reserved", 1),
        )
        .await
        .unwrap();
    assert_eq!(guardian.metadata().provider_account_id(), &account);
    while let Some(event) = guardian.next().await {
        event.unwrap();
    }
    assert_eq!(limits(&leases), [1, 2]);
    drop(guardian);

    // 同一个 Provider 的新请求读取关闭后的策略，不需要重新初始化选择器
    let mut unreserved = provider
        .execute(
            planned_request("openai", subagent_operation(None)),
            unqueued_context("req_guardian_reservation_disabled", 0),
        )
        .await
        .unwrap();
    while let Some(event) = unreserved.next().await {
        event.unwrap();
    }
    assert_eq!(limits(&leases), [1, 2, 2]);
    server.verify().await;
}
