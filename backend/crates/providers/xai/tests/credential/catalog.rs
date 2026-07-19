use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, CredentialRevision, OpaqueProviderData,
    ProviderAccountStore, QuotaObservation,
};
use gateway_core::routing::{ConfigRevision, InstanceHealth, ProviderInstance, ProviderKind};
use provider_xai::{
    GROK_CLI_BASE_URL, GrokBillingRequest, GrokBillingTransport, GrokBillingTransportError,
    GrokBillingTransportErrorKind, GrokBillingTransportFuture, GrokBillingTransportResponse,
    GrokCredentialCatalogCache, GrokCredentialCatalogError, GrokCredentialCatalogSeed,
    GrokCredentialCatalogService, GrokCredentialQuotaService, GrokCredentialRepository,
    GrokModelCatalogRequest, GrokModelCatalogTransport, GrokModelCatalogTransportError,
    GrokModelCatalogTransportErrorKind, GrokModelCatalogTransportFuture,
    GrokModelCatalogTransportResponse, GrokQuotaError, SecretValue,
};

use crate::support::{
    MemoryGrokCatalogCache, MemoryProviderAccountStore, account_id, create_input, instance_id,
    seed_input,
};

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../transport/catalog/fixtures/official_grok_models_snapshot.json");

struct QueueCatalogTransport {
    responses:
        Mutex<VecDeque<Result<GrokModelCatalogTransportResponse, GrokModelCatalogTransportError>>>,
}

impl QueueCatalogTransport {
    fn from_bodies(bodies: impl IntoIterator<Item = Vec<u8>>) -> Arc<Self> {
        Arc::new(Self {
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
            responses: Mutex::new(VecDeque::from([Err(GrokModelCatalogTransportError::new(
                GrokModelCatalogTransportErrorKind::Unavailable,
            ))])),
        })
    }
}

impl GrokModelCatalogTransport for QueueCatalogTransport {
    fn execute(&self, _: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
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

fn instance() -> ProviderInstance {
    ProviderInstance::new(
        instance_id(),
        ProviderKind::new("xai").expect("provider"),
        GROK_CLI_BASE_URL.to_owned(),
        true,
        InstanceHealth::Healthy,
    )
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
async fn synchronization_caches_each_account_and_returns_strict_union() {
    let (_, repository) =
        repository_with_accounts(&[("catalog-a", "subject-a"), ("catalog-b", "subject-b")]).await;
    let cache = MemoryGrokCatalogCache::shared();
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = cache.clone();
    let transport =
        QueueCatalogTransport::from_bodies([OFFICIAL_FIXTURE.to_vec(), OFFICIAL_FIXTURE.to_vec()]);
    let service = GrokCredentialCatalogService::new(repository, transport, cache_port);
    let snapshot = service
        .synchronize_instance(
            &instance(),
            ConfigRevision::new(7).expect("config revision"),
        )
        .await
        .expect("catalog sync");

    assert_eq!(snapshot.accounts().len(), 2);
    assert_eq!(snapshot.models().len(), 1);
    assert_eq!(snapshot.models()[0].request_model().as_str(), "grok-4.5");
    for id in [account_id("catalog-a"), account_id("catalog-b")] {
        assert!(
            cache
                .permits(
                    &id,
                    gateway_core::engine::credential::CredentialRevision::new(1).expect("revision"),
                    "grok-4.5",
                )
                .await
                .expect("cache lookup")
        );
    }
}

#[tokio::test]
async fn single_account_catalog_refresh_and_read_use_provider_cache_boundary() {
    let (_, repository) = repository_with_accounts(&[("account-models", "subject-models")]).await;
    let cache = MemoryGrokCatalogCache::shared();
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = cache;
    let service = GrokCredentialCatalogService::new(
        repository,
        QueueCatalogTransport::from_bodies([OFFICIAL_FIXTURE.to_vec()]),
        cache_port,
    );
    let refreshed = service
        .refresh_account_catalog(&account_id("account-models"), "0.2.101")
        .await
        .expect("refresh one account catalog");
    assert_eq!(refreshed.seed().models(), ["grok-4.5"]);

    let cached = service
        .read_account_catalog(
            &account_id("account-models"),
            CredentialRevision::new(1).expect("revision"),
        )
        .await
        .expect("read cache")
        .expect("cached catalog");
    assert_eq!(cached.seed().models(), ["grok-4.5"]);
}

#[tokio::test]
async fn single_account_catalog_read_miss_does_not_call_upstream() {
    let (_, repository) =
        repository_with_accounts(&[("account-models-miss", "subject-models")]).await;
    let service = GrokCredentialCatalogService::new(
        repository,
        QueueCatalogTransport::failure(),
        MemoryGrokCatalogCache::shared(),
    );
    assert!(
        service
            .read_account_catalog(
                &account_id("account-models-miss"),
                CredentialRevision::new(1).expect("revision"),
            )
            .await
            .expect("read cache")
            .is_none()
    );
}

#[tokio::test]
async fn disabled_accounts_are_not_sent_to_catalog_transport() {
    let (store, repository) = repository_with_accounts(&[("disabled", "subject-disabled")]).await;
    store
        .set_enabled(&account_id("disabled"), false)
        .await
        .expect("disable");
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = MemoryGrokCatalogCache::shared();
    let service = GrokCredentialCatalogService::new(
        repository,
        QueueCatalogTransport::from_bodies([]),
        cache_port,
    );
    assert!(matches!(
        service
            .synchronize_instance(&instance(), ConfigRevision::new(1).expect("revision"))
            .await,
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
    let service = GrokCredentialCatalogService::new(
        repository,
        QueueCatalogTransport::from_bodies([OFFICIAL_FIXTURE.to_vec()]),
        MemoryGrokCatalogCache::shared(),
    );

    let snapshot = service
        .synchronize_instance(&instance(), ConfigRevision::new(1).expect("revision"))
        .await
        .expect("discover catalog through quota exhausted account");

    assert_eq!(snapshot.models().len(), 1);
}

#[tokio::test]
async fn one_upstream_failure_rejects_the_whole_catalog_cycle() {
    let (_, repository) = repository_with_accounts(&[("failed", "subject-failed")]).await;
    let cache_port: Arc<dyn GrokCredentialCatalogCache> = MemoryGrokCatalogCache::shared();
    let service =
        GrokCredentialCatalogService::new(repository, QueueCatalogTransport::failure(), cache_port);
    assert!(matches!(
        service
            .synchronize_instance(&instance(), ConfigRevision::new(1).expect("revision"))
            .await,
        Err(GrokCredentialCatalogError::Upstream)
    ));
}

#[tokio::test]
async fn conflicting_facts_for_same_slug_fail_closed() {
    let (_, repository) =
        repository_with_accounts(&[("conflict-a", "subject-a"), ("conflict-b", "subject-b")]).await;
    let mut conflicting: serde_json::Value =
        serde_json::from_slice(OFFICIAL_FIXTURE).expect("fixture JSON");
    conflicting["data"][0]["name"] = serde_json::json!("Different name");
    let service = GrokCredentialCatalogService::new(
        repository,
        QueueCatalogTransport::from_bodies([
            OFFICIAL_FIXTURE.to_vec(),
            serde_json::to_vec(&conflicting).expect("conflicting JSON"),
        ]),
        MemoryGrokCatalogCache::shared(),
    );
    assert!(matches!(
        service
            .synchronize_instance(&instance(), ConfigRevision::new(1).expect("revision"))
            .await,
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
    let service = GrokCredentialCatalogService::new(
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
                "0.2.101",
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
    let service = GrokCredentialQuotaService::new(repository, transport.clone());

    let snapshot = service
        .refresh_account(&account_id("quota"))
        .await
        .expect("refresh quota");
    let persisted = store
        .get_quota(&account_id("quota"))
        .await
        .expect("read persisted quota")
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
async fn quota_projection_falls_back_to_legacy_monthly_usage() {
    let (_, repository) =
        repository_with_accounts(&[("monthly-quota", "subject-monthly-quota")]).await;
    let transport = QueueBillingTransport::success(
        br#"{"config":{"monthlyLimit":{"val":10000},"used":{"val":2500},"billingPeriodStart":"2026-07-01T00:00:00Z","billingPeriodEnd":"2026-08-01T00:00:00Z"}}"#,
    );
    let snapshot = GrokCredentialQuotaService::new(repository, transport)
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
    let snapshot = GrokCredentialQuotaService::new(repository, transport)
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
    let snapshot = GrokCredentialQuotaService::new(repository, transport)
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
    let snapshot = GrokCredentialQuotaService::new(repository, transport)
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
    let snapshot = GrokCredentialQuotaService::new(repository, transport)
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
    let service = GrokCredentialQuotaService::new(
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
    let service = GrokCredentialQuotaService::new(repository, transport.clone());

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
    GrokCredentialQuotaService::new(repository.clone(), good)
        .refresh_account(&account_id("stable"))
        .await
        .expect("seed good observation");
    let service = GrokCredentialQuotaService::new(repository, QueueBillingTransport::failure());

    assert!(matches!(
        service.refresh_account(&account_id("stable")).await,
        Err(GrokQuotaError::Upstream)
    ));
    let persisted = store
        .get_quota(&account_id("stable"))
        .await
        .expect("read quota")
        .expect("quota remains")
        .quota
        .expect("provider document");
    assert_eq!(
        persisted.expose_to_provider()["config"]["creditUsagePercent"].as_f64(),
        Some(10.0),
    );
}
