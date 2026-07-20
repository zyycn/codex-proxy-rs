use std::sync::Arc;
use std::time::SystemTime;

use chrono::{TimeZone, Utc};
use futures::executor::block_on;
use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, CredentialRevision, ProviderAccountId,
    ProviderAccountStore as _,
};
use gateway_core::routing::{InstanceHealth, ProviderInstance, ProviderKind};
use provider_openai::credential::{
    CodexCredentialCatalogError, CodexCredentialCatalogService, CreateCodexCredential,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{MemoryAccountStore, instance_id, loopback_origin_policy, profile, secret};

fn wire_profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "xterm".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("fixture time"),
    })
}

fn instance(base_url: &str) -> ProviderInstance {
    ProviderInstance::new(
        instance_id(),
        ProviderKind::new("openai").expect("provider"),
        base_url.to_owned(),
        true,
        InstanceHealth::Healthy,
    )
}

async fn create(store: &Arc<MemoryAccountStore>, id: &str, token: &str) {
    store
        .repository()
        .create_oauth_credential(CreateCodexCredential {
            account_id: id.to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: id.to_owned(),
            secret: secret(token),
            account: profile(&format!("chatgpt-{id}")),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await
        .expect("create account");
}

fn service(store: &Arc<MemoryAccountStore>) -> CodexCredentialCatalogService {
    CodexCredentialCatalogService::new(
        store.repository(),
        wire_profile(),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        loopback_origin_policy(),
    )
}

fn catalog(model: &str, display: &str) -> serde_json::Value {
    serde_json::json!({
        "models": [{
            "slug": model,
            "display_name": display,
            "supported_in_api": true,
            "supported_reasoning_levels": [{"effort": "medium"}],
            "context_window": 128000
        }]
    })
}

fn account_id(value: &str) -> ProviderAccountId {
    ProviderAccountId::new(value).expect("account id")
}

fn revision() -> CredentialRevision {
    CredentialRevision::new(1).expect("revision")
}

#[tokio::test]
async fn realtime_catalog_builds_union_and_per_account_entitlements() {
    let server = MockServer::start().await;
    for (token, model) in [("token-one", "gpt-5.4"), ("token-two", "gpt-5.5")] {
        Mock::given(method("GET"))
            .and(path("/backend-api/codex/models"))
            .and(header("authorization", format!("Bearer {token}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalog(model, model)))
            .expect(1)
            .mount(&server)
            .await;
    }
    let store = Arc::new(MemoryAccountStore::default());
    create(&store, "acct_one", "token-one").await;
    create(&store, "acct_two", "token-two").await;
    let snapshot = service(&store)
        .synchronize_instance(&instance(&format!("{}/backend-api", server.uri())))
        .await
        .expect("synchronize catalog");
    assert_eq!(snapshot.models().len(), 2);
    assert_eq!(
        snapshot.account_models(&account_id("acct_one"), revision()),
        Some(&["gpt-5.4".to_owned()][..])
    );
    assert_eq!(
        snapshot.account_models(&account_id("acct_two"), revision()),
        Some(&["gpt-5.5".to_owned()][..])
    );
}

#[tokio::test]
async fn realtime_catalog_should_preserve_the_official_model_order() {
    let server = MockServer::start().await;
    let official_order = ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.4"];
    Mock::given(method("GET"))
        .and(path("/backend-api/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "models": official_order
                .iter()
                .enumerate()
                .map(|(index, model)| serde_json::json!({
                    "slug": model,
                    "display_name": model,
                    "priority": index + 1,
                    "supported_in_api": true,
                    "supported_reasoning_levels": [{"effort": "medium"}],
                    "context_window": 128000
                }))
                .collect::<Vec<_>>()
        })))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create(&store, "acct_order", "token-order").await;
    let snapshot = service(&store)
        .synchronize_instance(&instance(&format!("{}/backend-api", server.uri())))
        .await
        .expect("ordered catalog");

    assert_eq!(
        snapshot
            .models()
            .iter()
            .map(|model| model.request_model().as_str())
            .collect::<Vec<_>>(),
        official_order
    );
    assert_eq!(
        snapshot.account_models(&account_id("acct_order"), revision()),
        Some(official_order.map(str::to_owned).as_slice())
    );
}

#[tokio::test]
async fn catalog_is_reused_until_explicit_invalidation_or_etag_change() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog("gpt-5.4", "GPT-5.4")))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create(&store, "acct_one", "token-one").await;
    let service = service(&store);
    let instance = instance(&format!("{}/backend-api", server.uri()));
    let first = service
        .synchronize_instance(&instance)
        .await
        .expect("first query");
    let second = service
        .synchronize_instance(&instance)
        .await
        .expect("cached query");
    assert_eq!(first.observed_at(), second.observed_at());
    assert!(service.cached(instance.id()).expect("cache read").is_some());
}

#[tokio::test]
async fn single_account_refresh_queries_only_that_account_and_updates_its_cache() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/codex/models"))
        .and(header("authorization", "Bearer token-two"))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog("gpt-5.5", "GPT-5.5")))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create(&store, "acct_one", "token-one").await;
    create(&store, "acct_two", "token-two").await;
    let service = service(&store);
    let instance = instance(&format!("{}/backend-api", server.uri()));
    let selected = ProviderAccountId::new("acct_two").expect("account id");
    let models = service
        .synchronize_account(&instance, &selected)
        .await
        .expect("single account catalog");
    assert_eq!(models, ["gpt-5.5"]);
    assert_eq!(
        service
            .cached_account_models(instance.id(), &selected, revision())
            .expect("cache read"),
        Some(vec!["gpt-5.5".to_owned()])
    );
    assert_eq!(
        service
            .cached_account_models(instance.id(), &account_id("acct_one"), revision(),)
            .expect("other cache read"),
        None
    );
}

#[tokio::test]
async fn invalidation_forces_a_new_realtime_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog("gpt-5.4", "GPT-5.4")))
        .expect(2)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create(&store, "acct_one", "token-one").await;
    let service = service(&store);
    let instance = instance(&format!("{}/backend-api", server.uri()));
    assert_eq!(service.catalog_generation().get(), 0);
    service
        .synchronize_instance(&instance)
        .await
        .expect("first query");
    assert_eq!(service.catalog_generation().get(), 1);
    service.invalidate(instance.id()).expect("invalidate");
    assert_eq!(service.catalog_generation().get(), 2);
    service
        .synchronize_instance(&instance)
        .await
        .expect("second query");
    assert_eq!(service.catalog_generation().get(), 3);
}

#[tokio::test]
async fn response_etag_change_is_deduplicated_and_queued_for_refresh() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/codex/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"models-v1\"")
                .set_body_json(catalog("gpt-5.4", "GPT-5.4")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create(&store, "acct_one", "token-one").await;
    let service = service(&store);
    let instance = instance(&format!("{}/backend-api", server.uri()));
    service
        .synchronize_instance(&instance)
        .await
        .expect("initial catalog");

    assert!(
        !service
            .observe_response_etag(instance.id(), "\"models-v1\"")
            .expect("same ETag")
    );
    assert!(
        service
            .observe_response_etag(instance.id(), "\"models-v2\"")
            .expect("changed ETag")
    );
    assert!(
        !service
            .observe_response_etag(instance.id(), "\"models-v2\"")
            .expect("duplicate ETag")
    );
    assert_eq!(
        service.wait_for_etag_refresh().await,
        vec![instance.id().clone()]
    );
}

#[tokio::test]
async fn quota_exhausted_account_remains_eligible_for_catalog_discovery() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog("gpt-5.4", "GPT-5.4")))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create(&store, "acct_quota", "token-one").await;
    let account = store.account("acct_quota").expect("created account");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            reason: Some("quota exhausted".to_owned()),
            cooldown_until: None,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark quota exhausted");

    let snapshot = service(&store)
        .synchronize_instance(&instance(&format!("{}/backend-api", server.uri())))
        .await
        .expect("discover catalog through quota exhausted account");

    assert_eq!(snapshot.models().len(), 1);
}

#[test]
fn catalog_without_schedulable_accounts_fails_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());
    let error =
        block_on(service(&store).synchronize_instance(&instance("http://127.0.0.1:9/backend-api")))
            .expect_err("empty account set");
    assert!(matches!(
        error,
        CodexCredentialCatalogError::NoEligibleCredential
    ));
}
