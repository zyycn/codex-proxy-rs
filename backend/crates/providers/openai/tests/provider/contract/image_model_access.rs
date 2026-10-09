//! Images 混合账号池的模型权限、亲和、重试与中间件改写回归

use super::*;
use gateway_core::account::{AccountAffinity, AccountModelAccess, AccountModelAccessMode};

const CPA_ACCOUNT: &str = "acct_provider_contract";
const IMAGE_ACCOUNT: &str = "acct_scope_new";
const IMAGE_BODY: &[u8] =
    br#"{ "model":"gpt-image-2", "prompt":"first", "prompt":"last", "future":9007199254740993 }"#;

fn policy(mode: AccountModelAccessMode, model: &str) -> AccountModelAccess {
    AccountModelAccess::new(mode, vec![model.to_owned()]).unwrap()
}

fn scope(first: AccountModelAccess, second: AccountModelAccess) -> Arc<FrozenAccountScope> {
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(
            [(CPA_ACCOUNT, first), (IMAGE_ACCOUNT, second)]
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

fn attempt(accounts: AccountAttemptContext) -> AttemptContext {
    configured_attempt(accounts, AccountAffinity::Strict, 1, None)
}

fn configured_attempt(
    accounts: AccountAttemptContext,
    affinity: AccountAffinity,
    index: u32,
    middleware: Option<Arc<dyn MiddlewarePlan>>,
) -> AttemptContext {
    let middleware = middleware.map(|plan| {
        FrozenMiddlewarePlan::new(
            plan,
            ExtensionSetReference::new(
                ExtensionSetId::new("image-access".to_owned()).unwrap(),
                Arc::new(TestExtensionLease),
            ),
        )
    });
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_image_access").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_middleware(
            middleware,
            Arc::from([]),
            "/v1/images/generations".to_owned(),
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
async fn images_do_not_use_forbidden_accounts_when_pinned_or_after_failover() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, CPA_ACCOUNT).await;
    create_account(&store, IMAGE_ACCOUNT).await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::default(),
        server.uri(),
        leases.clone(),
    );
    for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
        for (required, excluded) in [
            (
                Some(ProviderAccountId::new(CPA_ACCOUNT).unwrap()),
                BTreeSet::new(),
            ),
            (
                None,
                BTreeSet::from([ProviderAccountId::new(IMAGE_ACCOUNT).unwrap()]),
            ),
        ] {
            for index in [1, 2] {
                let scope = scope(
                    policy(AccountModelAccessMode::Allowlist, "kimi-k3"),
                    policy(AccountModelAccessMode::Allowlist, "gpt-image-2"),
                );
                let error = provider
                    .clone()
                    .execute(
                        planned_provider_endpoint_request(
                            "openai",
                            image_operation(kind, IMAGE_BODY),
                        ),
                        configured_attempt(
                            AccountAttemptContext::new(excluded.clone(), required.clone(), None)
                                .with_account_scope(scope),
                            AccountAffinity::Strict,
                            index,
                            None,
                        ),
                    )
                    .await
                    .err()
                    .expect("forbidden account must not be used");
                assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
                assert_eq!(error.send_state(), UpstreamSendState::NotSent);
            }
        }
    }
    assert!(leases.requests.lock().unwrap().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unresolved_image_models_only_use_unrestricted_accounts_without_rewriting_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, CPA_ACCOUNT).await;
    create_account(&store, IMAGE_ACCOUNT).await;
    store.set_scheduling(CPA_ACCOUNT, None, AccountWeight::new(100).unwrap());
    let provider = provider_with_base_url(&store, server.uri());
    let bodies: &[&[u8]] = &[
        br#"{"prompt":"default model"}"#,
        br#"{"model":null}"#,
        br#"{"model":{ "future":"unknown" }}"#,
        br#"{"model":""}"#,
        br#"{"model":"gpt-image-2","model":"forbidden"}"#,
        br#"{"model":"forbidden","model":"gpt-image-2"}"#,
        br#"{"model":"gpt-image-2", broken"#,
        b"--image-boundary\r\nopaque multipart body\r\n--image-boundary--\r\n",
    ];
    for mode in [
        AccountModelAccessMode::Allowlist,
        AccountModelAccessMode::Denylist,
    ] {
        for body in bodies {
            for unrestricted in [true, false] {
                let scope = scope(
                    policy(mode, "gpt-image-2"),
                    if unrestricted {
                        AccountModelAccess::all()
                    } else {
                        policy(mode, "gpt-image-2")
                    },
                );
                let result = provider
                    .clone()
                    .execute(
                        planned_provider_endpoint_request(
                            "openai",
                            image_operation(ImageRequestKind::Edit, body),
                        ),
                        attempt(
                            AccountAttemptContext::new(BTreeSet::new(), None, None)
                                .with_account_scope(scope),
                        ),
                    )
                    .await;
                if unrestricted {
                    let mut stream = result.expect("unrestricted account accepts opaque payloads");
                    assert_eq!(
                        stream.metadata().provider_account_id().as_str(),
                        IMAGE_ACCOUNT
                    );
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
                        .expect("unknown model cannot bypass either model policy");
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
async fn image_model_access_overrides_existing_session_affinity() {
    for mode in [
        AccountAffinity::Strict,
        AccountAffinity::Preferred,
        AccountAffinity::Relaxed,
    ] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, CPA_ACCOUNT).await;
        create_account(&store, IMAGE_ACCOUNT).await;
        store.set_scheduling(CPA_ACCOUNT, None, AccountWeight::new(100).unwrap());
        let affinity = Arc::new(MemorySessionAffinity::default());
        let provider = provider_with_affinity_and_base_url(&store, affinity.clone(), server.uri());
        let operation = image_operation(
            ImageRequestKind::Generation,
            br#"{"model":"gpt-image-2","session_id":"image-model-session","prompt":"test"}"#,
        );
        let stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request("openai", operation.clone()),
                configured_attempt(
                    AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(
                        scope(AccountModelAccess::all(), AccountModelAccess::all()),
                    ),
                    mode,
                    1,
                    None,
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            CPA_ACCOUNT
        );
        drop(stream);
        let stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request("openai", operation),
                configured_attempt(
                    AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(
                        scope(
                            policy(AccountModelAccessMode::Allowlist, "kimi-k3"),
                            policy(AccountModelAccessMode::Allowlist, "gpt-image-2"),
                        ),
                    ),
                    mode,
                    1,
                    None,
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            IMAGE_ACCOUNT
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn final_image_model_after_middleware_must_be_allowed_by_the_selected_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, CPA_ACCOUNT).await;
    let provider = provider_with_base_url(&store, server.uri());
    for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
        for (replacement, allowed) in [
            (json!("forbidden"), false),
            (Value::Null, false),
            (json!("gpt-image-future"), true),
        ] {
            let scope = scope(
                AccountModelAccess::new(
                    AccountModelAccessMode::Allowlist,
                    vec!["gpt-image-2".to_owned(), "gpt-image-future".to_owned()],
                )
                .unwrap(),
                AccountModelAccess::all(),
            );
            let middleware = Arc::new(RecordingMiddleware {
                observed: Arc::default(),
                replacement: ("model".to_owned(), replacement.clone()),
                request_headers: Vec::new(),
            });
            let result = provider
                .clone()
                .execute(
                    planned_provider_endpoint_request("openai", image_operation(kind, IMAGE_BODY)),
                    configured_attempt(
                        AccountAttemptContext::new(BTreeSet::new(), None, None)
                            .with_account_scope(scope),
                        AccountAffinity::Strict,
                        1,
                        Some(middleware),
                    ),
                )
                .await;
            if allowed {
                let mut stream = result.unwrap();
                while let Some(event) = stream.next().await {
                    event.unwrap();
                }
                assert_eq!(
                    captured_request_body(
                        server.received_requests().await.unwrap().last().unwrap()
                    )["model"],
                    replacement
                );
            } else {
                let error = result
                    .err()
                    .expect("rewriting cannot escape account policy");
                assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
                assert_eq!(error.send_state(), UpstreamSendState::NotSent);
                assert!(error.retry_is_prohibited());
            }
        }
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn standalone_search_does_not_inherit_image_model_restrictions() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, CPA_ACCOUNT).await;
    let search = Operation::Search(StandaloneSearchRequest::from_raw_json(
        RawJsonPayload::new(
            "openai",
            Bytes::from_static(br#"{"id":"search","commands":{}}"#),
        )
        .unwrap(),
    ));
    let stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_provider_endpoint_request("openai", search),
            attempt(
                AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope(
                    policy(AccountModelAccessMode::Allowlist, "kimi-k3"),
                    AccountModelAccess::all(),
                )),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        CPA_ACCOUNT
    );
}

fn image_operation(kind: ImageRequestKind, body: &[u8]) -> Operation {
    Operation::GenerateImage(ImageRequest::from_raw_json(
        kind,
        RawJsonPayload::new("openai", Bytes::copy_from_slice(body)).unwrap(),
    ))
}

#[tokio::test]
async fn images_filter_mixed_accounts_by_model_before_weight_and_preserve_wire() {
    let cpa = MockServer::start().await;
    let oauth = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key(
            CPA_ACCOUNT,
            cpa.uri(),
            provider_openai::credential::ResponsesTransport::Http,
        )
        .await;
    store.set_scheduling(CPA_ACCOUNT, None, AccountWeight::new(100).unwrap());
    create_account(&store, IMAGE_ACCOUNT).await;
    Mock::given(method("POST"))
        .and(body_bytes(IMAGE_BODY.to_vec()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"b64_json":"AAEC"}]})),
        )
        .expect(4)
        .mount(&oauth)
        .await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::default(),
        oauth.uri(),
        leases.clone(),
    );
    for cpa_policy in [
        policy(AccountModelAccessMode::Allowlist, "kimi-k3"),
        policy(AccountModelAccessMode::Denylist, "gpt-image-2"),
    ] {
        for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
            let scope = scope(
                cpa_policy.clone(),
                policy(AccountModelAccessMode::Allowlist, "gpt-image-2"),
            );
            let mut stream = provider
                .clone()
                .execute(
                    planned_provider_endpoint_request("openai", image_operation(kind, IMAGE_BODY)),
                    attempt(
                        AccountAttemptContext::new(BTreeSet::new(), None, None)
                            .with_account_scope(scope),
                    ),
                )
                .await
                .unwrap();
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                IMAGE_ACCOUNT
            );
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
        }
    }
    assert!(cpa.received_requests().await.unwrap().is_empty());
    assert!(
        leases
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.account_id().as_str() == IMAGE_ACCOUNT)
    );
    assert_eq!(
        oauth
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| request.url.path().to_owned())
            .collect::<Vec<_>>(),
        [
            "/codex/images/generations",
            "/codex/images/edits",
            "/codex/images/generations",
            "/codex/images/edits"
        ]
    );
}
