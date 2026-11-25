//! 对应生产职责的行为测试

use super::*;

#[tokio::test]
async fn invalidation_queues_background_refresh_without_an_upstream_etag() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);
    service
        .invalidate_accounts(&[ProviderAccountId::new("acct_first").unwrap()])
        .await
        .unwrap();
    service
        .invalidate_accounts(&[ProviderAccountId::new("acct_first").unwrap()])
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        service.wait_for_refresh(),
    )
    .await
    .expect("configuration invalidation must wake the catalog worker");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            service.wait_for_refresh(),
        )
        .await
        .is_err(),
        "duplicate invalidations should share one pending refresh"
    );
}

#[tokio::test]
async fn matching_etag_renews_only_its_account_until_the_next_change_or_expiry() {
    use gateway_core::account::AccountCatalogObservation;
    use std::time::{Duration, SystemTime};

    let store = Arc::new(MemoryAccountStore::default());
    let active = seed_account(&store, "acct_active").await;
    let idle = seed_account(&store, "acct_idle").await;
    let document = [&[0_u8][..], OFFICIAL_FIXTURE].concat();
    let old_tag = r#""v1""#;
    let new_tag = r#""v2""#;
    let observed_at = SystemTime::now() - Duration::from_mins(61);
    for account in [&active, &idle] {
        assert!(
            store
                .save_account_catalog(
                    &AccountCatalogObservation {
                        account_id: account.id().clone(),
                        revision: account.revision(),
                        plan: account.plan_type().map(str::to_owned),
                        observed_at,
                        etag: Some(old_tag.to_owned()),
                    },
                    &document
                )
                .await
                .unwrap()
        );
    }
    let server = MockServer::start().await;
    for account in [&active, &idle] {
        Mock::given(method("GET"))
            .and(path("/codex/models"))
            .and(header(
                "chatgpt-account-id",
                format!("chatgpt-{}", account.id()),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", old_tag)
                    .set_body_raw(OFFICIAL_FIXTURE, "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    service.restore().await.unwrap();
    assert!(!service.observe_response_etag(&active, old_tag).unwrap());
    service.refresh_catalogs().await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers["chatgpt-account-id"],
        "chatgpt-acct_idle"
    );

    tokio::time::pause();
    tokio::time::advance(Duration::from_mins(59)).await;
    service.refresh_catalogs().await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    tokio::time::advance(Duration::from_mins(1)).await;
    tokio::time::resume();
    service.refresh_catalogs().await.unwrap();
    server.verify().await;

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .and(header("chatgpt-account-id", "chatgpt-acct_active"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", new_tag)
                .set_body_json(
                    serde_json::json!({"models":[{"slug":"active-only","display_name":"Active"}]}),
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(!service.observe_response_etag(&active, old_tag).unwrap());
    assert!(
        service
            .observe_response_etag(&active, "change-hint")
            .unwrap()
    );
    assert!(!service.observe_response_etag(&active, old_tag).unwrap());
    service.refresh_catalogs().await.unwrap();
    assert!(
        !service
            .observe_response_etag(&active, "change-hint")
            .unwrap()
    );
    assert!(!service.observe_response_etag(&active, new_tag).unwrap());
    service.refresh_catalogs().await.unwrap();
    assert_eq!(
        service.cached_account_models(&active).unwrap().unwrap(),
        ["active-only"]
    );
    server.verify().await;
}

#[tokio::test]
async fn simultaneous_periodic_refreshes_share_the_completed_scan() {
    let store = Arc::new(MemoryAccountStore::default());
    for index in 0..64 {
        seed_account(&store, &format!("acct_refresh_{index:02}")).await;
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(OFFICIAL_FIXTURE.to_vec(), "application/json")
                .set_delay(std::time::Duration::from_millis(20)),
        )
        .expect(64)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    let (first, second, third) = tokio::join!(
        service.refresh_catalogs(),
        service.refresh_catalogs(),
        service.refresh_catalogs(),
    );
    let first = first.expect("first refresh");
    assert_eq!(first.models(), second.expect("shared refresh").models());
    assert_eq!(first.models(), third.expect("shared refresh").models());
    server.verify().await;
}

#[tokio::test]
async fn api_key_background_catalog_preserves_negotiated_reasoning_levels_and_entitlements() {
    use gateway_core::routing::ProviderModelContent;
    use provider_openai::credential::ResponsesTransport;
    use provider_openai::transport::CodexCatalogCapabilityEvidence;
    use serde_json::json;

    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key("acct_api", upstream.uri(), ResponsesTransport::Http)
        .await;
    let original = json!({"slug":"z-unknown-reasoning", "display_name":"Unknown reasoning"});
    let second = json!({
        "slug":"vendor/model", "display_name":"Second upstream model",
        "default_reasoning_level":"high",
        "supported_reasoning_levels":[{"effort":"high","description":"Upstream description"}]
    });
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with({
            let native = json!({"models":[original, second]});
            move |request: &Request| {
                if request.url.query() == Some("client_version=0.144.0") {
                    ResponseTemplate::new(200).set_body_json(&native)
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({"data":[
                        {"id":"z-unknown-reasoning"}, {"id":"vendor/model"}
                    ]}))
                }
            }
        })
        .expect(1)
        .mount(&upstream)
        .await;
    let service = service_with_catalog_cache(&store, upstream.uri(), catalog_cache());
    let snapshot = service
        .refresh_catalogs()
        .await
        .expect("native background directory");
    assert_eq!(
        snapshot.models()[0].capabilities().reasoning(),
        CodexCatalogCapabilityEvidence::Unknown
    );
    let account = store.account("acct_api").unwrap();
    assert_eq!(
        service.cached_account_models(&account).unwrap().unwrap(),
        ["z-unknown-reasoning", "vendor/model"]
    );
    let models = service
        .client_model_catalog(&client_scope(&[account]))
        .await
        .unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        ["z-unknown-reasoning", "vendor/model"]
    );
    for (model, expected) in models.iter().zip([original, second]) {
        let ProviderModelContent::Native(payload) = &model.content else {
            panic!("native model expected")
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(payload.body()).unwrap(),
            expected
        );
    }
    upstream.verify().await;
}
