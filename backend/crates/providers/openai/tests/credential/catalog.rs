use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{TimeZone as _, Utc};
use futures::executor::block_on;
use gateway_core::engine::credential::{ProviderAccount, ProviderAccountId};
use gateway_core::provider_ports::ProviderCatalogCachePort;
use provider_openai::OFFICIAL_CODEX_BASE_URL;
use provider_openai::credential::{
    CodexCredentialCatalogError, CodexCredentialCatalogService, ImportCodexOAuthCredential,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::support::{MemoryAccountStore, catalog_cache, profile, secret};

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../transport/fixtures/official_models_snapshot.json");

struct ReplacingCatalogResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for ReplacingCatalogResponder {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let body = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            OFFICIAL_FIXTURE.to_vec()
        } else {
            br#"{"models":[{"slug":"gpt-5.5","display_name":"GPT-5.5"}]}"#.to_vec()
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_raw(body, "application/json")
    }
}

/// 按调用顺序为不同套餐 scope 返回不同的目录正文。
struct SequencedCatalogResponder {
    calls: Arc<AtomicUsize>,
    bodies: [&'static [u8]; 2],
}

impl Respond for SequencedCatalogResponder {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let index = self.calls.fetch_add(1, Ordering::SeqCst).min(1);
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_raw(self.bodies[index].to_vec(), "application/json")
    }
}

fn wire_profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "catalog-contract".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("fixture time"),
    })
}

fn service(store: &Arc<MemoryAccountStore>) -> CodexCredentialCatalogService {
    service_with_catalog_cache(store, OFFICIAL_CODEX_BASE_URL.to_owned(), catalog_cache())
}

fn service_with_catalog_cache(
    store: &Arc<MemoryAccountStore>,
    base_url: String,
    catalog_cache: Arc<dyn ProviderCatalogCachePort>,
) -> CodexCredentialCatalogService {
    CodexCredentialCatalogService::new(
        store.repository(),
        wire_profile(),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        base_url,
        catalog_cache,
    )
}

async fn seed_account(store: &Arc<MemoryAccountStore>, account_id: &str) -> ProviderAccount {
    seed_account_with_plan(store, account_id, "pro").await
}

async fn seed_account_with_plan(
    store: &Arc<MemoryAccountStore>,
    account_id: &str,
    plan: &str,
) -> ProviderAccount {
    let mut verified_account = profile(&format!("chatgpt-{account_id}"));
    verified_account.plan_type = Some(plan.to_owned());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            name: account_id.to_owned(),
            secret: secret(&format!("access-{account_id}")),
            verified_account,
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    store.account(account_id).expect("seeded account")
}

#[test]
fn catalog_starts_without_a_snapshot_or_generation() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);

    assert!(service.cached().expect("cache read").is_none());
    assert_eq!(service.catalog_generation().get(), 0);
}

#[test]
fn catalog_without_openai_accounts_fails_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());

    let error = block_on(service(&store).synchronize()).expect_err("empty account set");

    assert!(matches!(
        error,
        CodexCredentialCatalogError::NoEligibleCredential
    ));
}

#[tokio::test]
async fn missing_account_refresh_fails_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());
    let account = ProviderAccountId::new("acct_missing").expect("account id");

    let error = service(&store)
        .refresh_account_catalog(&account)
        .await
        .expect_err("missing account");

    assert!(matches!(
        error,
        CodexCredentialCatalogError::NoEligibleCredential
    ));
}

#[tokio::test]
async fn plan_catalog_cache_is_shared_and_manual_refresh_replaces_it() {
    let store = Arc::new(MemoryAccountStore::default());
    let first = seed_account(&store, "acct_catalog_a").await;
    let second = seed_account(&store, "acct_catalog_b").await;
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ReplacingCatalogResponder {
            calls: Arc::clone(&calls),
        })
        .expect(2)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let initial = service
        .cached_or_refresh_account_catalog(&first)
        .await
        .expect("cold cache fetch");
    let cached = service
        .cached_or_refresh_account_catalog(&second)
        .await
        .expect("shared cache hit");
    let refreshed = service
        .refresh_account_catalog(second.id())
        .await
        .expect("manual refresh");
    let replaced = service
        .cached_or_refresh_account_catalog(&first)
        .await
        .expect("replaced cache hit");

    assert_eq!(initial.models(), ["gpt-5.4"]);
    assert_eq!(cached.models(), ["gpt-5.4"]);
    assert_eq!(refreshed.models(), ["gpt-5.5"]);
    assert_eq!(replaced.models(), ["gpt-5.5"]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.verify().await;
}

#[tokio::test]
async fn plan_catalog_refresh_stops_after_three_failed_accounts() {
    let store = Arc::new(MemoryAccountStore::default());
    let accounts = [
        seed_account(&store, "acct_catalog_limit_a").await,
        seed_account(&store, "acct_catalog_limit_b").await,
        seed_account(&store, "acct_catalog_limit_c").await,
        seed_account(&store, "acct_catalog_limit_d").await,
    ];
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(503))
        .expect(3)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let error = service
        .refresh_account_catalog(accounts[0].id())
        .await
        .expect_err("three failed accounts stop the refresh");

    assert!(matches!(
        error,
        CodexCredentialCatalogError::Upstream { .. }
    ));
    server.verify().await;
}

#[tokio::test]
async fn cross_plan_presentation_drift_unions_on_the_first_seen_model() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_account_with_plan(&store, "acct_union_plus", "plus").await;
    seed_account_with_plan(&store, "acct_union_pro", "pro").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(SequencedCatalogResponder {
            calls: Arc::new(AtomicUsize::new(0)),
            bodies: [
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4 Plus","description":"plus copy","supported_in_api":true}]}"#,
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4 Pro","description":"pro copy","supported_in_api":true,"context_window":272000}]}"#,
            ],
        })
        .expect(2)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let snapshot = service
        .synchronize()
        .await
        .expect("presentation drift must not wedge the union");

    assert_eq!(snapshot.models().len(), 1);
    assert_eq!(snapshot.models()[0].display_name(), "GPT-5.4 Plus");
    server.verify().await;
}

#[tokio::test]
async fn cross_plan_supported_in_api_drift_unions_on_the_first_seen_model() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_account_with_plan(&store, "acct_conflict_plus", "plus").await;
    seed_account_with_plan(&store, "acct_conflict_pro", "pro").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(SequencedCatalogResponder {
            calls: Arc::new(AtomicUsize::new(0)),
            bodies: [
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4","supported_in_api":true}]}"#,
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4","supported_in_api":false}]}"#,
            ],
        })
        .expect(2)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let snapshot = service
        .synchronize()
        .await
        .expect("supported_in_api drift must not block catalog publication");

    assert_eq!(snapshot.models().len(), 1);
    server.verify().await;
}

#[tokio::test]
async fn free_and_k12_catalog_entitlements_are_isolated_by_plan_scope() {
    let store = Arc::new(MemoryAccountStore::default());
    let first_free = seed_account_with_plan(&store, "acct_free_a", "free").await;
    let second_free = seed_account_with_plan(&store, "acct_free_b", "free").await;
    let k12 = seed_account_with_plan(&store, "acct_k12", "k12").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(SequencedCatalogResponder {
            calls: Arc::new(AtomicUsize::new(0)),
            bodies: [
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4","supported_in_api":true}]}"#,
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4","supported_in_api":true},{"slug":"gpt-5.6-sol","display_name":"GPT-5.6 Sol","supported_in_api":true}]}"#,
            ],
        })
        .expect(2)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let snapshot = service.synchronize().await.expect("mixed-plan catalog");

    assert_eq!(snapshot.models().len(), 2);
    assert_eq!(
        service
            .cached_account_models(&first_free)
            .expect("first free entitlement"),
        Some(vec!["gpt-5.4".to_owned()])
    );
    assert_eq!(
        service
            .cached_account_models(&second_free)
            .expect("second free entitlement"),
        Some(vec!["gpt-5.4".to_owned()])
    );
    assert_eq!(
        service
            .observed_model_support(&first_free, "gpt-5.6-sol")
            .expect("free support fact"),
        Some(false)
    );
    assert_eq!(
        service
            .observed_model_support(&k12, "gpt-5.6-sol")
            .expect("K12 support fact"),
        Some(true)
    );
    let synchronized_generation = service.catalog_generation().get();
    service.invalidate().expect("invalidate mixed-plan catalog");
    assert!(
        service
            .cached()
            .expect("cache read after invalidation")
            .is_none()
    );
    assert_eq!(
        service.catalog_generation().get(),
        synchronized_generation + 1
    );
    assert_eq!(
        service
            .observed_model_support(&k12, "gpt-5.6-sol")
            .expect("support fact after invalidation"),
        None
    );
    server.verify().await;
}

#[tokio::test]
async fn response_etag_change_is_deduplicated_and_queued_once() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);

    assert!(
        service
            .observe_response_etag("\"models-v2\"")
            .expect("new ETag")
    );
    assert!(
        !service
            .observe_response_etag("\"models-v2\"")
            .expect("duplicate ETag")
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        service.wait_for_etag_refresh(),
    )
    .await
    .expect("queued ETag refresh");
}

#[tokio::test]
async fn periodic_catalog_refresh_does_not_complete_an_etag_refresh_it_did_not_claim() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);

    assert!(
        service
            .observe_response_etag("\"models-v2\"")
            .expect("new ETag")
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        service.wait_for_etag_refresh(),
    )
    .await
    .expect("ETag daemon claims the refresh");

    assert!(matches!(
        service.refresh_catalogs().await,
        Err(CodexCredentialCatalogError::NoEligibleCredential)
    ));
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            service.wait_for_etag_refresh(),
        )
        .await
        .is_err(),
        "periodic refresh must not requeue or finish the daemon-owned ETag"
    );
}

#[test]
fn invalid_response_etag_is_rejected_without_touching_the_catalog() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);

    let error = service
        .observe_response_etag("invalid\netag")
        .expect_err("invalid ETag");

    assert!(matches!(error, CodexCredentialCatalogError::InvalidEtag));
    assert!(service.cached().expect("cache read").is_none());
    assert_eq!(service.catalog_generation().get(), 0);
}
