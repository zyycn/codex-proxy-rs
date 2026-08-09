use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU32;
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
    ProviderCredentialState, ProviderCredentialStatePort, ProviderLeaseAcquisition,
    ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshPolicy, ProviderRuntimePolicyPort,
    ProviderStoreError,
};
use provider_xai::{
    GrokCredentialCatalogCache, GrokCredentialRecovery, GrokCredentialRecoveryOutcome,
    GrokCredentialRefreshError, GrokCredentialRefreshOutcome, GrokCredentialRefreshService,
    GrokCredentialRefresher, GrokCredentialRepository, GrokModelCatalogRequest,
    GrokModelCatalogTransport, GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse,
    GrokRefreshFailure, GrokRefreshTokens, SecretValue,
};

use crate::support::{
    MemoryGrokCatalogCache, MemoryProviderAccountStore, create_input, credential_object,
    runtime_policy, seed_input,
};

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../transport/catalog/fixtures/official_grok_models_snapshot.json");
const OAUTH_BACKOFF_ATTEMPTS: u32 = 5;

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

struct MutableRuntimePolicy {
    policy: Mutex<ProviderRefreshPolicy>,
}

impl MutableRuntimePolicy {
    fn new(policy: ProviderRefreshPolicy) -> Self {
        Self {
            policy: Mutex::new(policy),
        }
    }

    fn set(&self, policy: ProviderRefreshPolicy) {
        *self.policy.lock().expect("runtime policy lock") = policy;
    }
}

impl ProviderRuntimePolicyPort for MutableRuntimePolicy {
    fn load_refresh_policy(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        let policy = *self.policy.lock().expect("runtime policy lock");
        Box::pin(async move { Ok(policy) })
    }
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

fn refresh_policy_with_margin(margin: Duration) -> ProviderRefreshPolicy {
    ProviderRefreshPolicy::try_new(margin, NonZeroU32::new(2).expect("positive concurrency"))
        .expect("valid refresh policy")
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
    fixture_many_with_state_and_runtime_policy(
        inputs,
        responses,
        lease_available,
        credential_state,
        runtime_policy(),
    )
    .await
}

async fn fixture_many_with_state_and_runtime_policy(
    inputs: impl IntoIterator<Item = provider_xai::CreateGrokCredential>,
    responses: impl IntoIterator<Item = Result<GrokRefreshTokens, GrokRefreshFailure>>,
    lease_available: bool,
    credential_state: Arc<dyn ProviderCredentialStatePort>,
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
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
    let service = GrokCredentialRefreshService::new(
        repository.clone(),
        refresher_port,
        catalog,
        leases,
        credential_state,
        runtime_policy,
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

async fn set_token_deadlines(
    store: &MemoryProviderAccountStore,
    id: &ProviderAccountId,
    access_token_expires_at: SystemTime,
    next_refresh_at: SystemTime,
) {
    let account = store.account(id).expect("account to update");
    let credential = store.credential(id).expect("credential to update");
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
        Some(access_token_expires_at),
        Some(next_refresh_at),
    )
    .expect("valid token deadline update");
    assert!(matches!(
        store
            .compare_and_swap_credential(update)
            .await
            .expect("update token deadlines"),
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
    assert!(
        store
            .account(&id)
            .expect("account after refresh")
            .next_refresh_at()
            .is_none(),
        "successful refresh must not persist a policy-derived schedule"
    );
}

#[tokio::test]
async fn refresh_due_uses_the_current_margin_without_persisting_a_normal_schedule() {
    let mut input = create_input("runtime-margin", "subject-runtime-margin");
    input.account.access_token_expires_at = Utc::now() + chrono::Duration::minutes(2);
    let access_token_expires_at: SystemTime = input.account.access_token_expires_at.into();
    let id = input.account_id.clone();
    let policy = Arc::new(MutableRuntimePolicy::new(refresh_policy_with_margin(
        Duration::from_secs(60),
    )));
    let (store, _, refresher, service) = fixture_many_with_state_and_runtime_policy(
        [input],
        [Ok(success_tokens(None))],
        true,
        Arc::new(CountingCredentialState::default()),
        policy.clone(),
    )
    .await;

    assert!(
        service
            .refresh_due()
            .await
            .expect("refresh cycle before margin change")
            .is_empty()
    );
    let account = store.account(&id).expect("account before margin change");
    assert_eq!(
        account.access_token_expires_at(),
        Some(access_token_expires_at)
    );
    assert!(account.next_refresh_at().is_none());
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 0);

    policy.set(refresh_policy_with_margin(Duration::from_secs(5 * 60)));
    assert!(matches!(
        service
            .refresh_due()
            .await
            .expect("refresh cycle after margin change")
            .as_slice(),
        [GrokCredentialRefreshOutcome::Refreshed { account_id, .. }] if account_id == &id
    ));
    assert!(
        store
            .account(&id)
            .expect("account after refresh")
            .next_refresh_at()
            .is_none(),
        "the runtime policy must not be persisted as a normal schedule"
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
    assert_eq!(
        store.last_error_message(&id).as_deref(),
        Some("refresh_invalid_grant")
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
    assert_eq!(
        store.last_error_message(&id).as_deref(),
        Some("account_banned")
    );
}

#[tokio::test]
async fn ambiguous_refresh_applies_bounded_oauth_backoff() {
    let input = due_input("ambiguous");
    let id = input.account_id.clone();
    let counting = Arc::new(CountingCredentialState::default());
    let (store, _, refresher, service) = fixture_many_with_state(
        [input],
        [Err(GrokRefreshFailure::Ambiguous)],
        true,
        counting.clone(),
    )
    .await;
    let started_at = SystemTime::now();
    let outcomes = service.refresh_due().await.expect("refresh");
    assert!(matches!(
        outcomes.as_slice(),
        [GrokCredentialRefreshOutcome::Ambiguous { account_id }] if account_id == &id
    ));
    assert_eq!(
        store.account(&id).expect("account").availability(),
        AccountAvailability::Unknown
    );
    let account = store.account(&id).expect("account");
    assert_eq!(account.revision().get(), 2);
    let retry_delay = account
        .next_refresh_at()
        .expect("OAuth retry scheduled")
        .duration_since(started_at)
        .expect("OAuth retry is in the future");
    assert!(
        retry_delay <= Duration::from_secs(7),
        "first retry must use bounded OAuth backoff, got {retry_delay:?}"
    );
    assert_eq!(counting.count(&id), 1);
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
async fn pre_send_transient_failure_applies_bounded_oauth_backoff() {
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
async fn exhausted_oauth_backoff_continues_after_access_token_expiry() {
    let input = due_input("backoff-recovery");
    let id = input.account_id.clone();
    let counting = Arc::new(CountingCredentialState::default());
    let (store, _, refresher, service) = fixture_many_with_state(
        [input],
        [
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
        ],
        true,
        counting.clone(),
    )
    .await;
    set_token_deadlines(
        &store,
        &id,
        (Utc::now() - chrono::Duration::minutes(1)).into(),
        SystemTime::now() - Duration::from_secs(1),
    )
    .await;

    for _ in 0..OAUTH_BACKOFF_ATTEMPTS {
        service.refresh_due().await.expect("backoff attempt");
        force_due(&store, &id).await;
    }

    let observed_at = SystemTime::now();
    service.refresh_due().await.expect("recovery attempt");
    let recovery_delay = store
        .account(&id)
        .expect("account after recovery defer")
        .next_refresh_at()
        .expect("recovery scheduled")
        .duration_since(observed_at)
        .expect("recovery is in the future");
    assert!(
        (Duration::from_secs(8 * 60)..=Duration::from_secs(12 * 60 + 1)).contains(&recovery_delay),
        "recovery delay should be about ten minutes, got {recovery_delay:?}"
    );
    assert_eq!(counting.count(&id), OAUTH_BACKOFF_ATTEMPTS + 1);
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn oauth_backoff_never_passes_an_unexpired_access_token_expiry() {
    let mut input = due_input("backoff-expiry-cap");
    input.account.access_token_expires_at = Utc::now() + chrono::Duration::seconds(30);
    let access_expires_at: SystemTime = input.account.access_token_expires_at.into();
    let id = input.account_id.clone();
    let counting = Arc::new(CountingCredentialState::default());
    let (store, _, _, service) = fixture_many_with_state(
        [input],
        [
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
            Err(GrokRefreshFailure::Transient),
        ],
        true,
        counting.clone(),
    )
    .await;

    for attempt in 0..=OAUTH_BACKOFF_ATTEMPTS {
        service.refresh_due().await.expect("backoff attempt");
        if attempt < OAUTH_BACKOFF_ATTEMPTS {
            force_due(&store, &id).await;
        }
    }

    assert_eq!(
        store
            .account(&id)
            .expect("account after capped retry")
            .next_refresh_at(),
        Some(access_expires_at)
    );
    assert_eq!(counting.count(&id), OAUTH_BACKOFF_ATTEMPTS + 1);
}

#[tokio::test]
async fn recovery_window_exhaustion_marks_account_expired_without_token_exchange() {
    let input = due_input("recovery-window-exhausted");
    let id = input.account_id.clone();
    let (store, _, refresher, service) = fixture(input, [Ok(success_tokens(None))], true).await;
    set_token_deadlines(
        &store,
        &id,
        (Utc::now() - chrono::Duration::hours(25)).into(),
        (Utc::now() + chrono::Duration::hours(12)).into(),
    )
    .await;

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(matches!(
        outcomes.as_slice(),
        [GrokCredentialRefreshOutcome::Invalidated { account_id }] if account_id == &id
    ));
    assert_eq!(
        store.account(&id).expect("expired account").availability(),
        AccountAvailability::Expired
    );
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(refresher.responses.lock().expect("queue").len(), 1);
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
    let access_expires_at = Utc::now() + chrono::Duration::minutes(2);
    let mut bad = due_input("bad");
    bad.account.access_token_expires_at = access_expires_at;
    let bad_id = bad.account_id.clone();
    let mut good = due_input("good");
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
async fn scheduled_oauth_refresh_runs_after_credential_rotation() {
    let input = due_input("refresh-after-rotation");
    let id = input.account_id.clone();
    let (store, _, refresher, service) = fixture(
        input,
        [
            Ok(success_tokens(Some("recovered-refresh"))),
            Ok(success_tokens(Some("scheduled-refresh"))),
        ],
        true,
    )
    .await;

    let outcome = service
        .recover_unauthorized(&id, CredentialRevision::new(1).expect("revision"))
        .await;
    assert_eq!(outcome, GrokCredentialRecoveryOutcome::Recovered);
    assert_eq!(store.account(&id).expect("account").revision().get(), 2);

    set_token_deadlines(
        &store,
        &id,
        (Utc::now() + chrono::Duration::minutes(2)).into(),
        SystemTime::now() - Duration::from_secs(1),
    )
    .await;
    let outcomes = service.refresh_due().await.expect("refresh due");
    assert!(matches!(
        outcomes.as_slice(),
        [GrokCredentialRefreshOutcome::Refreshed {
            account_id,
            credential_revision,
        }] if account_id == &id && credential_revision.get() == 4
    ));
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 2);
}
