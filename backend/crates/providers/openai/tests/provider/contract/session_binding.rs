//! 验证会话主账号偏好、严格绑定、最终请求身份与轮次关联

use super::*;

#[tokio::test]
async fn cyber_policy_session_exclusion_has_actionable_non_retryable_error() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"code":"cyber_policy","type":"invalid_request_error","message":"synthetic policy rejection"}
        })))
        .mount(&server).await;
    let provider = provider_with_affinity_and_base_url(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        server.uri(),
    );
    let request = || planned_request("openai", turn_request("policy-session", "policy-turn"));
    let mut stream = provider
        .clone()
        .execute(
            request(),
            context("req_policy_first", CancellationToken::new()),
        )
        .await
        .unwrap();
    let first_error = loop {
        match stream.next().await {
            Some(Err(error)) => break error,
            Some(Ok(_)) => {}
            None => panic!("upstream rejection must surface"),
        }
    };
    assert_eq!(first_error.upstream_status(), Some(400));
    assert_eq!(
        first_error
            .client_visible_upstream_error()
            .unwrap()
            .message(),
        "synthetic policy rejection"
    );
    assert!(
        first_error
            .error_details()
            .unwrap()
            .contains("synthetic policy rejection")
    );
    let error = provider
        .clone()
        .execute(
            request(),
            context("req_policy_retry", CancellationToken::new()),
        )
        .await
        .err()
        .expect("excluded session must fail before upstream");
    assert_eq!(error.kind(), ProviderErrorKind::SessionPolicyRestricted);
    assert!(error.retry_is_prohibited());
    let visible = error
        .client_visible_upstream_error()
        .expect("actionable client message");
    assert_eq!(visible.code(), Some("cyber_policy"));
    assert!(visible.message().contains("Do not repeatedly retry"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    // 可见但不属于固定账号范围的候选不能掩盖会话限制
    create_account(&store, "acct_subagent_b").await;
    let pinned = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_policy_pinned").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_request_location(Some(global_request_location())),
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(5),
        account_policy(),
        AccountAttemptContext::new(
            BTreeSet::new(),
            Some(ProviderAccountId::new("acct_subagent_a").unwrap()),
            None,
        )
        .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    );
    let error = provider
        .clone()
        .execute(request(), pinned)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::SessionPolicyRestricted);
    assert!(error.retry_is_prohibited());
    assert_eq!(
        error.client_visible_upstream_error().unwrap().code(),
        Some("cyber_policy")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    // 其他账号仍可用于该会话时，不能误报全部账号受限
    let available = provider
        .clone()
        .execute(
            request(),
            context("req_policy_available", CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(
        available.metadata().provider_account_id().as_str(),
        "acct_subagent_b"
    );
    drop(available);
    // 会话限制不能影响同一密钥的其他会话
    store
        .set_enabled(&ProviderAccountId::new("acct_subagent_b").unwrap(), false)
        .await
        .unwrap();
    drop(
        provider
            .execute(
                planned_request("openai", turn_request("unrelated-session", "other-turn")),
                context("req_other_session", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
}

#[derive(Debug)]
struct ChangeTurnMetadata(&'static [u8]);

impl MiddlewarePlan for ChangeTurnMetadata {
    fn handle(
        &self,
        _context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        let metadata = self.0;
        Box::pin(async move {
            let protocol = request.protocol().to_owned();
            let mut headers = request.headers().to_vec();
            let body = request.body().clone();
            headers.push(MiddlewareHeader::new(
                "x-codex-turn-metadata",
                Bytes::from_static(metadata),
            ));
            next.run(request.replace_parts(protocol, headers, body, None)?)
                .await
        })
    }
}

#[tokio::test]
async fn final_middleware_session_header_cannot_use_another_sessions_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url(&store, affinity, server.uri());
    let request = |session| {
        planned_request(
            "openai",
            Operation::Generate(generate_with_session_context(session, None, None)),
        )
    };
    drop(
        provider
            .clone()
            .execute(
                request("session-a"),
                context("req_seed_a", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    store.set_scheduling("acct_subagent_b", None, AccountWeight::new(100).unwrap());
    drop(
        provider
            .clone()
            .execute(
                request("session-b"),
                context("req_seed_b", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
    let error = provider
        .execute(
            request("session-a"),
            context_with_middleware(
                "req_final_identity",
                Arc::new(ChangeTurnMetadata(br#"{"session_id":"session-b"}"#)),
                FastMode::Default,
            ),
        )
        .await
        .err()
        .expect("mismatched owner must fail before send");
    assert!(error.retry_is_prohibited());
    assert!(server.received_requests().await.unwrap().is_empty());
}

pub(super) fn turn_request(session: &str, turn: &str) -> Operation {
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", json!({
            "model":"gpt-5.4", "input":"hello",
            "client_metadata":{"x-codex-turn-metadata":json!({"session_id":session,"thread_id":session,"turn_id":turn}).to_string()}
        }).as_object().unwrap().clone()).unwrap()
        .with_context(Map::from_iter([("use_websocket".into(), json!(false))])),
    ))
}

fn image_for_turn(kind: ImageRequestKind, turn: &str) -> Operation {
    Operation::GenerateImage(ImageRequest::from_raw_json(
        kind,
        RawJsonPayload::new(
            "openai",
            Bytes::from_static(br#"{"prompt":"a square","model":"gpt-image-1"}"#),
        )
        .unwrap()
        .with_context(Map::from_iter([("image_turn_id".into(), json!(turn))])),
    ))
}

#[tokio::test]
async fn independent_requests_claim_and_reuse_their_session_account() {
    for source in [
        None,
        Some("memory_consolidation"),
        Some("future_background_task"),
    ] {
        assert_independent_session_can_claim(source).await;
    }
}

async fn assert_independent_session_can_claim(source: Option<&str>) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!("event: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_scope_capture\",\"status\":\"in_progress\"}}}}\n\n{CAPTURE_COMPLETED_SSE}"),
                "text/event-stream",
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url(&store, affinity.clone(), server.uri());

    // 独立后台任务无需另一根线程先发请求，预热与正式请求共用自身会话绑定
    for prewarm in [true, false] {
        let metadata = json!({
            "session_id": "independent-session",
            "thread_id": "independent-worker",
            "thread_source": source,
            "request_kind": if prewarm { "prewarm" } else { "turn" },
            "turn_id": "independent-turn"
        });
        let mut body = json!({
            "model": "gpt-5.4",
            "input": "run background task",
            "client_metadata": {
                "session_id": "independent-session",
                "thread_id": "independent-worker",
                "x-openai-subagent": source,
                "x-codex-turn-metadata": metadata.to_string()
            }
        });
        if prewarm {
            body["generate"] = json!(false);
        }
        let payload = ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([("use_websocket".into(), json!(prewarm))]));
        let mut stream = timeout(
            Duration::from_secs(1),
            provider.clone().execute(
                planned_request(
                    "openai",
                    Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
                ),
                context(
                    if prewarm {
                        "req_independent_prewarm"
                    } else {
                        "req_independent_turn"
                    },
                    CancellationToken::new(),
                ),
            ),
        )
        .await
        .expect("independent request must not wait for a nonexistent root")
        .expect("independent request obtains an account");
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_subagent_a"
        );
        assert_eq!(affinity.binding_count(), 1);
        if prewarm {
            drop(stream);
            create_account(&store, "acct_subagent_b").await;
            store.set_scheduling("acct_subagent_b", None, AccountWeight::new(100).unwrap());
        } else {
            let mut completed = false;
            while let Some(event) = stream.next().await {
                completed |= event
                    .unwrap()
                    .canonical_facts()
                    .iter()
                    .any(|event| matches!(event, GatewayEvent::Completed(_)));
            }
            assert!(
                completed,
                "independent turn must reach the upstream and complete"
            );
        }
    }
}

#[tokio::test]
async fn concurrent_root_and_child_requests_claim_one_session_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    create_account(&store, "acct_subagent_b").await;
    let affinity = Arc::new(MemorySessionAffinity::with_initial_claim_barrier(2));
    let provider = provider_with_affinity(&store, affinity.clone());
    let select = |thread| {
        provider.clone().execute(
            planned_request(
                "openai",
                Operation::Generate(generate_with_session_context(
                    "req_root",
                    Some(thread),
                    Some(if thread == "req_root" {
                        "{}"
                    } else {
                        r#"{"subagent_kind":"thread_spawn"}"#
                    }),
                )),
            ),
            context(thread, CancellationToken::new()),
        )
    };
    let (first, second) = timeout(Duration::from_secs(2), async {
        tokio::join!(select("req_root"), select("req_child"))
    })
    .await
    .expect("concurrent claims settle without waiting for a parent");
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(
        first.metadata().provider_account_id(),
        second.metadata().provider_account_id()
    );
    assert_eq!(affinity.binding_count(), 1);
}

#[tokio::test]
async fn parent_metadata_without_a_session_does_not_invent_a_binding() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = provider_with_affinity(&store, affinity.clone());
    let generate = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({
                "model":"gpt-5.4", "input":"hello",
                "client_metadata":{"x-codex-turn-metadata":json!({
                    "thread_id":"child", "parent_thread_id":"parent", "subagent_kind":"thread_spawn"
                }).to_string()}
            })
            .as_object()
            .unwrap()
            .clone(),
        )
        .unwrap(),
    );
    let stream = timeout(
        Duration::from_secs(1),
        provider.execute(
            planned_request("openai", Operation::Generate(generate)),
            context("req_no_session", CancellationToken::new()),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_subagent_a"
    );
    assert_eq!(affinity.binding_count(), 0);
}

#[tokio::test]
async fn final_middleware_child_identity_can_claim_a_missing_binding() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url(&store, affinity.clone(), server.uri());
    let stream = provider.execute(
        planned_request("openai", generate_operation()),
        context_with_middleware(
            "req_final_child_claim",
            Arc::new(ChangeTurnMetadata(br#"{"session_id":"new-session","thread_id":"child","subagent_kind":"thread_spawn"}"#)),
            FastMode::Default,
        ),
    ).await.expect("final child identity can atomically claim its session");
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_subagent_a"
    );
    assert_eq!(affinity.binding_count(), 1);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn official_image_turn_uses_current_session_owner_even_after_migration() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url(&store, affinity.clone(), server.uri());
    drop(
        provider
            .clone()
            .execute(
                planned_request("openai", turn_request("root", "known-turn")),
                context("req_seed_turn", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    store.set_scheduling("acct_subagent_b", None, AccountWeight::new(100).unwrap());
    for expected in ["acct_subagent_a", "acct_subagent_b"] {
        if expected == "acct_subagent_b" {
            let current = store.account("acct_subagent_a").unwrap();
            store.set_enabled(current.id(), false).await.unwrap();
            drop(
                provider
                    .clone()
                    .execute(
                        planned_request("openai", turn_request("root", "next-turn")),
                        context("req_migrate_turn", CancellationToken::new()),
                    )
                    .await
                    .unwrap(),
            );
        }
        for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
            let stream = provider
                .clone()
                .execute(
                    planned_provider_endpoint_request("openai", image_for_turn(kind, "known-turn")),
                    context("req_known_image_turn", CancellationToken::new()),
                )
                .await
                .unwrap();
            assert_eq!(stream.metadata().provider_account_id().as_str(), expected);
        }
        assert_eq!(affinity.binding_count(), 1);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unknown_image_turn_does_not_infer_a_conversation() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = provider_with_affinity(&store, affinity.clone());
    drop(
        provider
            .execute(
                planned_provider_endpoint_request(
                    "openai",
                    image_for_turn(ImageRequestKind::Generation, "unknown-turn"),
                ),
                context("req_unknown_image_turn", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
    assert_eq!(affinity.binding_count(), 0);
}

#[tokio::test]
async fn child_claims_before_root_and_then_follows_the_bound_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        affinity.clone(),
        server.uri(),
        leases.clone(),
    );
    let child = || {
        planned_request(
            "openai",
            Operation::Generate(generate_with_session_context(
                "root",
                Some("child"),
                Some(r#"{"subagent_kind":"thread_spawn"}"#),
            )),
        )
    };
    let stream = timeout(
        Duration::from_secs(1),
        provider.clone().execute(
            child(),
            context("req_child_first", CancellationToken::new()),
        ),
    )
    .await
    .expect("child can claim before the root arrives")
    .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_subagent_a"
    );
    drop(stream);
    assert_eq!(affinity.binding_count(), 1);
    create_account(&store, "acct_subagent_b").await;
    store.set_scheduling("acct_subagent_b", None, AccountWeight::new(100).unwrap());
    let root = provider
        .clone()
        .execute(
            planned_request("openai", turn_request("root", "root-turn")),
            context("req_root_after_child", CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(
        root.metadata().provider_account_id().as_str(),
        "acct_subagent_a"
    );
    drop(root);

    // 子线程可首次认领，但已有绑定后不能因为主账号繁忙而自行迁移
    leases
        .busy_accounts
        .lock()
        .unwrap()
        .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
    let cancel = CancellationToken::new();
    let mut pending = Box::pin(
        provider
            .clone()
            .execute(child(), context("req_child_wait", cancel.clone())),
    );
    assert!(
        timeout(Duration::from_millis(150), pending.as_mut())
            .await
            .is_err()
    );
    cancel.cancel();
    let error = pending.await.err().expect("cancelled child");
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert!(error.retry_is_prohibited());
    let mut pending = Box::pin(provider.clone().execute(
        child(),
        context("req_child_wait_again", CancellationToken::new()),
    ));
    assert!(
        timeout(Duration::from_millis(150), pending.as_mut())
            .await
            .is_err()
    );
    let root = provider
        .clone()
        .execute(
            planned_request("openai", turn_request("root", "root-migrate")),
            context("req_root_migrate", CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(
        root.metadata().provider_account_id().as_str(),
        "acct_subagent_b"
    );
    drop(root);
    let resumed = timeout(Duration::from_secs(2), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        resumed.metadata().provider_account_id().as_str(),
        "acct_subagent_b"
    );
    assert_eq!(affinity.binding_count(), 1);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn descendant_images_wait_for_the_owner_even_when_ordinary_queues_are_disabled() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        affinity.clone(),
        server.uri(),
        leases.clone(),
    );
    drop(
        provider
            .clone()
            .execute(
                planned_request("openai", turn_request("root", "root-turn")),
                context("req_seed_root", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
    let child_turn = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", json!({"model":"gpt-5.4","input":"child","client_metadata":{"session_id":"root","thread_id":"child","turn_id":"child-turn"}}).as_object().unwrap().clone()).unwrap()
    ));
    drop(
        provider
            .clone()
            .execute(
                planned_request("openai", child_turn),
                context("req_seed_child", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    leases
        .busy_accounts
        .lock()
        .unwrap()
        .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
    for (kind, turn, explicit_child) in [
        (ImageRequestKind::Generation, "child-turn", false),
        (ImageRequestKind::Edit, "child-turn", false),
        (ImageRequestKind::Generation, "root-turn", true),
        (ImageRequestKind::Edit, "root-turn", true),
    ] {
        let image = if explicit_child {
            Operation::GenerateImage(ImageRequest::from_raw_json(
                kind,
                RawJsonPayload::new(
                    "openai",
                    Bytes::from_static(
                        br#"{"prompt":"a square","model":"gpt-image-1","session_id":"root"}"#,
                    ),
                )
                .unwrap()
                .with_context(Map::from_iter([
                    ("image_turn_id".into(), json!(turn)),
                    (
                        "turn_metadata".into(),
                        json!(json!({"session_id":"root","thread_id":"child"}).to_string()),
                    ),
                ])),
            ))
        } else {
            image_for_turn(kind, turn)
        };
        let mut pending = Box::pin(provider.clone().execute(
            planned_provider_endpoint_request("openai", image),
            context("req_child_image_wait", CancellationToken::new()),
        ));
        assert!(
            timeout(Duration::from_millis(150), pending.as_mut())
                .await
                .is_err()
        );
        assert!(
            leases
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.account_id().as_str() == "acct_subagent_a")
        );
        assert!(server.received_requests().await.unwrap().is_empty());
        leases.busy_accounts.lock().unwrap().clear();
        let stream = timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_subagent_a"
        );
        drop(stream);
        leases
            .busy_accounts
            .lock()
            .unwrap()
            .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
    }
}

#[tokio::test]
async fn child_queue_timeout_does_not_rebind_or_allow_provider_fallback() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        affinity.clone(),
        server.uri(),
        leases.clone(),
    );
    drop(
        provider
            .clone()
            .execute(
                planned_request("openai", turn_request("root", "root-turn")),
                context("req_timeout_seed_root", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    leases
        .busy_accounts
        .lock()
        .unwrap()
        .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
    let attempt = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_child_timeout").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(5),
        account_policy().with_queue(gateway_core::concurrency::ConcurrencyQueuePolicy {
            max_waiting: 1,
            timeout: Duration::from_millis(120),
        }),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    );
    let error = provider
        .execute(
            planned_request(
                "openai",
                Operation::Generate(generate_with_session_context("root", Some("child"), None)),
            ),
            attempt,
        )
        .await
        .err()
        .expect("queue timeout");
    assert_eq!(error.kind(), ProviderErrorKind::ConcurrencyQueueTimeout);
    assert!(error.retry_is_prohibited());
    assert_eq!(affinity.binding_count(), 1);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[derive(Debug)]
struct SessionScheduler {
    decision: gateway_core::engine::policy::AccountScheduleDecision,
}
impl gateway_core::engine::policy::RequestPolicyPlan for SessionScheduler {
    fn route_model(
        &self,
        _: gateway_core::engine::policy::ModelRouteInput,
    ) -> BoxFuture<
        'static,
        Result<
            gateway_core::engine::policy::ModelRouteDecision,
            gateway_core::engine::policy::RequestPolicyFault,
        >,
    > {
        Box::pin(async { Ok(gateway_core::engine::policy::ModelRouteDecision::Unhandled) })
    }
    fn schedule_account(
        &self,
        input: gateway_core::engine::policy::AccountScheduleInput,
    ) -> BoxFuture<
        'static,
        Result<
            gateway_core::engine::policy::AccountScheduleDecision,
            gateway_core::engine::policy::RequestPolicyFault,
        >,
    > {
        assert!(
            input
                .candidates()
                .iter()
                .any(|candidate| candidate.account_id().as_str() == "acct_subagent_b")
        );
        let decision = self.decision.clone();
        Box::pin(async move { Ok(decision) })
    }
}
struct SessionExtensionLease;
impl gateway_core::routing::extensions::ExtensionSetLease for SessionExtensionLease {
    fn is_ready(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn child_binding_respects_filtered_delegation_and_explicit_plugin_choices() {
    use gateway_core::account::AccountAffinity;
    use gateway_core::engine::policy::RequestPolicyContext;
    use gateway_core::routing::extensions::{ExtensionSetId, ExtensionSetReference};
    for (choice, mode) in [
        ("delegate", AccountAffinity::Strict),
        ("pick", AccountAffinity::Strict),
        ("filter", AccountAffinity::Strict),
        ("filter", AccountAffinity::Preferred),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_subagent_a").await;
        let affinity = Arc::new(MemorySessionAffinity::default());
        let leases = Arc::new(TestLeaseCoordinator::default());
        let server = MockServer::start().await;
        let provider = provider_with_affinity_and_base_url_and_leases(
            &store,
            affinity,
            server.uri(),
            leases.clone(),
        );
        drop(
            provider
                .clone()
                .execute(
                    planned_request("openai", turn_request("root", "root-turn")),
                    context("req_plugin_root", CancellationToken::new()),
                )
                .await
                .unwrap(),
        );
        create_account(&store, "acct_subagent_b").await;
        leases
            .busy_accounts
            .lock()
            .unwrap()
            .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
        let id = ModelRequestId::new("req_plugin_child").unwrap();
        let key = ClientApiKeyId::new("key_openai_contract").unwrap();
        let policy = RequestPolicyContext::new(
            Arc::new(SessionScheduler {
                decision: match choice {
                    "pick" => gateway_core::engine::policy::AccountScheduleDecision::Pick(
                        ProviderAccountId::new("acct_subagent_b").unwrap(),
                    ),
                    "filter" => {
                        gateway_core::engine::policy::AccountScheduleDecision::DelegateFiltered(
                            BTreeSet::from([ProviderAccountId::new("acct_subagent_a").unwrap()]),
                        )
                    }
                    _ => gateway_core::engine::policy::AccountScheduleDecision::Delegate,
                },
            }),
            ExtensionSetReference::new(
                ExtensionSetId::new("session-choice".into()).unwrap(),
                Arc::new(SessionExtensionLease),
            ),
            id.clone(),
            key.clone(),
            vec![],
        );
        let attempt = AttemptContext::new(
            RequestAttemptContext::new(id, key).with_request_policy(Some(policy)),
            NonZeroU32::new(1).unwrap(),
            SystemTime::now() + Duration::from_secs(5),
            account_policy().with_openai_account_affinity(mode),
            AccountAttemptContext::new(BTreeSet::new(), None, None)
                .with_account_scope(contract_account_scope()),
            None,
            CancellationToken::new(),
        );
        let mut pending = Box::pin(provider.clone().execute(
            planned_request(
                "openai",
                Operation::Generate(generate_with_session_context("root", Some("child"), None)),
            ),
            attempt,
        ));
        if choice == "filter" && mode == AccountAffinity::Strict {
            let error = timeout(Duration::from_secs(2), pending)
                .await
                .unwrap()
                .err()
                .expect("excluded child owner must be terminal");
            assert_eq!(error.kind(), ProviderErrorKind::RequestPolicyDenied);
            assert_eq!(
                error.send_state(),
                gateway_core::upstream::UpstreamSendState::NotSent
            );
        } else if choice == "pick" || choice == "filter" {
            let stream = timeout(Duration::from_secs(2), pending)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                "acct_subagent_b"
            );
        } else {
            assert!(
                timeout(Duration::from_millis(150), pending.as_mut())
                    .await
                    .is_err()
            );
            leases.busy_accounts.lock().unwrap().clear();
            let stream = timeout(Duration::from_secs(2), pending)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                "acct_subagent_a"
            );
        }
    }
}

#[tokio::test]
async fn connection_local_replay_does_not_claim_or_migrate_a_session() {
    use gateway_core::engine::continuation::NativeContinuationScope;

    for existing_binding in [true, false] {
        for scope_from_state in [false, true] {
            for recovery in [
                ContinuationAttempt::ReplayOwner,
                ContinuationAttempt::ReplayAny,
            ] {
                let store = Arc::new(MemoryAccountStore::default());
                create_account(&store, "acct_subagent_a").await;
                let affinity = Arc::new(MemorySessionAffinity::default());
                let leases = Arc::new(TestLeaseCoordinator::default());
                let server = MockServer::start().await;
                let provider = provider_with_affinity_and_base_url_and_leases(
                    &store,
                    affinity.clone(),
                    server.uri(),
                    leases.clone(),
                );
                if existing_binding {
                    drop(
                        provider
                            .clone()
                            .execute(
                                planned_request("openai", turn_request("root", "seed-turn")),
                                context("req_seed_owner", CancellationToken::new()),
                            )
                            .await
                            .unwrap(),
                    );
                }
                create_account(&store, "acct_subagent_b").await;
                let renewals = affinity.renewal_ttls();
                let aliases = affinity.alias_ttls();
                leases.requests.lock().unwrap().clear();

                let provider_kind = ProviderKind::new("openai").unwrap();
                let account = ProviderAccountId::new("acct_subagent_a").unwrap();
                let client = ClientApiKeyId::new("key_openai_contract").unwrap();
                let owner = ProviderAccountStateOwner::new(provider_kind.clone(), account.clone());
                let pin = NativeContinuationPin::new(
                    PreviousResponseId::new("client-previous"),
                    PreviousResponseId::new("upstream-previous"),
                    client.clone(),
                    provider_kind,
                    account.clone(),
                )
                .with_scope(if scope_from_state {
                    NativeContinuationScope::Persisted
                } else {
                    NativeContinuationScope::ConnectionLocal
                });
                // 覆盖 Coordinator 排除旧 owner 后改走 ReplayAny 的入口
                let excluded = if recovery == ContinuationAttempt::ReplayAny {
                    BTreeSet::from([account])
                } else {
                    BTreeSet::new()
                };
                let attempt = AttemptContext::new(
                    RequestAttemptContext::new(
                        ModelRequestId::new("req_invalid_replay").unwrap(),
                        client,
                    ),
                    NonZeroU32::new(2).unwrap(),
                    SystemTime::now() + Duration::from_secs(5),
                    account_policy(),
                    AccountAttemptContext::new(excluded, None, Some(owner))
                        .with_account_scope(contract_account_scope()),
                    Some(ContinuationBinding::Pinned(pin)),
                    CancellationToken::new(),
                )
                .with_continuation_attempt(recovery);
                let mut generate = generate_with_session_context("root", Some("root"), None);
                if scope_from_state {
                    // Provider 保存的状态比通用 pin 更精确，不能只检查 pin 的作用域
                    generate = generate.with_provider_session_state(ProviderSessionState::new(
                        "openai", json!({
                            "account_id":"acct_subagent_a", "conversation_id":"conversation-root",
                            "continuation_scope":"connection_local"
                        }).as_object().unwrap().clone(),
                    ).unwrap());
                }
                let error = provider
                    .clone()
                    .execute(
                        planned_request("openai", Operation::Generate(generate)),
                        attempt,
                    )
                    .await
                    .err()
                    .expect("connection-local replay requires client recovery");
                assert_eq!(
                    error.kind(),
                    ProviderErrorKind::ContinuationRecoveryRequired
                );
                assert_eq!(error.send_state(), UpstreamSendState::NotSent);
                assert_eq!(
                    error.continuation_recovery_disposition(),
                    Some(ContinuationRecoveryDisposition::ClientReplayRequired)
                );
                assert_eq!(
                    affinity.renewal_ttls(),
                    renewals,
                    "rejected replay must not write a binding"
                );
                assert_eq!(affinity.alias_ttls(), aliases);
                assert_eq!(affinity.binding_count(), usize::from(existing_binding));
                assert!(
                    leases.requests.lock().unwrap().is_empty(),
                    "rejected replay must not acquire a lease"
                );
                if existing_binding {
                    let child = provider
                        .clone()
                        .execute(
                            planned_request(
                                "openai",
                                Operation::Generate(generate_with_session_context(
                                    "root",
                                    Some("child"),
                                    None,
                                )),
                            ),
                            context("req_child_after_rejected_replay", CancellationToken::new()),
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        child.metadata().provider_account_id().as_str(),
                        "acct_subagent_a"
                    );
                }
                assert!(server.received_requests().await.unwrap().is_empty());
            }
        }
    }
}

#[tokio::test]
async fn old_native_continuation_cannot_restore_the_pre_migration_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        affinity,
        server.uri(),
        leases.clone(),
    );
    let root = || planned_request("openai", turn_request("root", "native-root-turn"));
    drop(
        provider
            .clone()
            .execute(root(), context("req_native_root", CancellationToken::new()))
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    leases
        .busy_accounts
        .lock()
        .unwrap()
        .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
    drop(
        provider
            .clone()
            .execute(
                root(),
                context("req_native_migration", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
    leases.busy_accounts.lock().unwrap().clear();
    let error = provider
        .clone()
        .execute(
            planned_request(
                "openai",
                Operation::Generate(generate_with_session_context("root", Some("child"), None)),
            ),
            pinned_continuation_context(
                "req_old_native_state",
                "acct_subagent_a",
                "resp_old",
                "resp_old",
                1,
                ContinuationAttempt::Native,
            ),
        )
        .await
        .err()
        .expect("old native state requires full replay");
    assert_eq!(
        error.kind(),
        ProviderErrorKind::ContinuationRecoveryRequired
    );
    let stream = provider
        .execute(
            root(),
            context("req_native_still_current", CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_subagent_b"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

fn preferred_context(request_id: &str, scope: Arc<FrozenAccountScope>) -> AttemptContext {
    preferred_context_with_ttl(request_id, scope, Duration::from_secs(24 * 3600))
}

fn preferred_context_with_ttl(
    request_id: &str,
    scope: Arc<FrozenAccountScope>,
    ttl: Duration,
) -> AttemptContext {
    preferred_context_with_policy(
        request_id,
        scope,
        account_policy()
            .with_openai_account_affinity(gateway_core::account::AccountAffinity::Preferred)
            .with_openai_session_affinity_ttl(ttl),
    )
}

fn preferred_context_with_policy(
    request_id: &str,
    scope: Arc<FrozenAccountScope>,
    policy: gateway_core::account::AccountSelectionPolicy,
) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        ),
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(5),
        policy,
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn relaxed_and_preferred_session_requests_apply_distinct_scheduling_policies() {
    use gateway_core::account::{AccountAffinity, SmartSchedulingConfig};
    use gateway_core::concurrency::ConcurrencyQueuePolicy;
    use gateway_core::operation::{
        ProviderHttpHeader, ProviderHttpMethod, ProviderHttpRequest, RawHttpPayload,
    };

    for (mode, max_waiting) in [
        (AccountAffinity::Relaxed, 0),
        (AccountAffinity::Relaxed, 10),
        (AccountAffinity::Preferred, 0),
        (AccountAffinity::Preferred, 10),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_subagent_a").await;
        let affinity = Arc::new(MemorySessionAffinity::default());
        let leases = Arc::new(TestLeaseCoordinator::default());
        let server = MockServer::start().await;
        let provider = provider_with_affinity_and_base_url_and_leases(
            &store,
            affinity.clone(),
            server.uri(),
            leases.clone(),
        );
        let policy = account_policy()
            .with_openai_account_affinity(mode)
            .with_smart_scheduling(SmartSchedulingConfig::new([1.0; 6]).unwrap())
            .with_queue(ConcurrencyQueuePolicy {
                max_waiting,
                timeout: Duration::from_secs(1),
            });
        let select = |operation| {
            let request = if matches!(operation, Operation::Generate(_)) {
                planned_request("openai", operation)
            } else if matches!(operation, Operation::ProviderHttp(_)) {
                let provider_kind = ProviderKind::new("openai").unwrap();
                let snapshot = RuntimeSnapshot::new(
                    ConfigRevision::new(1).unwrap(),
                    SettingsValues::new(2, "smart", BTreeMap::new(), None, None),
                    vec![provider_kind.clone()],
                    Vec::new(),
                    Vec::new(),
                )
                .unwrap();
                let plan = snapshot
                    .plan_provider_endpoint(
                        &provider_kind,
                        Some(&UpstreamModelId::new("gpt-live-1-codex").unwrap()),
                        &operation,
                        contract_account_scope(),
                        &RoutingContext::default(),
                    )
                    .unwrap();
                ProviderRequest::new(operation, plan.candidates()[0].clone())
            } else {
                planned_provider_endpoint_request("openai", operation)
            };
            provider.clone().execute(
                request,
                preferred_context_with_policy(
                    "req_preferred_session",
                    contract_account_scope(),
                    policy,
                ),
            )
        };
        drop(select(turn_request("root", "shared-turn")).await.unwrap());
        create_account(&store, "acct_subagent_b").await;
        store.set_scheduling("acct_subagent_b", None, AccountWeight::new(100).unwrap());
        // 后备池中另有低权重账号，验证分流继续遵循配置的调度策略
        create_account(&store, "acct_provider_contract").await;
        for operation in [
            turn_request("root", "next-turn"),
            Operation::Generate(generate_with_session_context("root", Some("child"), None)),
            Operation::Generate(generate_with_session_context("root", Some("sibling"), None)),
            image_for_turn(ImageRequestKind::Generation, "shared-turn"),
            image_for_turn(ImageRequestKind::Edit, "shared-turn"),
            Operation::ProviderHttp(
                ProviderHttpRequest::new(
                    "realtime-calls",
                    ProviderHttpMethod::Post,
                    None,
                    vec![
                        ProviderHttpHeader::new("session-id", Bytes::from_static(b"root")),
                        ProviderHttpHeader::new("thread-id", Bytes::from_static(b"live-child")),
                    ],
                    RawHttpPayload::new(
                        "openai",
                        Bytes::from_static(
                            br#"{"sdp":"v=0","session":{"model":"gpt-live-1-codex"}}"#,
                        ),
                    )
                    .unwrap(),
                )
                .unwrap(),
            ),
            Operation::Search(StandaloneSearchRequest::from_raw_json(
                RawJsonPayload::new(
                    "openai",
                    Bytes::from_static(br#"{"id":"root","commands":{"search_query":[]}}"#),
                )
                .unwrap(),
            )),
        ] {
            for busy in [false, true, false] {
                if busy {
                    leases
                        .busy_accounts
                        .lock()
                        .unwrap()
                        .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
                } else {
                    leases.busy_accounts.lock().unwrap().clear();
                }
                let stream =
                    tokio::time::timeout(Duration::from_millis(500), select(operation.clone()))
                        .await
                        .expect("an idle fallback must be admitted without waiting for the primary")
                        .unwrap();
                assert_eq!(
                    stream.metadata().provider_account_id().as_str(),
                    if busy || mode == AccountAffinity::Relaxed {
                        "acct_subagent_b"
                    } else {
                        "acct_subagent_a"
                    }
                );
            }
        }
        assert_eq!(affinity.binding_count(), 1);
    }
}

#[tokio::test]
async fn preferred_child_can_claim_without_a_root_and_model_access_can_split_accounts() {
    use gateway_core::account::{AccountModelAccess, AccountModelAccessMode};
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = provider_with_affinity(&store, affinity.clone());
    let select = |thread, model: &'static str, scope| {
        provider.clone().execute(
            planned_request_for_model(
                "openai",
                Operation::Generate(generate_with_session_context("root", Some(thread), None)),
                model,
            ),
            preferred_context("req_preferred_model", scope),
        )
    };
    drop(
        select("early-child", "gpt-5.4", contract_account_scope())
            .await
            .unwrap(),
    );
    assert_eq!(
        affinity.binding_count(),
        1,
        "the first session request claims the shared primary"
    );
    drop(
        select("root", "gpt-5.4", contract_account_scope())
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    let directory = RuntimeAccountDirectory::new(
        [
            ("acct_subagent_a", "gpt-5.4"),
            ("acct_subagent_b", "gpt-5.5"),
        ]
        .into_iter()
        .map(|(id, model)| {
            (
                ProviderAccountId::new(id).unwrap(),
                RuntimeAccount::new(ProviderKind::new("openai").unwrap(), BTreeSet::new())
                    .with_model_access(
                        AccountModelAccess::new(
                            AccountModelAccessMode::Allowlist,
                            vec![model.to_owned()],
                        )
                        .unwrap(),
                    ),
            )
        })
        .collect(),
    );
    let scope = Arc::new(FrozenAccountScope::new(
        Arc::new(directory),
        ClientRoutingScope::all_accounts(),
    ));
    let child = select("model-child", "gpt-5.5", scope.clone())
        .await
        .unwrap();
    assert_eq!(
        child.metadata().provider_account_id().as_str(),
        "acct_subagent_b"
    );
    drop(child);
    assert_eq!(
        select("root", "gpt-5.4", scope)
            .await
            .unwrap()
            .metadata()
            .provider_account_id()
            .as_str(),
        "acct_subagent_a"
    );
    assert_eq!(affinity.binding_count(), 1);
}

#[tokio::test]
async fn preferred_image_turn_prefers_the_session_primary_and_accepts_matching_root_identity() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        affinity,
        server.uri(),
        leases.clone(),
    );
    drop(
        provider
            .clone()
            .execute(
                planned_request("openai", turn_request("root", "root-turn")),
                preferred_context("req_preferred_root", contract_account_scope()),
            )
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    leases
        .busy_accounts
        .lock()
        .unwrap()
        .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
    let child = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", json!({"model":"gpt-5.4","input":"child","client_metadata":{"session_id":"root","thread_id":"child","turn_id":"child-turn"}}).as_object().unwrap().clone()).unwrap()
    ));
    drop(
        provider
            .clone()
            .execute(
                planned_request("openai", child),
                preferred_context("req_preferred_child", contract_account_scope()),
            )
            .await
            .unwrap(),
    );
    leases.busy_accounts.lock().unwrap().clear();
    for session in [None, Some("root")] {
        let mut body = json!({"prompt":"a square", "model":"gpt-image-1"});
        if let Some(session) = session {
            body["session_id"] = json!(session);
        }
        let image = Operation::GenerateImage(ImageRequest::from_raw_json(
            ImageRequestKind::Generation,
            RawJsonPayload::new("openai", Bytes::from(serde_json::to_vec(&body).unwrap()))
                .unwrap()
                .with_context(Map::from_iter([(
                    "image_turn_id".into(),
                    json!("child-turn"),
                )])),
        ));
        let stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request("openai", image),
                preferred_context("req_preferred_image", contract_account_scope()),
            )
            .await
            .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_subagent_a"
        );
    }
    // 轮次只关联会话身份，模式切换不能恢复一次临时分流的账号
    for strict in [true, false] {
        let attempt = if strict {
            context("req_strict_old_turn", CancellationToken::new())
        } else {
            preferred_context("req_preferred_old_turn", contract_account_scope())
        };
        let stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request(
                    "openai",
                    image_for_turn(ImageRequestKind::Generation, "child-turn"),
                ),
                attempt,
            )
            .await
            .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_subagent_a"
        );
    }
}

#[tokio::test]
async fn configured_ttl_renews_binding_and_image_alias_only_at_successful_admission() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url(&store, affinity.clone(), server.uri());
    let week = Duration::from_secs(7 * 24 * 3600);
    let hour = Duration::from_secs(3600);
    for ttl in [week, hour] {
        let stream = provider
            .clone()
            .execute(
                planned_request("openai", turn_request("root", "ttl-turn")),
                preferred_context_with_ttl("req_ttl_root", contract_account_scope(), ttl),
            )
            .await
            .unwrap();
        let before = affinity.renewal_ttls();
        assert_eq!(before.last(), Some(&ttl));
        assert_eq!(affinity.alias_ttls().last(), Some(&ttl));
        drop(stream);
        assert_eq!(
            affinity.renewal_ttls(),
            before,
            "dropping the response cannot renew the binding"
        );
    }
    drop(
        provider
            .execute(
                planned_provider_endpoint_request(
                    "openai",
                    image_for_turn(ImageRequestKind::Generation, "ttl-turn"),
                ),
                preferred_context_with_ttl("req_ttl_image", contract_account_scope(), week),
            )
            .await
            .unwrap(),
    );
    assert_eq!(affinity.renewal_ttls(), vec![week, hour, week]);
    assert_eq!(affinity.alias_ttls(), vec![week, hour, week]);
}

#[tokio::test]
async fn preferred_final_thread_rewrite_within_the_session_preserves_the_primary() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url(&store, affinity, server.uri());
    let request = |thread| {
        planned_request(
            "openai",
            Operation::Generate(generate_with_session_context("root", Some(thread), None)),
        )
    };
    drop(
        provider
            .clone()
            .execute(
                request("root"),
                preferred_context("req_seed_root", contract_account_scope()),
            )
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    let a = store.account("acct_subagent_a").unwrap();
    store.set_enabled(a.id(), false).await.unwrap();
    drop(
        provider
            .clone()
            .execute(
                request("child"),
                preferred_context("req_seed_child", contract_account_scope()),
            )
            .await
            .unwrap(),
    );
    store.set_enabled(a.id(), true).await.unwrap();
    let plan = FrozenMiddlewarePlan::new(
        Arc::new(ChangeTurnMetadata(
            br#"{"session_id":"root","thread_id":"child"}"#,
        )),
        ExtensionSetReference::new(
            ExtensionSetId::new("rewrite-thread".into()).unwrap(),
            Arc::new(TestExtensionLease),
        ),
    );
    let attempt = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_final_thread").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_middleware(
            Some(plan),
            Arc::from([]),
            "/v1/responses".into(),
            ClientTransport::HttpSse,
        ),
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(5),
        account_policy()
            .with_openai_account_affinity(gateway_core::account::AccountAffinity::Preferred),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    );
    let stream = provider.execute(request("root"), attempt).await.unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_subagent_a"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn preferred_spillover_uses_selected_credentials_on_the_wire() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
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
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        affinity.clone(),
        server.uri(),
        leases.clone(),
    );
    for (turn, busy) in [("first", false), ("spillover", true), ("recovered", false)] {
        if busy {
            create_account(&store, "acct_subagent_b").await;
            leases
                .busy_accounts
                .lock()
                .unwrap()
                .insert(ProviderAccountId::new("acct_subagent_a").unwrap());
        } else {
            leases.busy_accounts.lock().unwrap().clear();
        }
        let mut stream = provider
            .clone()
            .execute(
                planned_request("openai", turn_request("root", turn)),
                preferred_context("req_preferred_wire", contract_account_scope()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let sent = server.received_requests().await.unwrap();
    let accounts = sent
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
    assert_eq!(affinity.binding_count(), 1);
    server.verify().await;
}

#[tokio::test]
async fn preferred_unavailable_primary_can_recover_without_losing_its_binding() {
    use gateway_core::account::{
        AccountAffinity, AccountRuntimeSignals, AccountSelectionPolicy, RotationStrategy,
    };
    use gateway_core::concurrency::ConcurrencyQueuePolicy;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        server.uri(),
        leases.clone(),
    );
    let policy = AccountSelectionPolicy::new(RotationStrategy::Smart, NonZeroU32::new(2).unwrap())
        .with_openai_account_affinity(AccountAffinity::Preferred)
        .with_queue(ConcurrencyQueuePolicy {
            max_waiting: 10,
            timeout: Duration::from_secs(1),
        });
    let select = || {
        provider.clone().execute(
            planned_request("openai", turn_request("root", "availability-turn")),
            preferred_context_with_policy(
                "req_primary_availability",
                contract_account_scope(),
                policy,
            ),
        )
    };
    drop(select().await.unwrap());
    create_account(&store, "acct_subagent_b").await;
    let primary = ProviderAccountId::new("acct_subagent_a").unwrap();
    for reason in ["concurrency", "disabled", "quota"] {
        let mut signals = AccountRuntimeSignals {
            in_flight: 0,
            last_started_at: None,
            quota_reset_at: None,
            quota_remaining_rank: None,
            cooldown: None,
            failure_rate_basis_points: None,
            first_output_latency_ms: None,
        };
        match reason {
            "concurrency" => signals.in_flight = 2,
            "disabled" => {
                store.set_enabled(&primary, false).await.unwrap();
            }
            "quota" => {
                store
                    .apply_quota_access(QuotaAccessChange {
                        account_id: primary.clone(),
                        expected_revision: store.account(primary.as_str()).unwrap().revision(),
                        state: QuotaState::exhausted(
                            QuotaEvidence::UsageLimitReached,
                            SystemTime::now(),
                            None,
                        ),
                    })
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        leases
            .signals
            .lock()
            .unwrap()
            .insert(primary.clone(), signals);
        let fallback = tokio::time::timeout(Duration::from_millis(500), select())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fallback.metadata().provider_account_id().as_str(),
            "acct_subagent_b",
            "{reason}"
        );
        drop(fallback);
        leases.signals.lock().unwrap().clear();
        store.set_enabled(&primary, true).await.unwrap();
        store
            .apply_quota_access(QuotaAccessChange {
                account_id: primary.clone(),
                expected_revision: store.account(primary.as_str()).unwrap().revision(),
                state: QuotaState::allowed(SystemTime::now()),
            })
            .await
            .unwrap();
        assert_eq!(
            select()
                .await
                .unwrap()
                .metadata()
                .provider_account_id()
                .as_str(),
            "acct_subagent_a",
            "{reason}"
        );
    }
}

#[tokio::test]
async fn preferred_native_continuation_keeps_its_actual_owner_without_rebinding_the_primary() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let provider = provider_with_affinity(&store, Arc::new(MemorySessionAffinity::default()));
    let request = || planned_request("openai", turn_request("root", "continuation-turn"));
    drop(
        provider
            .clone()
            .execute(
                request(),
                preferred_context("req_primary", contract_account_scope()),
            )
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    for continuation_attempt in [
        ContinuationAttempt::Native,
        ContinuationAttempt::ReplayOwner,
        ContinuationAttempt::ReplayAny,
    ] {
        let account = ProviderAccountId::new("acct_subagent_b").unwrap();
        let provider_kind = ProviderKind::new("openai").unwrap();
        let client = ClientApiKeyId::new("key_openai_contract").unwrap();
        let owner = ProviderAccountStateOwner::new(provider_kind.clone(), account.clone());
        let pin = NativeContinuationPin::new(
            PreviousResponseId::new("resp_spillover"),
            PreviousResponseId::new("resp_spillover"),
            client.clone(),
            provider_kind,
            account,
        )
        .with_scope(gateway_core::engine::continuation::NativeContinuationScope::Persisted);
        let attempt = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new("req_spillover_continuation").unwrap(),
                client,
            )
            .with_request_location(Some(global_request_location())),
            NonZeroU32::MIN,
            SystemTime::now() + Duration::from_secs(5),
            account_policy()
                .with_openai_account_affinity(gateway_core::account::AccountAffinity::Preferred),
            AccountAttemptContext::new(BTreeSet::new(), None, Some(owner))
                .with_account_scope(contract_account_scope()),
            Some(ContinuationBinding::Pinned(pin)),
            CancellationToken::new(),
        )
        .with_continuation_attempt(continuation_attempt);
        let result = provider.clone().execute(request(), attempt).await;
        if continuation_attempt == ContinuationAttempt::ReplayAny {
            // 仍携带原生句柄的请求不能越过状态归属约束切回主账号
            assert_eq!(
                result.err().unwrap().kind(),
                ProviderErrorKind::ContinuationRecoveryRequired
            );
        } else {
            let stream = result.unwrap();
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                "acct_subagent_b"
            );
        }
        assert_eq!(
            provider
                .clone()
                .execute(
                    request(),
                    preferred_context("req_after_continuation", contract_account_scope())
                )
                .await
                .unwrap()
                .metadata()
                .provider_account_id()
                .as_str(),
            "acct_subagent_a"
        );
    }
}

#[tokio::test]
async fn legacy_thread_turn_alias_resumes_on_the_session_primary_without_rewriting_identity() {
    use gateway_core::provider_ports::{
        ProviderSessionAffinityKey, ProviderSessionAffinityPort, ProviderSessionAlias,
    };
    use sha2::Digest;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = provider_with_affinity(&store, affinity.clone());
    let client = ClientApiKeyId::new("key_openai_contract").unwrap();
    let provider_kind = ProviderKind::new("openai").unwrap();
    let root = turn_request("root", "root-turn");
    let root_key = ProviderSessionAffinityKey::try_new(
        provider
            .request_observation(&root, &client)
            .continuation
            .affinity_hash
            .unwrap(),
    )
    .unwrap();
    drop(
        provider
            .clone()
            .execute(
                planned_request("openai", root),
                preferred_context("req_legacy_root", contract_account_scope()),
            )
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    let child_key = ProviderSessionAffinityKey::try_new("legacy-child-binding").unwrap();
    affinity.seed_binding(
        &provider_kind,
        child_key.expose_to_store(),
        ProviderAccountId::new("acct_subagent_b").unwrap(),
    );
    // 持久化轮次键沿用既有哈希合同，模拟升级前的 child → root 关联
    let turn_key = ProviderSessionAffinityKey::try_new(hex::encode(sha2::Sha256::digest(
        b"codex-session-affinity-v1\0client-turn\0key_openai_contract\0legacy-turn",
    )))
    .unwrap();
    let legacy = ProviderSessionAlias {
        session_key: child_key,
        root_session_key: Some(root_key),
        follow_only: false,
    };
    affinity
        .bind_alias(
            &provider_kind,
            &turn_key,
            &legacy,
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let resume = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", json!({"model":"gpt-5.4","input":"resume","client_metadata":{"session_id":"root","thread_id":"child","turn_id":"legacy-turn"}}).as_object().unwrap().clone()).unwrap(),
    ));
    let stream = provider
        .clone()
        .execute(
            planned_request("openai", resume),
            preferred_context("req_legacy_resume", contract_account_scope()),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_subagent_a"
    );
    drop(stream);
    for attempt in [
        preferred_context("req_legacy_image", contract_account_scope()),
        context("req_legacy_strict_image", CancellationToken::new()),
    ] {
        let stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request(
                    "openai",
                    image_for_turn(ImageRequestKind::Generation, "legacy-turn"),
                ),
                attempt,
            )
            .await
            .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_subagent_a"
        );
    }
    assert_eq!(
        affinity
            .load_alias(&provider_kind, &turn_key)
            .await
            .unwrap(),
        Some(legacy)
    );
}
