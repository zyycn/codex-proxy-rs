use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::Utc;
use gateway_core::engine::credential::{
    AccountAvailability, CredentialCasOutcome, CredentialCasUpdate, CredentialRevision,
    ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate,
};
use gateway_core::provider_ports::{
    ProviderCooldown, ProviderCooldownPort, ProviderCredentialState, ProviderCredentialStatePort,
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest, ProviderStoreError,
};
use provider_xai::{
    GrokCredentialCatalogCache, GrokCredentialRecovery, GrokCredentialRecoveryOutcome,
    GrokCredentialRefreshError, GrokCredentialRefreshOutcome, GrokCredentialRefreshService,
    GrokCredentialRefresher, GrokCredentialRepository, GrokModelCatalogRequest,
    GrokModelCatalogTransport, GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse,
    GrokRefreshFailure, GrokRefreshTokens, SecretValue,
};

use crate::support::{
    MemoryCooldownPort, MemoryGrokCatalogCache, MemoryProviderAccountStore, create_input,
    credential_object, runtime_policy, seed_input,
};

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../transport/catalog/fixtures/official_grok_models_snapshot.json");

struct StaticCatalogTransport;

impl GrokModelCatalogTransport for StaticCatalogTransport {
    fn execute(&self, _: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        Box::pin(async {
            Ok(GrokModelCatalogTransportResponse::new(
                OFFICIAL_FIXTURE,
                None,
            ))
        })
    }
}

struct QueueRefresher {
    prepare_calls: AtomicUsize,
    responses: Mutex<VecDeque<Result<GrokRefreshTokens, GrokRefreshFailure>>>,
}

impl QueueRefresher {
    fn new(
        responses: impl IntoIterator<Item = Result<GrokRefreshTokens, GrokRefreshFailure>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            prepare_calls: AtomicUsize::new(0),
            responses: Mutex::new(responses.into_iter().collect()),
        })
    }
}

#[async_trait]
impl GrokCredentialRefresher for QueueRefresher {
    async fn prepare_cycle(&self) -> Result<(), GrokRefreshFailure> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn refresh(&self, _: &SecretValue) -> Result<GrokRefreshTokens, GrokRefreshFailure> {
        self.responses
            .lock()
            .expect("refresh queue")
            .pop_front()
            .expect("one refresh response")
    }
}

struct TestRefreshLeases {
    available: bool,
    calls: AtomicUsize,
}

impl ProviderLeasePort for TestRefreshLeases {
    fn load_state<'a>(
        &'a self,
        _: &'a gateway_core::routing::ProviderKind,
        _: &'a [gateway_core::engine::credential::ProviderAccountId],
    ) -> futures::future::BoxFuture<
        'a,
        Result<gateway_core::provider_ports::ProviderSchedulingState, ProviderStoreError>,
    > {
        Box::pin(async {
            Ok(gateway_core::provider_ports::ProviderSchedulingState::new(
                Default::default(),
                0,
            ))
        })
    }

    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> futures::future::BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async move {
            assert!(matches!(
                request,
                ProviderLeaseRequest::RefreshCapacity(_) | ProviderLeaseRequest::Refresh(_)
            ));
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(if self.available {
                ProviderLeaseAcquisition::Acquired(Box::new(()))
            } else {
                ProviderLeaseAcquisition::Busy { retry_after: None }
            })
        })
    }
}

fn success_tokens(rotated: Option<&str>) -> GrokRefreshTokens {
    GrokRefreshTokens {
        access_token: SecretValue::new("new-access"),
        rotated_refresh_token: rotated.map(SecretValue::new),
        expires_in: Duration::from_secs(3600),
    }
}

fn due_input(suffix: &str) -> provider_xai::CreateGrokCredential {
    let mut input = create_input(suffix, &format!("subject-{suffix}"));
    input.account.access_token_expires_at = Utc::now() + chrono::Duration::minutes(2);
    input.next_refresh_at = Utc::now() - chrono::Duration::seconds(1);
    input
}

async fn fixture(
    input: provider_xai::CreateGrokCredential,
    responses: impl IntoIterator<Item = Result<GrokRefreshTokens, GrokRefreshFailure>>,
    lease_available: bool,
) -> (
    Arc<MemoryProviderAccountStore>,
    GrokCredentialRepository,
    Arc<QueueRefresher>,
    GrokCredentialRefreshService,
) {
    fixture_many([input], responses, lease_available).await
}

/// 带外部注入 cooldown 端口的 fixture（AUD-05 组合测试用）。
#[allow(clippy::type_complexity)]
async fn fixture_with_cooldowns(
    input: provider_xai::CreateGrokCredential,
    responses: impl IntoIterator<Item = Result<GrokRefreshTokens, GrokRefreshFailure>>,
    lease_available: bool,
    cooldowns: Arc<MemoryCooldownPort>,
) -> (
    Arc<MemoryProviderAccountStore>,
    GrokCredentialRepository,
    Arc<QueueRefresher>,
    GrokCredentialRefreshService,
    Arc<MemoryCooldownPort>,
) {
    let store = MemoryProviderAccountStore::shared();
    let account_store: Arc<dyn ProviderAccountStore> = store.clone();
    let repository = GrokCredentialRepository::new(account_store);
    seed_input(&store, &input).await.expect("create account");
    let refresher = QueueRefresher::new(responses);
    let refresher_port: Arc<dyn GrokCredentialRefresher> = refresher.clone();
    let cache: Arc<dyn GrokCredentialCatalogCache> = MemoryGrokCatalogCache::shared();
    let catalog = Arc::new(crate::support::grok_catalog_service(
        repository.clone(),
        Arc::new(StaticCatalogTransport),
        cache,
    ));
    let leases = Arc::new(TestRefreshLeases {
        available: lease_available,
        calls: AtomicUsize::new(0),
    });
    let service = GrokCredentialRefreshService::new(
        repository.clone(),
        refresher_port,
        catalog,
        leases,
        cooldowns.clone(),
        Arc::new(CountingCredentialState::default()),
        runtime_policy(),
    );
    (store, repository, refresher, service, cooldowns)
}

/// 真实累加连续失败计数的测试 double，用于断言退避的指数增长与成功清零。
#[derive(Default)]
struct CountingCredentialState {
    counts: Mutex<BTreeMap<String, u32>>,
    cleared: Mutex<Vec<String>>,
}

impl CountingCredentialState {
    fn count(&self, account_id: &ProviderAccountId) -> u32 {
        self.counts
            .lock()
            .expect("backoff counts lock")
            .get(account_id.as_str())
            .copied()
            .unwrap_or(0)
    }

    fn was_cleared(&self, account_id: &ProviderAccountId) -> bool {
        self.cleared
            .lock()
            .expect("cleared accounts lock")
            .iter()
            .any(|id| id == account_id.as_str())
    }
}

impl ProviderCredentialStatePort for CountingCredentialState {
    fn replace(
        &self,
        _state: ProviderCredentialState,
    ) -> futures::future::BoxFuture<'_, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn read<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderCredentialState>, ProviderStoreError>>
    {
        Box::pin(async { Ok(None) })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn record_refresh_backoff<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        _window: Duration,
    ) -> futures::future::BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async move {
            let mut counts = self.counts.lock().expect("backoff counts lock");
            let entry = counts.entry(account_id.as_str().to_owned()).or_insert(0);
            *entry += 1;
            Ok(*entry)
        })
    }

    fn clear_refresh_backoff<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            self.counts
                .lock()
                .expect("backoff counts lock")
                .remove(account_id.as_str());
            self.cleared
                .lock()
                .expect("cleared accounts lock")
                .push(account_id.as_str().to_owned());
            Ok(())
        })
    }
}

async fn fixture_many(
    inputs: impl IntoIterator<Item = provider_xai::CreateGrokCredential>,
    responses: impl IntoIterator<Item = Result<GrokRefreshTokens, GrokRefreshFailure>>,
    lease_available: bool,
) -> (
    Arc<MemoryProviderAccountStore>,
    GrokCredentialRepository,
    Arc<QueueRefresher>,
    GrokCredentialRefreshService,
) {
    fixture_many_with_state(
        inputs,
        responses,
        lease_available,
        Arc::new(CountingCredentialState::default()),
    )
    .await
}

async fn fixture_many_with_state(
    inputs: impl IntoIterator<Item = provider_xai::CreateGrokCredential>,
    responses: impl IntoIterator<Item = Result<GrokRefreshTokens, GrokRefreshFailure>>,
    lease_available: bool,
    credential_state: Arc<dyn ProviderCredentialStatePort>,
) -> (
    Arc<MemoryProviderAccountStore>,
    GrokCredentialRepository,
    Arc<QueueRefresher>,
    GrokCredentialRefreshService,
) {
    let store = MemoryProviderAccountStore::shared();
    let account_store: Arc<dyn ProviderAccountStore> = store.clone();
    let repository = GrokCredentialRepository::new(account_store);
    for input in inputs {
        seed_input(&store, &input).await.expect("create account");
    }
    let refresher = QueueRefresher::new(responses);
    let refresher_port: Arc<dyn GrokCredentialRefresher> = refresher.clone();
    let cache: Arc<dyn GrokCredentialCatalogCache> = MemoryGrokCatalogCache::shared();
    let catalog = Arc::new(crate::support::grok_catalog_service(
        repository.clone(),
        Arc::new(StaticCatalogTransport),
        cache,
    ));
    let leases = Arc::new(TestRefreshLeases {
        available: lease_available,
        calls: AtomicUsize::new(0),
    });
    let cooldowns = Arc::new(MemoryCooldownPort::default());
    let service = GrokCredentialRefreshService::new(
        repository.clone(),
        refresher_port,
        catalog,
        leases,
        cooldowns,
        credential_state,
        runtime_policy(),
    );
    (store, repository, refresher, service)
}

/// 用 store 的 CAS 把 next_refresh_at 复位到过去，使已退避到未来的账号再次到期。
async fn force_due(store: &MemoryProviderAccountStore, id: &ProviderAccountId) {
    let account = store.account(id).expect("account to reset");
    let credential = store.credential(id).expect("credential to reset");
    let update = CredentialCasUpdate::new(
        id.clone(),
        account.revision(),
        ProviderAccountUpdate {
            account_id: id.clone(),
            name: account.name().to_owned(),
            email: account.email().map(str::to_owned),
            plan_type: account.plan_type().map(str::to_owned),
        },
        credential,
        true,
        account.access_token_expires_at(),
        Some(SystemTime::now() - Duration::from_secs(1)),
    )
    .expect("valid due-time reset");
    assert!(matches!(
        store
            .compare_and_swap_credential(update)
            .await
            .expect("reset next_refresh_at to the past"),
        CredentialCasOutcome::Updated(_)
    ));
}

#[tokio::test]
async fn successful_refresh_rotates_plaintext_tokens_once() {
    let input = due_input("success");
    let id = input.account_id.clone();
    let (store, _, refresher, service) =
        fixture(input, [Ok(success_tokens(Some("new-refresh")))], true).await;
    let outcomes = service.refresh_due().await.expect("refresh cycle");

    assert!(matches!(
        outcomes.as_slice(),
        [GrokCredentialRefreshOutcome::Refreshed {
            account_id,
            credential_revision
        }] if account_id == &id && credential_revision.get() == 2
    ));
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 1);
    let credential = store.credential(&id).expect("credential");
    assert_eq!(
        credential_object(&credential)
            .get("refresh_token")
            .and_then(|value| value.as_str()),
        Some("new-refresh")
    );
    assert!(
        !credential_object(&credential).contains_key("refresh_token_expires_at"),
        "rotated RT has no authoritative expiry in the refresh response"
    );
}

#[tokio::test]
async fn unauthorized_recovery_forces_refresh_before_the_due_time() {
    let input = create_input("unauthorized-recovery", "subject-unauthorized-recovery");
    let id = input.account_id.clone();
    let (store, _, refresher, service) =
        fixture(input, [Ok(success_tokens(Some("rotated-refresh")))], true).await;

    let outcome = service
        .recover_unauthorized(&id, CredentialRevision::new(1).expect("revision"))
        .await;

    assert_eq!(outcome, GrokCredentialRecoveryOutcome::Recovered);
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.account(&id).expect("account").revision().get(), 2);
}

#[tokio::test]
async fn unauthorized_recovery_marks_a_permanently_rejected_refresh_expired() {
    let input = create_input("unauthorized-expired", "subject-unauthorized-expired");
    let id = input.account_id.clone();
    let (store, _, _, service) =
        fixture(input, [Err(GrokRefreshFailure::InvalidGrant)], true).await;

    let outcome = service
        .recover_unauthorized(&id, CredentialRevision::new(1).expect("revision"))
        .await;

    assert_eq!(outcome, GrokCredentialRecoveryOutcome::Rejected);
    assert_eq!(
        store.account(&id).expect("account").availability(),
        AccountAvailability::Expired
    );
}

#[tokio::test]
async fn manual_refresh_returns_prepared_rotation_without_writing_store() {
    let input = due_input("manual");
    let id = input.account_id.clone();
    let (store, _, refresher, service) =
        fixture(input, [Ok(success_tokens(Some("manual-refresh")))], true).await;
    let current = store
        .load_current_credential(&id)
        .await
        .expect("load current credential");

    let prepared = service
        .prepare_manual_refresh(current)
        .await
        .expect("prepare manual refresh");
    assert_eq!(store.account(&id).expect("account").revision().get(), 1);
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 1);
    let (_profile, credential, guard) = prepared.into_parts();
    assert!(format!("{guard:?}").contains("<held>"));
    assert!(matches!(
        store
            .compare_and_swap_credential(credential)
            .await
            .expect("persist prepared rotation"),
        CredentialCasOutcome::Updated(revision) if revision.get() == 2
    ));
    drop(guard);
}

#[tokio::test]
async fn manual_refresh_preserves_snapshot_revision_as_the_final_cas_fence() {
    let input = due_input("manual-stale");
    let id = input.account_id.clone();
    let (store, _, refresher, service) = fixture(
        input,
        [Ok(success_tokens(Some("manual-stale-refresh")))],
        true,
    )
    .await;
    let current = store
        .load_current_credential(&id)
        .await
        .expect("load current credential");
    force_due(&store, &id).await;

    let prepared = service
        .prepare_manual_refresh(current)
        .await
        .expect("prepare from current snapshot");
    let (_, credential, _guard) = prepared.into_parts();

    assert!(matches!(
        store
            .compare_and_swap_credential(credential)
            .await
            .expect("attempt stale prepared rotation"),
        CredentialCasOutcome::Conflict
    ));
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn manual_refresh_preserves_failure_class_without_store_write() {
    let input = due_input("manual-invalid");
    let id = input.account_id.clone();
    let (store, _, _, service) =
        fixture(input, [Err(GrokRefreshFailure::InvalidGrant)], true).await;
    let current = store
        .load_current_credential(&id)
        .await
        .expect("load current credential");

    assert!(matches!(
        service.prepare_manual_refresh(current).await,
        Err(GrokCredentialRefreshError::ManualFailure(
            GrokRefreshFailure::InvalidGrant
        ))
    ));
    assert_eq!(store.account(&id).expect("account").revision().get(), 1);
}

#[tokio::test]
async fn omitted_rotated_refresh_token_preserves_existing_rt() {
    let input = due_input("preserve");
    let id = input.account_id.clone();
    let (store, _, _, service) = fixture(input, [Ok(success_tokens(None))], true).await;
    service.refresh_due().await.expect("refresh");
    let credential = store.credential(&id).expect("credential");
    assert_eq!(
        credential_object(&credential)
            .get("refresh_token")
            .and_then(|value| value.as_str()),
        Some("refresh-preserve")
    );
    assert!(credential_object(&credential).contains_key("refresh_token_expires_at"));
}

#[tokio::test]
async fn unknown_refresh_token_expiry_does_not_block_refresh() {
    let mut input = due_input("unknown-expiry");
    let id = input.account_id.clone();
    input.account.refresh_token_expires_at = None;
    let (store, _, _, service) = fixture(input, [Ok(success_tokens(None))], true).await;

    let outcomes = service.refresh_due().await.expect("refresh cycle");
    assert!(matches!(
        outcomes.as_slice(),
        [GrokCredentialRefreshOutcome::Refreshed { account_id, .. }] if account_id == &id
    ));
    assert_eq!(store.account(&id).expect("account").revision().get(), 2);
}

#[tokio::test]
async fn empty_due_set_does_not_resolve_discovery_or_call_upstream() {
    let input = create_input("not-due", "subject-not-due");
    let (_, _, refresher, service) = fixture(input, [], true).await;
    assert!(service.refresh_due().await.expect("empty cycle").is_empty());
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn lease_unavailable_never_calls_refresh_exchange() {
    let input = due_input("lease");
    let id = input.account_id.clone();
    let (_, _, refresher, service) = fixture(input, [], false).await;
    let outcomes = service.refresh_due().await.expect("refresh cycle");
    assert!(matches!(
        outcomes.as_slice(),
        [GrokCredentialRefreshOutcome::LeaseUnavailable { account_id }] if account_id == &id
    ));
    assert!(refresher.responses.lock().expect("queue").is_empty());
}

#[tokio::test]
async fn invalid_grant_marks_account_expired() {
    let input = due_input("invalid-grant");
    let id = input.account_id.clone();
    let (store, _, _, service) =
        fixture(input, [Err(GrokRefreshFailure::InvalidGrant)], true).await;
    service.refresh_due().await.expect("refresh");
    assert_eq!(
        store.account(&id).expect("account").availability(),
        AccountAvailability::Expired
    );
}

#[tokio::test]
async fn banned_failure_marks_account_banned() {
    let input = due_input("banned");
    let id = input.account_id.clone();
    let (store, _, _, service) = fixture(input, [Err(GrokRefreshFailure::Banned)], true).await;
    service.refresh_due().await.expect("refresh");
    assert_eq!(
        store.account(&id).expect("account").availability(),
        AccountAvailability::Banned
    );
}

#[tokio::test]
async fn ambiguous_refresh_uses_runtime_cooldown_without_persisting_account_state() {
    let input = due_input("ambiguous");
    let id = input.account_id.clone();
    let (store, _, refresher, service) =
        fixture(input, [Err(GrokRefreshFailure::Ambiguous)], true).await;
    let outcomes = service.refresh_due().await.expect("refresh");
    assert!(matches!(
        outcomes.as_slice(),
        [GrokCredentialRefreshOutcome::Ambiguous { account_id }] if account_id == &id
    ));
    assert_eq!(
        store.account(&id).expect("account").availability(),
        AccountAvailability::Unknown
    );
    assert_eq!(store.account(&id).expect("account").revision().get(), 1);
    assert!(
        service
            .refresh_due()
            .await
            .expect("second cycle")
            .is_empty()
    );
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pre_send_transient_failure_applies_bounded_cooldown() {
    let input = due_input("transient");
    let id = input.account_id.clone();
    let (store, _, _, service) = fixture(input, [Err(GrokRefreshFailure::Transient)], true).await;
    service.refresh_due().await.expect("refresh");
    let account = store.account(&id).expect("account");
    assert_eq!(account.availability(), AccountAvailability::Unknown);
    assert_eq!(account.revision().get(), 2);
    assert!(
        account
            .next_refresh_at()
            .is_some_and(|retry| retry > std::time::SystemTime::now())
    );
}

#[tokio::test]
async fn refresh_backoff_grows_exponentially_across_attempts() {
    let input = due_input("backoff-growth");
    let id = input.account_id.clone();
    let counting = Arc::new(CountingCredentialState::default());
    let (store, _, _, service) = fixture_many_with_state(
        [input],
        [
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
        ],
        true,
        counting.clone(),
    )
    .await;

    let before_first = SystemTime::now();
    let first = service.refresh_due().await.expect("first refresh cycle");
    assert!(matches!(
        first.as_slice(),
        [GrokCredentialRefreshOutcome::Transient { .. }]
    ));
    let first_delay = store
        .account(&id)
        .expect("account after first defer")
        .next_refresh_at()
        .expect("first retry scheduled")
        .duration_since(before_first)
        .expect("first delay is in the future");
    assert_eq!(counting.count(&id), 1);

    force_due(&store, &id).await;

    let before_second = SystemTime::now();
    let second = service.refresh_due().await.expect("second refresh cycle");
    assert!(matches!(
        second.as_slice(),
        [GrokCredentialRefreshOutcome::Transient { .. }]
    ));
    let second_delay = store
        .account(&id)
        .expect("account after second defer")
        .next_refresh_at()
        .expect("second retry scheduled")
        .duration_since(before_second)
        .expect("second delay is in the future");
    assert_eq!(counting.count(&id), 2);

    // base=5s、factor=3：第二次（attempt=2）应比第一次（attempt=1）显著更久。
    assert!(
        second_delay > first_delay * 2,
        "second backoff {second_delay:?} should grow well beyond first {first_delay:?}"
    );
}

#[tokio::test]
async fn successful_refresh_clears_backoff_counter() {
    let input = due_input("backoff-clear");
    let id = input.account_id.clone();
    let counting = Arc::new(CountingCredentialState::default());
    let (store, _, _, service) = fixture_many_with_state(
        [input],
        [
            Err(GrokRefreshFailure::Transient),
            Ok(success_tokens(Some("cleared-refresh"))),
        ],
        true,
        counting.clone(),
    )
    .await;

    let first = service.refresh_due().await.expect("first refresh cycle");
    assert!(matches!(
        first.as_slice(),
        [GrokCredentialRefreshOutcome::Transient { .. }]
    ));
    assert_eq!(counting.count(&id), 1);

    force_due(&store, &id).await;

    let second = service.refresh_due().await.expect("second refresh cycle");
    assert!(matches!(
        second.as_slice(),
        [GrokCredentialRefreshOutcome::Refreshed { .. }]
    ));
    assert!(counting.was_cleared(&id));
    assert_eq!(counting.count(&id), 0);
}

#[tokio::test]
async fn invalid_refresh_lifetime_is_rejected_without_cas_write() {
    let input = due_input("bad-lifetime");
    let id = input.account_id.clone();
    let (store, _, _, service) = fixture(
        input,
        [Ok(GrokRefreshTokens {
            access_token: SecretValue::new("new-access"),
            rotated_refresh_token: None,
            expires_in: Duration::ZERO,
        })],
        true,
    )
    .await;
    assert!(matches!(
        service.refresh_due().await.expect("isolated refresh cycle").as_slice(),
        [GrokCredentialRefreshOutcome::Failed { account_id }] if account_id == &id
    ));
    assert_eq!(store.account(&id).expect("account").revision().get(), 1);
}

#[tokio::test]
async fn malformed_account_refresh_does_not_stop_later_accounts() {
    let due_at = Utc::now() - chrono::Duration::seconds(1);
    let access_expires_at = Utc::now() + chrono::Duration::minutes(2);
    let mut bad = due_input("bad");
    bad.next_refresh_at = due_at;
    bad.account.access_token_expires_at = access_expires_at;
    let bad_id = bad.account_id.clone();
    let mut good = due_input("good");
    good.next_refresh_at = due_at;
    good.account.access_token_expires_at = access_expires_at;
    let good_id = good.account_id.clone();
    let (store, _, _, service) = fixture_many(
        [bad, good],
        [
            Ok(GrokRefreshTokens {
                access_token: SecretValue::new("invalid"),
                rotated_refresh_token: None,
                expires_in: Duration::ZERO,
            }),
            Ok(success_tokens(Some("good-refresh"))),
        ],
        true,
    )
    .await;

    let outcomes = service.refresh_due().await.expect("isolated refresh cycle");

    assert!(matches!(
        outcomes.as_slice(),
        [
            GrokCredentialRefreshOutcome::Failed { account_id: failed },
            GrokCredentialRefreshOutcome::Refreshed {
                account_id: refreshed,
                credential_revision,
            },
        ] if failed == &bad_id && refreshed == &good_id && credential_revision.get() == 2
    ));
    assert_eq!(
        store
            .account(&good_id)
            .expect("good account")
            .revision()
            .get(),
        2
    );
}

#[tokio::test]
async fn runtime_cooldown_survives_credential_rotation_and_blocks_refresh() {
    // cooldown 写于 revision N，轮换到 N+1 后 refresh worker 不得清除
    // 或失效它；cooldown 活跃期间账号被跳过刷新。
    let input = create_input(
        "cooldown-survives-rotation",
        "subject-cooldown-survives-rotation",
    );
    let id = input.account_id.clone();
    let cooldowns = Arc::new(MemoryCooldownPort::default());
    let (store, _, _, service, cooldowns) = fixture_with_cooldowns(
        input,
        [Ok(success_tokens(Some("rotated-refresh")))],
        true,
        cooldowns,
    )
    .await;
    // 先写 account cooldown（revision 1，未来到期）。
    let until = SystemTime::now() + Duration::from_secs(3600);
    cooldowns
        .put_if_later(ProviderCooldown::new(
            id.clone(),
            CredentialRevision::new(1).expect("revision"),
            until,
        ))
        .await
        .expect("write cooldown");

    // 轮换到 revision 2。
    let outcome = service
        .recover_unauthorized(&id, CredentialRevision::new(1).expect("revision"))
        .await;
    assert_eq!(outcome, GrokCredentialRecoveryOutcome::Recovered);
    assert_eq!(store.account(&id).expect("account").revision().get(), 2);

    // cooldown 仍有效（轮换不清除）。
    assert!(
        cooldowns
            .cooldown(&id)
            .is_some_and(|cooldown| cooldown.until() > SystemTime::now()),
        "rotation must not clear account runtime cooldown"
    );
    // cooldown 活跃期间 refresh worker 跳过该账号（不清除、不刷新）。
    let outcomes = service.refresh_due().await.expect("refresh due");
    assert!(
        outcomes.is_empty(),
        "cooldown-active account must be skipped by refresh worker"
    );
    assert_eq!(store.account(&id).expect("account").revision().get(), 2);
}
