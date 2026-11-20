//! 验证 xAI 目录缓存、并发合并与指定账号刷新

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
use gateway_core::account::{
    CredentialRevision, OpaqueProviderData, ProviderAccountStore, ProviderAccountUpdate,
    QuotaAccessChange, QuotaEvidence, QuotaState,
};
use gateway_core::provider_ports::{
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderStoreError,
};
use provider_xai::{
    GrokCatalogCache, GrokCatalogScope, GrokCredentialCatalogCache, GrokCredentialCatalogError,
    GrokCredentialCatalogSeed, GrokCredentialRepository, GrokModelCatalogRequest,
    GrokModelCatalogTransport, GrokModelCatalogTransportError, GrokModelCatalogTransportErrorKind,
    GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse, GrokPlanCatalog,
};

use crate::support::{
    MemoryGrokCatalogCache, MemoryProviderAccountStore, account_id, create_input, seed_input,
};

const CLI_PROXY_FIXTURE: &[u8] =
    include_bytes!("../transport/catalog/fixtures/cli_proxy_models.json");

struct QueueCatalogTransport {
    calls: AtomicUsize,
    responses:
        Mutex<VecDeque<Result<GrokModelCatalogTransportResponse, GrokModelCatalogTransportError>>>,
}

impl QueueCatalogTransport {
    fn from_bodies(bodies: impl IntoIterator<Item = Vec<u8>>) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            responses: Mutex::new(
                bodies
                    .into_iter()
                    .map(|body| Ok(GrokModelCatalogTransportResponse::new(body, None)))
                    .collect(),
            ),
        })
    }

    fn failure() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            responses: Mutex::new(VecDeque::from([Err(GrokModelCatalogTransportError::new(
                GrokModelCatalogTransportErrorKind::Unavailable,
            ))])),
        })
    }

    fn from_results(
        responses: impl IntoIterator<
            Item = Result<GrokModelCatalogTransportResponse, GrokModelCatalogTransportError>,
        >,
    ) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            responses: Mutex::new(responses.into_iter().collect()),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl GrokModelCatalogTransport for QueueCatalogTransport {
    fn execute(&self, _: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = self
            .responses
            .lock()
            .expect("response queue")
            .pop_front()
            .expect("one response per account");
        Box::pin(async move { response })
    }
}

struct CorruptCatalogCachePort;

impl ProviderCatalogCachePort for CorruptCatalogCachePort {
    fn replace<'a>(
        &'a self,
        _key: &'a ProviderCatalogCacheKey,
        _catalog: &'a OpaqueProviderData,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn read<'a>(
        &'a self,
        _key: &'a ProviderCatalogCacheKey,
    ) -> BoxFuture<'a, Result<Option<OpaqueProviderData>, ProviderStoreError>> {
        Box::pin(async {
            let mut document = serde_json::Map::new();
            document.insert("version".to_owned(), serde_json::json!("corrupt"));
            Ok(Some(OpaqueProviderData::new(document)))
        })
    }
}

async fn repository_with_accounts(
    suffixes: &[(&str, &str)],
) -> (Arc<MemoryProviderAccountStore>, GrokCredentialRepository) {
    let store = MemoryProviderAccountStore::shared();
    let account_store: Arc<dyn ProviderAccountStore> = store.clone();
    let repository = GrokCredentialRepository::new(account_store);
    for (suffix, subject) in suffixes {
        seed_input(&store, &create_input(suffix, subject))
            .await
            .expect("create account");
    }
    (store, repository)
}

#[tokio::test]
async fn catalog_query_caches_each_plan_and_returns_strict_union() {
    let (store, repository) =
        repository_with_accounts(&[("catalog-a", "subject-a"), ("catalog-b", "subject-b")]).await;
    let cache = MemoryGrokCatalogCache::shared();
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = cache.clone();
    let transport = QueueCatalogTransport::from_bodies([
        CLI_PROXY_FIXTURE.to_vec(),
        CLI_PROXY_FIXTURE.to_vec(),
        CLI_PROXY_FIXTURE.to_vec(),
        CLI_PROXY_FIXTURE.to_vec(),
    ]);
    let service = crate::support::grok_catalog_service(repository, transport, cache_port);
    assert_eq!(service.catalog_generation().get(), 0);
    let models = service.query_models().await.expect("catalog sync");
    assert_eq!(service.catalog_generation().get(), 1);
    service.query_models().await.expect("same catalog sync");
    assert_eq!(service.catalog_generation().get(), 1);
    store
        .set_enabled(&account_id("catalog-b"), false)
        .await
        .unwrap();
    let remaining = service
        .query_models()
        .await
        .expect("same model, one remaining source");
    assert_eq!(service.catalog_generation().get(), 2);
    assert_eq!(remaining[0].1, [account_id("catalog-a")].into());
    assert_eq!(
        models[0].1,
        [account_id("catalog-a"), account_id("catalog-b")].into()
    );

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].0.request_model().as_str(), "grok-4.5");
    let account = store
        .account(&account_id("catalog-a"))
        .expect("created account");
    let scope = GrokCatalogScope::for_account(&account).expect("catalog scope");
    assert_eq!(
        cache
            .observed_model_support(&scope, "grok-4.5")
            .await
            .expect("cache lookup"),
        Some(true)
    );
}

#[tokio::test]
async fn single_account_catalog_refresh_and_read_use_provider_cache_boundary() {
    let (store, repository) =
        repository_with_accounts(&[("account-models", "subject-models")]).await;
    let cache = MemoryGrokCatalogCache::shared();
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = cache;
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::from_bodies([CLI_PROXY_FIXTURE.to_vec()]),
        cache_port,
    );
    let refreshed = service
        .refresh_account_catalog(&account_id("account-models"))
        .await
        .expect("refresh one account catalog");
    assert_eq!(refreshed.seed().models(), ["grok-4.5"]);

    let cached = service
        .read_account_catalog(
            &store
                .account(&account_id("account-models"))
                .expect("created account"),
        )
        .await
        .expect("read cache")
        .expect("cached catalog");
    assert_eq!(cached.seed().models(), ["grok-4.5"]);
}

#[tokio::test]
async fn disabled_account_refresh_discovers_models_with_the_pinned_account() {
    let (store, repository) =
        repository_with_accounts(&[("disabled-models", "subject-disabled-models")]).await;
    store
        .set_enabled(&account_id("disabled-models"), false)
        .await
        .expect("disable account");
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::from_bodies([CLI_PROXY_FIXTURE.to_vec()]),
        MemoryGrokCatalogCache::shared(),
    );

    let refreshed = service
        .refresh_account_catalog(&account_id("disabled-models"))
        .await
        .expect("disabled account refresh still discovers models");
    assert_eq!(refreshed.seed().models(), ["grok-4.5"]);
}

#[tokio::test]
async fn single_account_catalog_read_miss_does_not_call_upstream() {
    let (store, repository) =
        repository_with_accounts(&[("account-models-miss", "subject-models")]).await;
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::failure(),
        MemoryGrokCatalogCache::shared(),
    );
    assert!(
        service
            .read_account_catalog(
                &store
                    .account(&account_id("account-models-miss"))
                    .expect("created account"),
            )
            .await
            .expect("read cache")
            .is_none()
    );
}

#[tokio::test]
async fn corrupt_catalog_cache_entry_reads_as_miss_and_refetches() {
    let (store, repository) =
        repository_with_accounts(&[("catalog-corrupt", "subject-corrupt")]).await;
    let account = store
        .account(&account_id("catalog-corrupt"))
        .expect("created account");
    let scope = GrokCatalogScope::for_account(&account).expect("catalog scope");
    let cache = GrokCatalogCache::new(Arc::new(CorruptCatalogCachePort)).expect("provider cache");
    assert!(
        cache
            .read(&scope)
            .await
            .expect("corrupt cache entry must degrade to a miss")
            .is_none()
    );

    let transport = QueueCatalogTransport::from_bodies([CLI_PROXY_FIXTURE.to_vec()]);
    let service =
        crate::support::grok_catalog_service(repository, transport.clone(), Arc::new(cache));
    let catalog = service
        .cached_or_refresh_account_catalog(&account)
        .await
        .expect("corrupt cache entry falls back to a live refresh");

    assert_eq!(catalog.seed().models(), ["grok-4.5"]);
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn cached_account_catalog_uses_shared_plan_cache_without_upstream_call() {
    let (store, repository) =
        repository_with_accounts(&[("catalog-cache-hit", "subject-cache-hit")]).await;
    let account = store
        .account(&account_id("catalog-cache-hit"))
        .expect("created account");
    let scope = GrokCatalogScope::for_account(&account).expect("catalog scope");
    let cache = MemoryGrokCatalogCache::shared();
    cache
        .replace(GrokPlanCatalog::new(
            scope,
            chrono::Utc::now(),
            GrokCredentialCatalogSeed::new(["grok-4.5"], None).expect("seed"),
        ))
        .await
        .expect("cache catalog");
    let transport = QueueCatalogTransport::from_bodies([]);
    let service = crate::support::grok_catalog_service(repository, transport.clone(), cache);

    let catalog = service
        .cached_or_refresh_account_catalog(&account)
        .await
        .expect("cached catalog");

    assert_eq!(catalog.seed().models(), ["grok-4.5"]);
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn manual_catalog_refresh_replaces_the_shared_plan_cache() {
    let (store, repository) =
        repository_with_accounts(&[("catalog-manual-refresh", "subject-manual-refresh")]).await;
    let account = store
        .account(&account_id("catalog-manual-refresh"))
        .expect("created account");
    let scope = GrokCatalogScope::for_account(&account).expect("catalog scope");
    let cache = MemoryGrokCatalogCache::shared();
    cache
        .replace(GrokPlanCatalog::new(
            scope,
            chrono::Utc::now(),
            GrokCredentialCatalogSeed::new(["grok-before-refresh"], None).expect("seed"),
        ))
        .await
        .expect("cache catalog");
    let transport = QueueCatalogTransport::from_bodies([CLI_PROXY_FIXTURE.to_vec()]);
    let service = crate::support::grok_catalog_service(repository, transport.clone(), cache);

    let refreshed = service
        .refresh_account_catalog(account.id())
        .await
        .expect("manual refresh");

    assert_eq!(refreshed.seed().models(), ["grok-4.5"]);
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn catalog_refresh_falls_back_within_the_same_plan() {
    let (_, repository) = repository_with_accounts(&[
        ("catalog-fallback-a", "subject-fallback-a"),
        ("catalog-fallback-b", "subject-fallback-b"),
    ])
    .await;
    let transport = QueueCatalogTransport::from_results([
        Err(GrokModelCatalogTransportError::new(
            GrokModelCatalogTransportErrorKind::Unavailable,
        )),
        Ok(GrokModelCatalogTransportResponse::new(
            CLI_PROXY_FIXTURE.to_vec(),
            None,
        )),
    ]);
    let service = crate::support::grok_catalog_service(
        repository,
        transport.clone(),
        MemoryGrokCatalogCache::shared(),
    );

    let catalog = service
        .refresh_account_catalog(&account_id("catalog-fallback-a"))
        .await
        .expect("second account fills the plan catalog");

    assert_eq!(catalog.seed().models(), ["grok-4.5"]);
    assert_eq!(transport.calls(), 2);
}

#[tokio::test]
async fn catalog_refresh_stops_after_three_failed_accounts_in_one_plan() {
    let (_, repository) = repository_with_accounts(&[
        ("catalog-limit-a", "subject-limit-a"),
        ("catalog-limit-b", "subject-limit-b"),
        ("catalog-limit-c", "subject-limit-c"),
        ("catalog-limit-d", "subject-limit-d"),
    ])
    .await;
    let transport = QueueCatalogTransport::from_results((0..3).map(|_| {
        Err(GrokModelCatalogTransportError::new(
            GrokModelCatalogTransportErrorKind::Unavailable,
        ))
    }));
    let service = crate::support::grok_catalog_service(
        repository,
        transport.clone(),
        MemoryGrokCatalogCache::shared(),
    );

    let error = service
        .refresh_account_catalog(&account_id("catalog-limit-a"))
        .await
        .expect_err("three failures stop the refresh");

    assert!(matches!(error, GrokCredentialCatalogError::Upstream));
    assert_eq!(transport.calls(), 3);
}

#[tokio::test]
async fn disabled_accounts_are_not_sent_to_catalog_transport() {
    let (store, repository) = repository_with_accounts(&[("disabled", "subject-disabled")]).await;
    store
        .set_enabled(&account_id("disabled"), false)
        .await
        .expect("disable");
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = MemoryGrokCatalogCache::shared();
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::from_bodies([]),
        cache_port,
    );
    assert!(matches!(
        service.query_models().await,
        Err(GrokCredentialCatalogError::NoEligibleCredential)
    ));
}

#[tokio::test]
async fn quota_exhausted_account_remains_eligible_for_catalog_discovery() {
    let (store, repository) =
        repository_with_accounts(&[("quota-exhausted", "subject-quota-exhausted")]).await;
    let id = account_id("quota-exhausted");
    store
        .apply_quota_access(QuotaAccessChange {
            account_id: id,
            expected_revision: CredentialRevision::new(1).expect("revision"),
            state: QuotaState::exhausted(QuotaEvidence::ProviderDenied, SystemTime::now(), None),
        })
        .await
        .expect("mark quota exhausted");
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::from_bodies([CLI_PROXY_FIXTURE.to_vec()]),
        MemoryGrokCatalogCache::shared(),
    );

    let models = service
        .query_models()
        .await
        .expect("discover catalog through quota exhausted account");

    assert_eq!(models.len(), 1);
}

#[tokio::test]
async fn sole_plan_failure_rejects_the_catalog_cycle() {
    let (_, repository) = repository_with_accounts(&[("failed", "subject-failed")]).await;
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = MemoryGrokCatalogCache::shared();
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::failure(),
        cache_port,
    );
    assert!(matches!(
        service.query_models().await,
        Err(GrokCredentialCatalogError::Upstream)
    ));
}

#[tokio::test]
async fn failed_plan_scope_is_skipped_and_surviving_plans_still_cache() {
    let store = MemoryProviderAccountStore::shared();
    let account_store: Arc<dyn ProviderAccountStore> = store.clone();
    let repository = GrokCredentialRepository::new(account_store);
    // "plan:pro" 按 scope 排序在 "plan:standard" 之前，先消费失败响应
    let mut pro = create_input("plan-pro", "subject-pro");
    pro.account.plan_type = Some("pro".to_owned());
    seed_input(&store, &pro).await.expect("create pro account");
    seed_input(&store, &create_input("plan-standard", "subject-standard"))
        .await
        .expect("create standard account");
    let transport = QueueCatalogTransport::from_results([
        Err(GrokModelCatalogTransportError::new(
            GrokModelCatalogTransportErrorKind::Unavailable,
        )),
        Ok(GrokModelCatalogTransportResponse::new(
            CLI_PROXY_FIXTURE.to_vec(),
            None,
        )),
    ]);
    let cache = MemoryGrokCatalogCache::shared();
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = cache.clone();
    let service = crate::support::grok_catalog_service(repository, transport.clone(), cache_port);

    let models = service
        .query_models()
        .await
        .expect("surviving plan still refreshes the catalog");

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].0.request_model().as_str(), "grok-4.5");
    assert_eq!(transport.calls(), 2);
    let standard = store
        .account(&account_id("plan-standard"))
        .expect("created account");
    let standard_scope = GrokCatalogScope::for_account(&standard).expect("catalog scope");
    assert_eq!(
        cache
            .observed_model_support(&standard_scope, "grok-4.5")
            .await
            .expect("cache lookup"),
        Some(true)
    );
    let pro = store
        .account(&account_id("plan-pro"))
        .expect("created account");
    let pro_scope = GrokCatalogScope::for_account(&pro).expect("catalog scope");
    assert_eq!(
        cache
            .observed_model_support(&pro_scope, "grok-4.5")
            .await
            .expect("cache lookup"),
        None
    );
}

#[tokio::test]
async fn conflicting_facts_for_same_slug_fail_closed() {
    let (store, repository) =
        repository_with_accounts(&[("conflict-a", "subject-a"), ("conflict-b", "subject-b")]).await;
    let account_id = account_id("conflict-b");
    let account = store.account(&account_id).expect("created account");
    store
        .update_account(ProviderAccountUpdate {
            account_id,
            name: account.name().to_owned(),
            email: account.email().map(str::to_owned),
            plan_type: Some("premium".to_owned()),
        })
        .await
        .expect("separate plan catalog");
    let mut conflicting: serde_json::Value =
        serde_json::from_slice(CLI_PROXY_FIXTURE).expect("fixture JSON");
    conflicting["data"][0]["name"] = serde_json::json!("Different name");
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::from_bodies([
            CLI_PROXY_FIXTURE.to_vec(),
            serde_json::to_vec(&conflicting).expect("conflicting JSON"),
        ]),
        MemoryGrokCatalogCache::shared(),
    );
    assert!(matches!(
        service.query_models().await,
        Err(GrokCredentialCatalogError::ConflictingModelFacts)
    ));
}

#[test]
fn seed_rejects_duplicates_and_supports_exact_membership() {
    assert!(matches!(
        GrokCredentialCatalogSeed::new(["grok-4.5", "grok-4.5"], None),
        Err(GrokCredentialCatalogError::ConflictingModelFacts)
    ));
    let seed =
        GrokCredentialCatalogSeed::new(["grok-4.5", "grok-code-fast-1"], None).expect("valid seed");
    assert!(seed.permits("grok-4.5"));
    assert!(!seed.permits("grok-4"));
}
