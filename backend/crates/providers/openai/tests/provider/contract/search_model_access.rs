//! 独立搜索的账号模型限制、未知模型、亲和与重试选号回归

use super::*;
use gateway_core::account::{AccountAffinity, AccountModelAccess, AccountModelAccessMode};

const FIRST: &str = "acct_provider_contract";
const SECOND: &str = "acct_scope_new";
const MODEL: &str = "gpt-6-astra";
const BODY: &[u8] = br#"{ "model":"gpt-6-astra", "commands":{"search_query":[{"q":"test"}]}, "future":1, "future":9007199254740993 }"#;

fn policy(mode: AccountModelAccessMode, model: &str) -> AccountModelAccess {
    AccountModelAccess::new(mode, vec![model.to_owned()]).unwrap()
}

fn scope(first: AccountModelAccess, second: AccountModelAccess) -> Arc<FrozenAccountScope> {
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(
            [(FIRST, first), (SECOND, second)]
                .into_iter()
                .map(|(id, policy)| {
                    (
                        ProviderAccountId::new(id).unwrap(),
                        RuntimeAccount::new(ProviderKind::new("openai").unwrap(), BTreeSet::new())
                            .with_model_access(policy),
                    )
                })
                .collect(),
        )),
        ClientRoutingScope::all_accounts(),
    ))
}

fn search(body: &[u8]) -> ProviderRequest {
    planned_provider_endpoint_request(
        "openai",
        Operation::Search(StandaloneSearchRequest::from_raw_json(
            RawJsonPayload::new("openai", Bytes::copy_from_slice(body)).unwrap(),
        )),
    )
}

fn attempt(
    accounts: AccountAttemptContext,
    affinity: AccountAffinity,
    index: u32,
    middleware: Option<Arc<dyn MiddlewarePlan>>,
) -> AttemptContext {
    let middleware = middleware.map(|plan| {
        FrozenMiddlewarePlan::new(
            plan,
            ExtensionSetReference::new(
                ExtensionSetId::new("search-access".to_owned()).unwrap(),
                Arc::new(TestExtensionLease),
            ),
        )
    });
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_search_access").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_middleware(
            middleware,
            Arc::from([]),
            "/v1/alpha/search".to_owned(),
            ClientTransport::HttpJson,
        ),
        NonZeroU32::new(index).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        account_policy().with_openai_account_affinity(affinity),
        accounts,
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn search_filters_oauth_and_api_key_accounts_before_weight_and_preserves_wire() {
    let api = MockServer::start().await;
    let oauth = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key(
            FIRST,
            api.uri(),
            provider_openai::credential::ResponsesTransport::Http,
        )
        .await;
    create_account(&store, SECOND).await;
    store.set_scheduling(FIRST, None, AccountWeight::new(100).unwrap());
    for (server, endpoint) in [(&api, "/alpha/search"), (&oauth, "/codex/alpha/search")] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .and(body_bytes(BODY.to_vec()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":"result"})))
            .expect(2)
            .mount(server)
            .await;
    }
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::default(),
        oauth.uri(),
        leases.clone(),
    );
    for denied in [
        policy(AccountModelAccessMode::Denylist, MODEL),
        policy(AccountModelAccessMode::Allowlist, "other-model"),
    ] {
        let allowed = policy(AccountModelAccessMode::Allowlist, MODEL);
        for (scope, expected) in [
            (scope(denied.clone(), allowed.clone()), SECOND),
            (scope(allowed, denied.clone()), FIRST),
        ] {
            let mut stream = provider
                .clone()
                .execute(
                    search(BODY),
                    attempt(
                        AccountAttemptContext::new(BTreeSet::new(), None, None)
                            .with_account_scope(scope),
                        AccountAffinity::Strict,
                        1,
                        None,
                    ),
                )
                .await
                .unwrap();
            assert_eq!(stream.metadata().provider_account_id().as_str(), expected);
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
        }
    }
    assert_eq!(
        leases
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.account_id().as_str())
            .collect::<Vec<_>>(),
        [SECOND, FIRST, SECOND, FIRST]
    );
}

#[tokio::test]
async fn search_does_not_use_forbidden_accounts_when_pinned_or_after_failover() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, FIRST).await;
    create_account(&store, SECOND).await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::default(),
        server.uri(),
        leases.clone(),
    );
    for denied in [
        policy(AccountModelAccessMode::Denylist, MODEL),
        policy(AccountModelAccessMode::Allowlist, "other-model"),
    ] {
        for (required, excluded) in [
            (
                Some(ProviderAccountId::new(FIRST).unwrap()),
                BTreeSet::new(),
            ),
            (
                None,
                BTreeSet::from([ProviderAccountId::new(SECOND).unwrap()]),
            ),
        ] {
            for index in [1, 2] {
                let error = provider
                    .clone()
                    .execute(
                        search(BODY),
                        attempt(
                            AccountAttemptContext::new(excluded.clone(), required.clone(), None)
                                .with_account_scope(scope(
                                    denied.clone(),
                                    AccountModelAccess::all(),
                                )),
                            AccountAffinity::Strict,
                            index,
                            None,
                        ),
                    )
                    .await
                    .err()
                    .expect("forbidden accounts must not be leased");
                assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
                assert_eq!(error.send_state(), UpstreamSendState::NotSent);
            }
        }
    }
    assert!(leases.requests.lock().unwrap().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unresolved_search_models_only_use_unrestricted_accounts_and_preserve_wire() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/alpha/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":"result"})))
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, FIRST).await;
    create_account(&store, SECOND).await;
    store.set_scheduling(FIRST, None, AccountWeight::new(100).unwrap());
    let provider = provider_with_base_url(&store, server.uri());
    let bodies: &[&[u8]] = &[
        br#"{"commands":{}}"#,
        br#"{"model":null}"#,
        br#"{"model":42}"#,
        br#"{"model":""}"#,
        br#"{"model":{"future":"unknown"}}"#,
        br#"{"model":"gpt-6-astra","model":"other-model"}"#,
        br#"{"model":"other-model","model":"gpt-6-astra"}"#,
        br#"{"model":"gpt-6-astra", broken"#,
    ];
    for mode in [
        AccountModelAccessMode::Allowlist,
        AccountModelAccessMode::Denylist,
    ] {
        for body in bodies {
            for unrestricted in [true, false] {
                let scope = scope(
                    policy(mode, MODEL),
                    if unrestricted {
                        AccountModelAccess::all()
                    } else {
                        policy(mode, MODEL)
                    },
                );
                let result = provider
                    .clone()
                    .execute(
                        search(body),
                        attempt(
                            AccountAttemptContext::new(BTreeSet::new(), None, None)
                                .with_account_scope(scope),
                            AccountAffinity::Strict,
                            1,
                            None,
                        ),
                    )
                    .await;
                if unrestricted {
                    let mut stream = result.expect("unrestricted account accepts opaque search");
                    assert_eq!(stream.metadata().provider_account_id().as_str(), SECOND);
                    while let Some(event) = stream.next().await {
                        event.unwrap();
                    }
                    assert_eq!(
                        server
                            .received_requests()
                            .await
                            .unwrap()
                            .last()
                            .unwrap()
                            .body,
                        *body
                    );
                } else {
                    let error = result
                        .err()
                        .expect("unknown model cannot bypass model policy");
                    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
                    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
                }
            }
        }
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        bodies.len() * 2
    );
}

#[tokio::test]
async fn search_model_policy_overrides_existing_session_affinity() {
    for affinity in [
        AccountAffinity::Strict,
        AccountAffinity::Preferred,
        AccountAffinity::Relaxed,
    ] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, FIRST).await;
        create_account(&store, SECOND).await;
        store.set_scheduling(FIRST, None, AccountWeight::new(100).unwrap());
        let provider = provider_with_affinity_and_base_url(&store, Arc::default(), server.uri());
        let body = br#"{"id":"search-model-session","model":"gpt-6-astra","commands":{}}"#;
        for (scope, expected) in [
            (
                scope(AccountModelAccess::all(), AccountModelAccess::all()),
                FIRST,
            ),
            (
                scope(
                    policy(AccountModelAccessMode::Denylist, MODEL),
                    AccountModelAccess::all(),
                ),
                SECOND,
            ),
        ] {
            let stream = provider
                .clone()
                .execute(
                    search(body),
                    attempt(
                        AccountAttemptContext::new(BTreeSet::new(), None, None)
                            .with_account_scope(scope),
                        affinity,
                        1,
                        None,
                    ),
                )
                .await
                .unwrap();
            assert_eq!(stream.metadata().provider_account_id().as_str(), expected);
            drop(stream);
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn search_model_selection_does_not_restrict_plugin_body_rewrites() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/alpha/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":"result"})))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, FIRST).await;
    let middleware = Arc::new(RecordingMiddleware {
        observed: Arc::default(),
        replacement: ("model".to_owned(), json!("plugin-model")),
        request_headers: Vec::new(),
    });
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            search(BODY),
            attempt(
                AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope(
                    policy(AccountModelAccessMode::Allowlist, MODEL),
                    AccountModelAccess::all(),
                )),
                AccountAffinity::Strict,
                1,
                Some(middleware),
            ),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    assert_eq!(
        captured_request_body(&server.received_requests().await.unwrap()[0])["model"],
        "plugin-model"
    );
}

#[tokio::test]
async fn search_reuses_protocol_projection_until_credential_revision_changes() {
    use gateway_core::account::{CredentialCasUpdate, ProviderAccountUpdate};
    use provider_openai::credential::{
        ApiKeyProtocol, CodexCredentialCodec, CodexCredentialData, ResponsesTransport,
    };
    let store = Arc::new(MemoryAccountStore::default());
    let server = MockServer::start().await;
    store
        .seed_api_key_for_protocol(
            FIRST,
            server.uri(),
            ResponsesTransport::Http,
            ApiKeyProtocol::Messages,
        )
        .await;
    let provider = provider(&store);
    let ctx = || {
        attempt(
            AccountAttemptContext::new(BTreeSet::new(), None, None)
                .with_account_scope(scope(AccountModelAccess::all(), AccountModelAccess::all())),
            AccountAffinity::Strict,
            1,
            None,
        )
    };
    let loads = store.credential_loads();
    for _ in 0..2 {
        let err = provider
            .clone()
            .execute(search(BODY), ctx())
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind(), ProviderErrorKind::NoEligibleAccount);
    }
    assert_eq!(store.credential_loads() - loads, 1);
    let id = ProviderAccountId::new(FIRST).unwrap();
    let loaded = store.load_current_credential(&id).await.unwrap();
    let mut data = CodexCredentialCodec::decode_complete(&loaded.credential).unwrap();
    let CodexCredentialData::ApiKey(config) = &mut data else {
        panic!("预期 API Key 账号");
    };
    config.protocol = ApiKeyProtocol::Responses;
    let update = CredentialCasUpdate::new(
        id.clone(),
        loaded.account.revision(),
        ProviderAccountUpdate {
            account_id: id.clone(),
            name: FIRST.to_owned(),
            email: None,
            plan_type: None,
        },
        CodexCredentialCodec::encode_complete(data).unwrap(),
        false,
        None,
        None,
    )
    .unwrap()
    .preserving_profile();
    store.compare_and_swap_credential(update).await.unwrap();
    let stream = provider.execute(search(BODY), ctx()).await.unwrap();
    assert_eq!(stream.metadata().provider_account_id(), &id);
}
