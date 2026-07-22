use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use gateway_core::engine::credential::{
    AccountAvailability, CredentialCasOutcome, CredentialRevision, ProviderAccountStore,
};
use gateway_core::provider_ports::{
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
    MemoryGrokCatalogCache, MemoryProviderAccountStore, create_input, credential_object,
    runtime_policy, seed_input,
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
                None,
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
        runtime_policy(),
    );
    (store, repository, refresher, service)
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

    let prepared = service
        .prepare_manual_refresh(&id, CredentialRevision::new(1).expect("revision"))
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
async fn manual_refresh_rejects_stale_revision_before_upstream_call() {
    let input = due_input("manual-stale");
    let id = input.account_id.clone();
    let (_, _, refresher, service) = fixture(input, [], true).await;

    assert!(matches!(
        service
            .prepare_manual_refresh(&id, CredentialRevision::new(2).expect("revision"))
            .await,
        Err(GrokCredentialRefreshError::Repository(
            provider_xai::GrokCredentialRepositoryError::StaleCredentialRevision
        ))
    ));
    assert_eq!(refresher.prepare_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn manual_refresh_preserves_failure_class_without_store_write() {
    let input = due_input("manual-invalid");
    let id = input.account_id.clone();
    let (store, _, _, service) =
        fixture(input, [Err(GrokRefreshFailure::InvalidGrant)], true).await;

    assert!(matches!(
        service
            .prepare_manual_refresh(&id, CredentialRevision::new(1).expect("revision"))
            .await,
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
async fn ambiguous_refresh_fails_closed_and_is_not_retried() {
    let input = due_input("ambiguous");
    let id = input.account_id.clone();
    let (store, _, _, service) = fixture(input, [Err(GrokRefreshFailure::Ambiguous)], true).await;
    let outcomes = service.refresh_due().await.expect("refresh");
    assert!(matches!(
        outcomes.as_slice(),
        [GrokCredentialRefreshOutcome::Ambiguous { account_id }] if account_id == &id
    ));
    assert_eq!(
        store.account(&id).expect("account").availability(),
        AccountAvailability::Invalid
    );
    assert!(
        service
            .refresh_due()
            .await
            .expect("second cycle")
            .is_empty()
    );
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
