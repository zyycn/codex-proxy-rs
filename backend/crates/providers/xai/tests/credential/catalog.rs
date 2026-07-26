use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::future::{BoxFuture, join_all};
use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, CredentialCasUpdate, CredentialRevision,
    OpaqueProviderData, ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate,
    QuotaObservation,
};
use gateway_core::provider_ports::{
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderStoreError,
};
use provider_xai::{
    GrokBillingRequest, GrokBillingTransport, GrokBillingTransportError,
    GrokBillingTransportErrorKind, GrokBillingTransportFuture, GrokBillingTransportResponse,
    GrokCatalogCache, GrokCatalogScope, GrokCredentialCatalogCache, GrokCredentialCatalogError,
    GrokCredentialCatalogSeed, GrokCredentialRepository, GrokModelCatalogRequest,
    GrokModelCatalogTransport, GrokModelCatalogTransportError, GrokModelCatalogTransportErrorKind,
    GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse, GrokPlanCatalog,
    GrokQuotaError, SecretValue,
};

use crate::support::{
    MemoryGrokCatalogCache, MemoryProviderAccountStore, account_id, create_input, seed_input,
};

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../transport/catalog/fixtures/official_grok_models_snapshot.json");

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

struct QueueBillingTransport {
    calls: AtomicUsize,
    responses: Mutex<VecDeque<Result<GrokBillingTransportResponse, GrokBillingTransportError>>>,
}

impl QueueBillingTransport {
    fn success(body: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            responses: Mutex::new(VecDeque::from([Ok(GrokBillingTransportResponse::new(
                body,
            ))])),
        })
    }

    fn failure() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            responses: Mutex::new(VecDeque::from([Err(GrokBillingTransportError::new(
                GrokBillingTransportErrorKind::Unavailable,
            ))])),
        })
    }
}

impl GrokBillingTransport for QueueBillingTransport {
    fn execute(&self, _: GrokBillingRequest) -> GrokBillingTransportFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = self
            .responses
            .lock()
            .expect("billing response queue")
            .pop_front()
            .expect("one billing response");
        Box::pin(async move { response })
    }
}

/// 每次读取都返回同一份无法解码的 catalog 文档的存储端口。
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

enum BillingMutation {
    State(AccountStateChange),
    Credential(CredentialCasUpdate),
}

struct MutatingBillingTransport {
    store: Arc<MemoryProviderAccountStore>,
    mutation: Mutex<Option<BillingMutation>>,
    body: Vec<u8>,
}

impl GrokBillingTransport for MutatingBillingTransport {
    fn execute(&self, _: GrokBillingRequest) -> GrokBillingTransportFuture<'_> {
        let store = Arc::clone(&self.store);
        let mutation = self.mutation.lock().expect("mutation").take();
        let body = self.body.clone();
        Box::pin(async move {
            match mutation.expect("one mutation per request") {
                BillingMutation::State(change) => store
                    .apply_state_change(change)
                    .await
                    .expect("apply concurrent state"),
                BillingMutation::Credential(update) => {
                    store
                        .compare_and_swap_credential(update)
                        .await
                        .expect("apply concurrent credential rotation");
                }
            }
            Ok(GrokBillingTransportResponse::new(body))
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

async fn set_account_state(
    store: &MemoryProviderAccountStore,
    id: &ProviderAccountId,
    availability: AccountAvailability,
    cooldown_until: Option<SystemTime>,
) {
    store
        .apply_state_change(AccountStateChange {
            account_id: id.clone(),
            expected_revision: CredentialRevision::new(1).expect("revision"),
            availability,
            reason: Some("test isolation".to_owned()),
            cooldown_until,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("set account state");
}

#[tokio::test]
async fn concurrent_cold_scheduling_hydration_reads_quota_once() {
    let (store, repository) =
        repository_with_accounts(&[("quota-hydration", "subject-hydration")]).await;
    let account = store
        .account(&account_id("quota-hydration"))
        .expect("created account");
    let service = crate::support::grok_quota_service(repository, QueueBillingTransport::failure());

    join_all((0..32).map(|_| service.prepare_scheduling(std::slice::from_ref(&account)))).await;

    assert_eq!(store.quota_reads(), 1);
}

#[tokio::test]
async fn catalog_query_caches_each_plan_and_returns_strict_union() {
    let (store, repository) =
        repository_with_accounts(&[("catalog-a", "subject-a"), ("catalog-b", "subject-b")]).await;
    let cache = MemoryGrokCatalogCache::shared();
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = cache.clone();
    let transport = QueueCatalogTransport::from_bodies([
        OFFICIAL_FIXTURE.to_vec(),
        OFFICIAL_FIXTURE.to_vec(),
        OFFICIAL_FIXTURE.to_vec(),
        OFFICIAL_FIXTURE.to_vec(),
    ]);
    let service = crate::support::grok_catalog_service(repository, transport, cache_port);
    assert_eq!(service.catalog_generation().get(), 0);
    let models = service.query_models().await.expect("catalog sync");
    assert_eq!(service.catalog_generation().get(), 1);
    service.query_models().await.expect("same catalog sync");
    assert_eq!(service.catalog_generation().get(), 1);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].request_model().as_str(), "grok-4.5");
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
        QueueCatalogTransport::from_bodies([OFFICIAL_FIXTURE.to_vec()]),
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

    let transport = QueueCatalogTransport::from_bodies([OFFICIAL_FIXTURE.to_vec()]);
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
    let transport = QueueCatalogTransport::from_bodies([OFFICIAL_FIXTURE.to_vec()]);
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
            OFFICIAL_FIXTURE.to_vec(),
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
        .apply_state_change(AccountStateChange {
            account_id: id,
            expected_revision: CredentialRevision::new(1).expect("revision"),
            availability: AccountAvailability::QuotaExhausted,
            reason: Some("quota exhausted".to_owned()),
            cooldown_until: None,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark quota exhausted");
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::from_bodies([OFFICIAL_FIXTURE.to_vec()]),
        MemoryGrokCatalogCache::shared(),
    );

    let models = service
        .query_models()
        .await
        .expect("discover catalog through quota exhausted account");

    assert_eq!(models.len(), 1);
}

#[tokio::test]
async fn one_upstream_failure_rejects_the_whole_catalog_cycle() {
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
        serde_json::from_slice(OFFICIAL_FIXTURE).expect("fixture JSON");
    conflicting["data"][0]["name"] = serde_json::json!("Different name");
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::from_bodies([
            OFFICIAL_FIXTURE.to_vec(),
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

#[tokio::test]
async fn fetch_seed_rejects_non_header_safe_identity() {
    let (_, repository) = repository_with_accounts(&[]).await;
    let service = crate::support::grok_catalog_service(
        repository,
        QueueCatalogTransport::from_bodies([OFFICIAL_FIXTURE.to_vec()]),
        MemoryGrokCatalogCache::shared(),
    );
    assert!(matches!(
        service
            .fetch_seed(
                SecretValue::new("access"),
                SecretValue::new("非-ascii"),
                None,
            )
            .await,
        Err(GrokCredentialCatalogError::InvalidCredentialData)
    ));
}

#[tokio::test]
async fn quota_refresh_persists_dynamic_provider_document_and_projects_known_fields() {
    let (store, repository) = repository_with_accounts(&[("quota", "subject-quota")]).await;
    let transport = QueueBillingTransport::success(
        br#"{"config":{"creditUsagePercent":37.5,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-07-13T00:00:00Z","end":"2026-07-20T00:00:00Z"},"prepaidBalance":{"val":2500},"futureWindow":{"kind":"rolling"}}}"#,
    );
    let service = crate::support::grok_quota_service(repository, transport.clone());

    let snapshot = service
        .refresh_account(&account_id("quota"))
        .await
        .expect("refresh quota");
    let persisted = store
        .get_quotas(&[account_id("quota")])
        .await
        .expect("read persisted quota")
        .pop()
        .expect("quota exists");
    let document = persisted.quota.expect("provider quota");

    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(snapshot.billing().used_percent(), Some(37.5));
    assert_eq!(
        snapshot.billing().period_kind(),
        provider_xai::GrokQuotaPeriodKind::Weekly
    );
    assert_eq!(
        snapshot.billing().period_type(),
        Some("USAGE_PERIOD_TYPE_WEEKLY")
    );
    assert_eq!(snapshot.billing().prepaid_balance_cents(), Some(2500));
    assert!(
        document.expose_to_provider()["config"]
            .get("futureWindow")
            .is_some()
    );
}

#[tokio::test]
async fn recovered_quota_refresh_releases_existing_quota_and_cooldown_isolation() {
    for (suffix, availability, cooldown_until) in [
        ("recover-quota", AccountAvailability::QuotaExhausted, None),
        (
            "recover-cooldown",
            AccountAvailability::Cooldown,
            SystemTime::now().checked_add(Duration::from_secs(300)),
        ),
    ] {
        let (store, repository) = repository_with_accounts(&[(suffix, suffix)]).await;
        let id = account_id(suffix);
        set_account_state(&store, &id, availability, cooldown_until).await;
        crate::support::grok_quota_service(
            repository,
            QueueBillingTransport::success(br#"{"config":{"creditUsagePercent":25}}"#),
        )
        .refresh_account(&id)
        .await
        .expect("refresh recovered quota");

        let account = store.account(&id).expect("account");
        assert_eq!(account.availability(), AccountAvailability::Ready);
        assert_eq!(account.cooldown_until(), None);
    }
}

#[tokio::test]
async fn authoritative_exhaustion_preserves_quota_exhausted_state() {
    let (store, repository) =
        repository_with_accounts(&[("still-exhausted", "still-exhausted")]).await;
    let id = account_id("still-exhausted");
    set_account_state(&store, &id, AccountAvailability::QuotaExhausted, None).await;

    crate::support::grok_quota_service(
        repository,
        QueueBillingTransport::success(br#"{"config":{"creditUsagePercent":100}}"#),
    )
    .refresh_account(&id)
    .await
    .expect("refresh exhausted quota");

    assert_eq!(
        store.account(&id).expect("account").availability(),
        AccountAvailability::QuotaExhausted
    );
}

#[tokio::test]
async fn recovered_quota_does_not_clear_terminal_account_states() {
    for (suffix, availability) in [
        ("keep-banned", AccountAvailability::Banned),
        ("keep-expired", AccountAvailability::Expired),
        ("keep-invalid", AccountAvailability::Invalid),
    ] {
        let (store, repository) = repository_with_accounts(&[(suffix, suffix)]).await;
        let id = account_id(suffix);
        set_account_state(&store, &id, availability, None).await;
        crate::support::grok_quota_service(
            repository,
            QueueBillingTransport::success(br#"{"config":{"creditUsagePercent":10}}"#),
        )
        .refresh_account(&id)
        .await
        .expect("refresh terminal account quota");

        assert_eq!(
            store.account(&id).expect("account").availability(),
            availability
        );
    }
}

#[tokio::test]
async fn quota_refresh_does_not_overwrite_a_newer_cooldown() {
    let (store, repository) = repository_with_accounts(&[("new-cooldown", "new-cooldown")]).await;
    let id = account_id("new-cooldown");
    let new_cooldown = SystemTime::now()
        .checked_add(Duration::from_secs(600))
        .expect("cooldown time");
    let transport = Arc::new(MutatingBillingTransport {
        store: Arc::clone(&store),
        mutation: Mutex::new(Some(BillingMutation::State(AccountStateChange {
            account_id: id.clone(),
            expected_revision: CredentialRevision::new(1).expect("revision"),
            availability: AccountAvailability::Cooldown,
            reason: Some("new rate limit".to_owned()),
            cooldown_until: Some(new_cooldown),
            observed_at: SystemTime::now(),
        }))),
        body: br#"{"config":{"creditUsagePercent":10}}"#.to_vec(),
    });

    crate::support::grok_quota_service(repository, transport)
        .refresh_account(&id)
        .await
        .expect("refresh around concurrent cooldown");

    let account = store.account(&id).expect("account");
    assert_eq!(account.availability(), AccountAvailability::Cooldown);
    assert_eq!(account.cooldown_until(), Some(new_cooldown));
}

#[tokio::test]
async fn quota_refresh_rejects_a_concurrent_credential_revision() {
    let (store, repository) = repository_with_accounts(&[("new-revision", "new-revision")]).await;
    let id = account_id("new-revision");
    let account = store.account(&id).expect("account");
    let update = CredentialCasUpdate::new(
        id.clone(),
        account.revision(),
        ProviderAccountUpdate {
            account_id: id.clone(),
            name: account.name().to_owned(),
            email: account.email().map(str::to_owned),
            plan_type: account.plan_type().map(str::to_owned),
        },
        store.credential(&id).expect("credential"),
        account.has_refresh_token(),
        account.access_token_expires_at(),
        account.next_refresh_at(),
    )
    .expect("credential update");
    let transport = Arc::new(MutatingBillingTransport {
        store: Arc::clone(&store),
        mutation: Mutex::new(Some(BillingMutation::Credential(update))),
        body: br#"{"config":{"creditUsagePercent":10}}"#.to_vec(),
    });

    assert!(matches!(
        crate::support::grok_quota_service(repository, transport)
            .refresh_account(&id)
            .await,
        Err(GrokQuotaError::StaleCredentialSnapshot)
    ));
    assert_eq!(store.account(&id).expect("account").revision().get(), 2);
}

#[tokio::test]
async fn quota_projection_falls_back_to_legacy_monthly_usage() {
    let (_, repository) =
        repository_with_accounts(&[("monthly-quota", "subject-monthly-quota")]).await;
    let transport = QueueBillingTransport::success(
        br#"{"config":{"monthlyLimit":{"val":10000},"used":{"val":2500},"billingPeriodStart":"2026-07-01T00:00:00Z","billingPeriodEnd":"2026-08-01T00:00:00Z"}}"#,
    );
    let snapshot = crate::support::grok_quota_service(repository, transport)
        .refresh_account(&account_id("monthly-quota"))
        .await
        .expect("refresh monthly quota");

    assert_eq!(snapshot.billing().used_percent(), Some(25.0));
    assert_eq!(
        snapshot.billing().period_kind(),
        provider_xai::GrokQuotaPeriodKind::Monthly
    );
    assert_eq!(
        snapshot.billing().period_end(),
        Some("2026-08-01T00:00:00Z")
    );
}

#[tokio::test]
async fn quota_projection_preserves_unknown_period_for_dynamic_duration_fallback() {
    let (_, repository) =
        repository_with_accounts(&[("dynamic-quota", "subject-dynamic-quota")]).await;
    let transport = QueueBillingTransport::success(
        br#"{"config":{"creditUsagePercent":12.5,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_FORTNIGHT","start":"2026-07-01T00:00:00Z","end":"2026-07-15T00:00:00Z"}}}"#,
    );
    let snapshot = crate::support::grok_quota_service(repository, transport)
        .refresh_account(&account_id("dynamic-quota"))
        .await
        .expect("refresh dynamic quota");

    assert_eq!(snapshot.billing().used_percent(), Some(12.5));
    assert_eq!(
        snapshot.billing().period_kind(),
        provider_xai::GrokQuotaPeriodKind::Other
    );
    assert_eq!(
        snapshot.billing().period_start(),
        Some("2026-07-01T00:00:00Z")
    );
    assert_eq!(
        snapshot.billing().period_end(),
        Some("2026-07-15T00:00:00Z")
    );
}

#[tokio::test]
async fn weekly_period_without_reported_allowance_is_not_authoritative_quota() {
    let (_, repository) = repository_with_accounts(&[("free-quota", "subject-free-quota")]).await;
    let transport = QueueBillingTransport::success(
        br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-07-15T00:00:00Z","end":"2026-07-22T00:00:00Z"},"onDemandCap":{"val":0},"onDemandUsed":{"val":0},"prepaidBalance":{"val":0}}}"#,
    );
    let snapshot = crate::support::grok_quota_service(repository, transport)
        .refresh_account(&account_id("free-quota"))
        .await
        .expect("refresh Free quota");

    assert!(!snapshot.billing().has_authoritative_quota());
}

#[tokio::test]
async fn reported_zero_percent_is_authoritative_quota() {
    let (_, repository) =
        repository_with_accounts(&[("zero-percent", "subject-zero-percent")]).await;
    let transport = QueueBillingTransport::success(
        br#"{"config":{"creditUsagePercent":0,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-07-15T00:00:00Z","end":"2026-07-22T00:00:00Z"}}}"#,
    );
    let snapshot = crate::support::grok_quota_service(repository, transport)
        .refresh_account(&account_id("zero-percent"))
        .await
        .expect("refresh reported quota");

    assert!(snapshot.billing().has_authoritative_quota());
}

#[tokio::test]
async fn positive_prepaid_balance_is_authoritative_quota() {
    let (_, repository) =
        repository_with_accounts(&[("prepaid-quota", "subject-prepaid-quota")]).await;
    let transport = QueueBillingTransport::success(
        br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-07-15T00:00:00Z","end":"2026-07-22T00:00:00Z"},"prepaidBalance":{"val":500}}}"#,
    );
    let snapshot = crate::support::grok_quota_service(repository, transport)
        .refresh_account(&account_id("prepaid-quota"))
        .await
        .expect("refresh prepaid quota");

    assert!(snapshot.billing().has_authoritative_quota());
}

#[tokio::test]
async fn quota_read_rejects_corrupt_provider_document() {
    let (store, repository) = repository_with_accounts(&[("corrupt", "subject-corrupt")]).await;
    let mut document = serde_json::Map::new();
    document.insert("config".to_owned(), serde_json::json!([]));
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account_id("corrupt"),
            expected_revision: CredentialRevision::new(1).expect("revision"),
            quota: Some(OpaqueProviderData::new(document)),
            observed_at: Some(SystemTime::now()),
        })
        .await
        .expect("seed corrupt quota");
    let service = crate::support::grok_quota_service(
        repository,
        QueueBillingTransport::success(br#"{"config":null}"#),
    );

    assert!(matches!(
        service.read_account(&account_id("corrupt")).await,
        Err(GrokQuotaError::InvalidData)
    ));
}

#[tokio::test]
async fn disabled_account_quota_refresh_never_calls_upstream() {
    let (store, repository) =
        repository_with_accounts(&[("disabled-quota", "subject-disabled")]).await;
    store
        .set_enabled(&account_id("disabled-quota"), false)
        .await
        .expect("disable account");
    let transport = QueueBillingTransport::success(br#"{"config":null}"#);
    let service = crate::support::grok_quota_service(repository, transport.clone());

    assert!(matches!(
        service.refresh_account(&account_id("disabled-quota")).await,
        Err(GrokQuotaError::AccountUnavailable)
    ));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn failed_quota_refresh_does_not_replace_last_good_observation() {
    let (store, repository) = repository_with_accounts(&[("stable", "subject-stable")]).await;
    let good = QueueBillingTransport::success(br#"{"config":{"creditUsagePercent":10}}"#);
    crate::support::grok_quota_service(repository.clone(), good)
        .refresh_account(&account_id("stable"))
        .await
        .expect("seed good observation");
    let service = crate::support::grok_quota_service(repository, QueueBillingTransport::failure());

    assert!(matches!(
        service.refresh_account(&account_id("stable")).await,
        Err(GrokQuotaError::Upstream)
    ));
    let persisted = store
        .get_quotas(&[account_id("stable")])
        .await
        .expect("read quota")
        .pop()
        .expect("quota remains")
        .quota
        .expect("provider document");
    assert_eq!(
        persisted.expose_to_provider()["config"]["creditUsagePercent"].as_f64(),
        Some(10.0),
    );
}
