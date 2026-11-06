//! 无显式会话时，选号提示的优先级、隔离边界与完整历史要求

use super::*;

#[tokio::test]
async fn cache_preference_keys_follow_client_hints_or_the_opening_messages() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_affinity").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = provider_with_affinity(&store, affinity.clone());

    let select = async |client: &str, model: &str, body: serde_json::Value| {
        let before = affinity.lookup_keys().len();
        let attempt = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new("req_cache_key").unwrap(),
                ClientApiKeyId::new(client).unwrap(),
            ),
            NonZeroU32::MIN,
            SystemTime::now() + Duration::from_secs(5),
            account_policy(),
            AccountAttemptContext::new(BTreeSet::new(), None, None)
                .with_account_scope(contract_account_scope()),
            None,
            CancellationToken::new(),
        );
        let payload =
            ProtocolPayload::json_object("openai", body.as_object().unwrap().clone()).unwrap();
        drop(
            provider
                .clone()
                .execute(
                    planned_request_for_model(
                        "openai",
                        Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
                        model,
                    ),
                    attempt,
                )
                .await
                .unwrap(),
        );
        affinity.lookup_keys().get(before).cloned()
    };

    let model = "gpt-5.4";
    let opening = json!({"instructions":"be brief", "input":[{"role":"developer","content":"review"}, {"role":"user","content":"hello"}]});
    let key = select("client_a", model, opening.clone()).await.unwrap();
    let mut next = opening.clone();
    next["tools"] = json!([{"type":"function","name":"new_tool"}]);
    next["input"][1]["content"] =
        json!([{"type":"input_text","text":"hel"},{"type":"input_text","text":"lo"}]);
    next["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"next turn"}));
    assert_eq!(select("client_a", model, next).await.as_ref(), Some(&key));
    assert_ne!(
        select("client_b", model, opening.clone()).await.unwrap(),
        key
    );
    assert_ne!(
        select("client_a", "gpt-5.5", opening.clone())
            .await
            .unwrap(),
        key
    );
    for path in ["/instructions", "/input/0/content", "/input/1/content"] {
        let mut changed = opening.clone();
        *changed.pointer_mut(path).unwrap() = json!("different");
        assert_ne!(select("client_a", model, changed).await.unwrap(), key);
    }

    let chat_opening = json!({"input":[{"role":"system","content":"be brief"},{"role":"developer","content":"review"},{"role":"user","content":"hello"}]});
    assert_eq!(
        select("client_a", model, chat_opening).await.as_ref(),
        Some(&key)
    );

    let hinted = json!({"prompt_cache_key":"client-hint","input":"hello"});
    let cache = select("client_a", model, hinted.clone()).await.unwrap();
    assert_ne!(key, cache);
    assert_ne!(select("client_b", model, hinted).await.unwrap(), cache);
    assert_eq!(
        select(
            "client_a",
            model,
            json!({"prompt_cache_key":"client-hint", "input":"different"})
        )
        .await,
        Some(cache)
    );
    for input in [
        json!(" "),
        json!([{"role":"assistant","content":"partial history"},{"role":"user","content":"next"}]),
        json!([{"role":"user","content":[{"type":"input_image","image_url":"https://invalid.test/x"}]}]),
    ] {
        assert_eq!(
            select(
                "client_a",
                model,
                json!({"instructions":"shared", "input":input})
            )
            .await,
            None
        );
    }
}

#[tokio::test]
async fn cache_preference_storage_timeouts_follow_the_affinity_mode() {
    use gateway_core::account::AccountAffinity;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_affinity").await;
    for mode in [AccountAffinity::Preferred, AccountAffinity::Strict] {
        for affinity in [
            MemorySessionAffinity::with_operation_delay(Duration::from_secs(1)),
            MemorySessionAffinity::with_initial_claim_barrier(2),
        ] {
            let affinity = Arc::new(affinity);
            let server = MockServer::start().await;
            let provider =
                provider_with_affinity_and_base_url(&store, affinity.clone(), server.uri());
            let result = provider
                .execute(
                    planned_request("openai", generate_operation()),
                    session_binding::context_with_affinity_policy(
                        "req_cache_store_timeout",
                        contract_account_scope(),
                        account_policy().with_openai_account_affinity(mode),
                    ),
                )
                .await;
            if mode == AccountAffinity::Preferred {
                assert_eq!(
                    result.unwrap().metadata().provider_account_id().as_str(),
                    "acct_affinity"
                );
            } else {
                let error = result
                    .err()
                    .expect("strict must not bypass the binding store");
                assert_eq!(error.send_state(), UpstreamSendState::NotSent);
            }
            assert_eq!(affinity.binding_count(), 0);
            assert!(server.received_requests().await.unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn strict_cache_preference_waits_and_preserves_binding_on_rejected_waits() {
    use gateway_core::concurrency::ConcurrencyQueuePolicy;

    for cache_hint in [false, true] {
        for outcome in [
            "released",
            "timeout",
            "cancelled",
            "disabled",
            "queue_disabled",
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
            let policy = account_policy().with_queue(ConcurrencyQueuePolicy {
                max_waiting: u32::from(outcome != "queue_disabled"),
                timeout: Duration::from_millis(150),
            });
            let select = || {
                let mut body = json!({"input":"same opening"});
                if cache_hint {
                    body["prompt_cache_key"] = json!("client-cache-key");
                }
                let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
                    ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
                        .unwrap(),
                ));
                provider.clone().execute(
                    planned_request("openai", operation),
                    session_binding::context_with_affinity_policy(
                        "req_strict_cache",
                        contract_account_scope(),
                        policy,
                    ),
                )
            };
            drop(select().await.unwrap());
            create_account(&store, "acct_subagent_b").await;
            let primary = ProviderAccountId::new("acct_subagent_a").unwrap();
            leases.busy_accounts.lock().unwrap().insert(primary.clone());

            let mut waiting = Box::pin(select());
            if outcome != "queue_disabled" {
                assert!(
                    timeout(Duration::from_millis(30), waiting.as_mut())
                        .await
                        .is_err()
                );
                // 原账号占满时，即使另一个账号空闲也不分流，排队满后仍保留绑定
                let full = select()
                    .await
                    .err()
                    .expect("one waiting slot is already occupied");
                assert_eq!(full.send_state(), UpstreamSendState::NotSent);
            }
            match outcome {
                "released" => leases.busy_accounts.lock().unwrap().clear(),
                "disabled" => store.set_enabled(&primary, false).await.unwrap(),
                "timeout" => {
                    let error = waiting.await.err().expect("strict queue timeout");
                    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
                    waiting = Box::pin(select());
                    leases.busy_accounts.lock().unwrap().clear();
                }
                "cancelled" => {
                    // 丢弃请求 future 必须释放等待位置，后续请求仍能等原账号
                    drop(waiting);
                    waiting = Box::pin(select());
                    assert!(
                        timeout(Duration::from_millis(30), waiting.as_mut())
                            .await
                            .is_err()
                    );
                    leases.busy_accounts.lock().unwrap().clear();
                }
                "queue_disabled" => {}
                _ => unreachable!(),
            }
            let expected = if matches!(outcome, "disabled" | "queue_disabled") {
                "acct_subagent_b"
            } else {
                "acct_subagent_a"
            };
            assert_eq!(
                waiting
                    .await
                    .unwrap()
                    .metadata()
                    .provider_account_id()
                    .as_str(),
                expected
            );
            leases.busy_accounts.lock().unwrap().clear();
            assert_eq!(
                select()
                    .await
                    .unwrap()
                    .metadata()
                    .provider_account_id()
                    .as_str(),
                expected
            );
            assert_eq!(affinity.binding_count(), 1);
            assert!(affinity.alias_ttls().is_empty());
            assert!(server.received_requests().await.unwrap().is_empty());
        }
    }
}
