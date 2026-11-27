//! 首次目录查询失败后的周期恢复与能力发布

use crate::support::Fixture;
use gateway_core::{engine::provider::Provider, lifecycle::CancellationToken};
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

#[tokio::test]
async fn missing_catalog_is_retried_and_published_after_transient_failure() {
    let upstream = MockServer::start().await;
    let fixture = Fixture::new(&upstream.uri());
    *fixture.store.primary().catalog.lock().unwrap() = None;
    let failed = Mock::given(path("/v1internal:loadCodeAssist"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount_as_scoped(&upstream)
        .await;
    assert!(
        fixture
            .catalog
            .reconcile(&CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        fixture
            .provider
            .query_model_capabilities()
            .await
            .unwrap()
            .is_empty()
    );
    drop(failed);
    Mock::given(path("/v1internal:loadCodeAssist"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"cloudaicompanionProject":"fixture-project"})),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(path("/v1internal:fetchAvailableModels"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"models":{"fixture-model":{"displayName":"Fixture Model"}}})),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let generation = fixture.provider.catalog_generation();
    fixture
        .catalog
        .reconcile(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        fixture
            .provider
            .query_model_capabilities()
            .await
            .unwrap()
            .len(),
        1
    );
    assert_ne!(fixture.provider.catalog_generation(), generation);
    fixture
        .catalog
        .reconcile(&CancellationToken::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn credential_revision_change_replaces_the_cached_catalog() {
    use gateway_core::account::{CredentialRevision, CredentialState, ProviderAccount, QuotaState};
    let upstream = MockServer::start().await;
    let fixture = Fixture::new(&upstream.uri());
    fixture
        .catalog
        .reconcile(&CancellationToken::new())
        .await
        .unwrap();
    assert!(upstream.received_requests().await.unwrap().is_empty());
    let generation = fixture.provider.catalog_generation();
    {
        let mut loaded = fixture.store.primary().loaded.lock().unwrap();
        let account = &loaded.account;
        loaded.account = ProviderAccount::new(
            account.id().clone(),
            account.provider().clone(),
            account.name().into(),
            account.upstream_user_id().map(str::to_owned),
            "oauth".into(),
            CredentialRevision::new(2).unwrap(),
            account.access_token_expires_at(),
        )
        .with_account_facts(
            true,
            CredentialState::Ready,
            QuotaState::unknown(),
            None,
            None,
        );
    }
    Mock::given(path("/v1internal:loadCodeAssist"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"cloudaicompanionProject":"rotated-project"})),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(path("/v1internal:fetchAvailableModels"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"models":{"rotated-model":{"displayName":"Rotated Model"}}})),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    fixture
        .catalog
        .reconcile(&CancellationToken::new())
        .await
        .unwrap();
    assert_ne!(fixture.provider.catalog_generation(), generation);
    {
        let stored = fixture.store.primary().catalog.lock().unwrap();
        let (observation, catalog) = stored.as_ref().unwrap();
        assert_eq!(observation.revision, CredentialRevision::new(2).unwrap());
        let catalog: serde_json::Value = serde_json::from_slice(catalog).unwrap();
        assert_eq!(catalog["project"], "rotated-project");
        assert!(catalog["models"].get("rotated-model").is_some());
        assert!(catalog["models"].get("fixture-model").is_none());
    }
    fixture
        .catalog
        .reconcile(&CancellationToken::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn renewed_identity_rebinds_catalog_without_waiting_for_another_upstream_query() {
    let upstream = MockServer::start().await;
    let fixture = Fixture::new(&upstream.uri());
    let old = fixture.store.primary().loaded.lock().unwrap().clone();
    let mut captured = fixture
        .catalog
        .capture_refresh_catalogs(std::slice::from_ref(&old))
        .await
        .unwrap();
    let revision = gateway_core::account::CredentialRevision::new(2).unwrap();
    fixture.store.primary().loaded.lock().unwrap().account =
        gateway_core::account::ProviderAccount::new(
            old.account.id().clone(),
            old.account.provider().clone(),
            old.account.name().into(),
            old.account.upstream_user_id().map(str::to_owned),
            "oauth".into(),
            revision,
            old.account.access_token_expires_at(),
        );
    assert!(fixture.catalog.catalogs().await.unwrap().is_empty());
    fixture
        .catalog
        .after_credential_refresh(
            old.account.id(),
            revision,
            captured.remove(old.account.id()),
        )
        .await
        .unwrap();
    let catalogs = fixture.catalog.catalogs().await.unwrap();
    assert_eq!(catalogs[0].0[0].revision, revision);
    assert!(catalogs[0].1.models.contains_key("fixture-model"));
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn native_catalog_preserves_upstream_objects_without_chat_menu_filtering() {
    use gateway_core::routing::ProviderModelContent;
    let upstream = MockServer::start().await;
    let fixture = Fixture::new(&upstream.uri());
    *fixture.store.primary().catalog.lock().unwrap() = None;
    let document = json!({
        "models": {
            "chat-model": {"displayName":"Chat","tagTitle":"New","tagDescription":"Leaving on Nov 30.","quotaInfo":{"remainingFraction":0.5}},
            "gemini-3.1-flash-image": {"displayName":"Image","unknownImageMetadata":{"future":true}},
            "gemini-3.5-flash-lite": {"displayName":"Search"},
            "future-helper": {"unknown":[1,{"newCapability":true}]}
        },
        "agentModelSorts":[{"groups":[{"modelIds":["chat-model"],"futureGroupField":"keep"}]}],
        "imageGenerationModelIds":["gemini-3.1-flash-image"],
        "webSearchModelIds":["gemini-3.5-flash-lite"],
        "futureCatalogField":{"values":[1,2,3]}
    });
    Mock::given(path("/v1internal:loadCodeAssist"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"cloudaicompanionProject":"fixture-project"})),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(path("/v1internal:fetchAvailableModels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&document))
        .expect(1)
        .mount(&upstream)
        .await;
    fixture
        .catalog
        .reconcile(&CancellationToken::new())
        .await
        .unwrap();
    let mut expected = document;
    expected["models"]["chat-model"]
        .as_object_mut()
        .unwrap()
        .remove("quotaInfo");
    {
        let stored = fixture.store.primary().catalog.lock().unwrap();
        let mut actual: serde_json::Value =
            serde_json::from_slice(&stored.as_ref().unwrap().1).unwrap();
        assert_eq!(
            actual.as_object_mut().unwrap().remove("project"),
            Some(json!("fixture-project"))
        );
        assert_eq!(actual, expected);
    }
    let entries = fixture
        .catalog
        .client_model_catalog(&fixture.store.account_scope())
        .await
        .unwrap();
    assert_eq!(entries.len(), expected["models"].as_object().unwrap().len());
    for entry in entries {
        let ProviderModelContent::Native(payload) = entry.content else {
            panic!("expected native model");
        };
        let actual: serde_json::Value = serde_json::from_slice(payload.body()).unwrap();
        assert_eq!(actual, expected["models"][entry.model.as_str()]);
    }
}

#[tokio::test]
async fn native_catalog_selects_whole_objects_only_from_authorized_sources() {
    use gateway_core::account::ProviderAccountStore;
    use gateway_core::routing::ProviderModelContent;
    let upstream = MockServer::start().await;
    let fixture = Fixture::with_two_accounts(&upstream.uri());
    for (accounts, catalog) in fixture.catalog.catalogs().await.unwrap() {
        let mut document = serde_json::to_value(catalog).unwrap();
        document["models"]["fixture-model"] = if accounts[0].account_id.as_str() == "acct_fixture" {
            json!({"displayName":"Primary","primaryOnly":true})
        } else {
            json!({"displayName":"Standby","standbyOnly":true})
        };
        assert!(
            fixture
                .store
                .save_account_catalog(&accounts[0], &serde_json::to_vec(&document).unwrap())
                .await
                .unwrap()
        );
    }
    let scope = fixture.store.account_scope();
    for (scope, expected) in [
        (
            scope.as_ref().clone(),
            json!({"displayName":"Primary","primaryOnly":true}),
        ),
        (
            scope.excluding_account(
                gateway_core::account::ProviderAccountId::new("acct_fixture").unwrap(),
            ),
            json!({"displayName":"Standby","standbyOnly":true}),
        ),
    ] {
        let entries = fixture.catalog.client_model_catalog(&scope).await.unwrap();
        assert_eq!(entries.len(), 1);
        let ProviderModelContent::Native(payload) = &entries[0].content else {
            panic!("expected native model");
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(payload.body()).unwrap(),
            expected
        );
    }
    assert!(upstream.received_requests().await.unwrap().is_empty());
}
