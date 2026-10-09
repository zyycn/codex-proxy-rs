//! Images 的 OAuth 套餐资格、混合池、亲和和失败换号回归

use super::*;
use gateway_core::account::{AccountAffinity, ProviderAccountUpdate};

const PLUS: &str = "acct_provider_contract";
const FREE: [&str; 3] = ["acct_scope_new", "acct_scope_old", "acct_scope_same"];

async fn oauth_account(store: &MemoryAccountStore, id: &str, plan: Option<&str>) {
    let mut verified_account = profile(&format!("chatgpt-{id}"));
    verified_account.plan_type = plan.map(str::to_owned);
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: id.to_owned(),
            name: id.to_owned(),
            secret: secret(&format!("at-{id}")),
            verified_account,
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
}

fn image(kind: ImageRequestKind, body: Value) -> Operation {
    Operation::GenerateImage(ImageRequest::from_raw_json(
        kind,
        RawJsonPayload::new("openai", Bytes::from(serde_json::to_vec(&body).unwrap())).unwrap(),
    ))
}

fn selection_attempt(
    accounts: AccountAttemptContext,
    affinity: AccountAffinity,
    index: u32,
) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_image_eligibility").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        ),
        NonZeroU32::new(index).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        account_policy().with_openai_account_affinity(affinity),
        accounts.with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn images_use_plus_in_a_pool_with_three_higher_weight_free_accounts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("chatgpt-account-id", format!("chatgpt-{PLUS}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
        .expect(2)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    oauth_account(&store, PLUS, Some("plus")).await;
    for id in FREE {
        oauth_account(&store, id, Some("free")).await;
        store.set_scheduling(id, None, AccountWeight::new(100).unwrap());
    }
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::default(),
        server.uri(),
        leases.clone(),
    );
    for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
        let mut stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request(
                    "openai",
                    image(kind, json!({"model":"gpt-image-2","prompt":"test"})),
                ),
                context("req_plus_images", CancellationToken::new()),
            )
            .await
            .unwrap();
        assert_eq!(stream.metadata().provider_account_id().as_str(), PLUS);
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    assert!(
        leases
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.account_id().as_str() == PLUS)
    );
}

#[tokio::test]
async fn images_never_fall_back_to_free_when_plus_is_excluded_or_free_is_pinned() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    oauth_account(&store, PLUS, Some("plus")).await;
    oauth_account(&store, FREE[0], Some("free")).await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::default(),
        server.uri(),
        leases.clone(),
    );
    for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
        for body in [json!({"model":"gpt-image-2"}), json!({"prompt":"test"})] {
            for (required, excluded) in [
                (
                    Some(ProviderAccountId::new(FREE[0]).unwrap()),
                    BTreeSet::new(),
                ),
                (
                    None,
                    BTreeSet::from([ProviderAccountId::new(PLUS).unwrap()]),
                ),
            ] {
                for index in [1, 2] {
                    let error = provider
                        .clone()
                        .execute(
                            planned_provider_endpoint_request("openai", image(kind, body.clone())),
                            selection_attempt(
                                AccountAttemptContext::new(
                                    excluded.clone(),
                                    required.clone(),
                                    None,
                                ),
                                AccountAffinity::Strict,
                                index,
                            ),
                        )
                        .await
                        .err()
                        .expect("Free must remain ineligible for Images");
                    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
                    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
                }
            }
        }
    }
    assert!(leases.requests.lock().unwrap().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn images_escape_free_session_bindings_while_search_remains_available() {
    for mode in [
        AccountAffinity::Strict,
        AccountAffinity::Preferred,
        AccountAffinity::Relaxed,
    ] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        oauth_account(&store, PLUS, Some("plus")).await;
        oauth_account(&store, FREE[0], Some("free")).await;
        store.set_scheduling(FREE[0], None, AccountWeight::new(100).unwrap());
        let affinity = Arc::new(MemorySessionAffinity::default());
        let provider = provider_with_affinity_and_base_url(&store, affinity.clone(), server.uri());
        let search = Operation::Search(StandaloneSearchRequest::from_raw_json(
            RawJsonPayload::new(
                "openai",
                Bytes::from_static(br#"{"id":"image-plan-session","commands":{}}"#),
            )
            .unwrap(),
        ));
        let stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request("openai", search),
                selection_attempt(
                    AccountAttemptContext::new(BTreeSet::new(), None, None),
                    mode,
                    1,
                ),
            )
            .await
            .unwrap();
        assert_eq!(stream.metadata().provider_account_id().as_str(), FREE[0]);
        drop(stream);
        assert_eq!(affinity.binding_count(), 1);
        for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
            let key = affinity.lookup_keys()[0].clone();
            affinity.seed_binding(
                &ProviderKind::new("openai").unwrap(),
                &key,
                ProviderAccountId::new(FREE[0]).unwrap(),
            );
            let stream = provider
                .clone()
                .execute(
                    planned_provider_endpoint_request(
                        "openai",
                        image(
                            kind,
                            json!({
                                "model":"gpt-image-2", "session_id":"image-plan-session"
                            }),
                        ),
                    ),
                    selection_attempt(
                        AccountAttemptContext::new(BTreeSet::new(), None, None),
                        mode,
                        1,
                    ),
                )
                .await
                .unwrap();
            assert_eq!(stream.metadata().provider_account_id().as_str(), PLUS);
            drop(stream);
            assert_eq!(affinity.binding_count(), 1);
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn image_plan_eligibility_tracks_account_updates_without_affecting_text() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    oauth_account(&store, PLUS, Some("free")).await;
    let provider = provider_with_base_url(&store, server.uri());
    drop(
        provider
            .clone()
            .execute(
                planned_request("openai", generate_operation()),
                context("req_free_text", CancellationToken::new()),
            )
            .await
            .expect("Free can still use Responses"),
    );
    for plan in [
        Some("free"),
        Some("plus"),
        Some("free"),
        None,
        Some("future-plan"),
    ] {
        store
            .update_account(ProviderAccountUpdate {
                account_id: ProviderAccountId::new(PLUS).unwrap(),
                name: PLUS.to_owned(),
                email: None,
                plan_type: plan.map(str::to_owned),
            })
            .await
            .unwrap();
        let result = provider
            .clone()
            .execute(
                planned_provider_endpoint_request(
                    "openai",
                    image(ImageRequestKind::Generation, json!({"model":"gpt-image-2"})),
                ),
                context("req_plan_change", CancellationToken::new()),
            )
            .await;
        if plan == Some("free") {
            let error = result.err().expect("Free must be excluded");
            assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        } else {
            drop(result.expect("unknown plans are not inferred to be Free"));
        }
    }
}

#[tokio::test]
async fn api_key_image_accounts_do_not_inherit_chatgpt_free_plan_restrictions() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
        .expect(2)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key(
            PLUS,
            server.uri(),
            provider_openai::credential::ResponsesTransport::Http,
        )
        .await;
    store
        .update_account(ProviderAccountUpdate {
            account_id: ProviderAccountId::new(PLUS).unwrap(),
            name: PLUS.to_owned(),
            email: None,
            plan_type: Some("free".to_owned()),
        })
        .await
        .unwrap();
    let provider = provider_with_base_url(&store, server.uri());
    for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
        let mut stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request(
                    "openai",
                    image(kind, json!({"model":"gpt-image-2"})),
                ),
                context("req_api_key_images", CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
}
