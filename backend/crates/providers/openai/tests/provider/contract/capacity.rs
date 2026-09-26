use futures::FutureExt;
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

    // 子线程还没有自己的绑定，父账号只是默认偏好，不能被强制挤进父账号队列。
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
