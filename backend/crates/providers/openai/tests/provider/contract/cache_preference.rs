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
async fn cache_preference_storage_timeouts_allow_normal_admission() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_affinity").await;
    for affinity in [
        MemorySessionAffinity::with_operation_delay(Duration::from_secs(1)),
        MemorySessionAffinity::with_initial_claim_barrier(2),
    ] {
        let affinity = Arc::new(affinity);
        let provider = provider_with_affinity(&store, affinity.clone());
        let stream = provider
            .execute(
                planned_request("openai", generate_operation()),
                context("req_cache_store_timeout", CancellationToken::new()),
            )
            .await
            .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_affinity"
        );
        assert_eq!(affinity.binding_count(), 0);
    }
}
