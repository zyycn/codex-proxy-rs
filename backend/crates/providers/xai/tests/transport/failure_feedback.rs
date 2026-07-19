use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::Utc;
use gateway_core::engine::credential::{
    AccountAvailability, AccountRuntimeSignals, AccountSelectionPolicy, CredentialCasOutcome,
    CredentialRevision, ProviderAccountId, ProviderAccountStore, RotationStrategy,
};
use provider_xai::{
    GrokAccountCatalog, GrokAccountSchedulingState, GrokAccountSessionSelector,
    GrokCredentialAdmin, GrokCredentialAvailability, GrokCredentialCatalogCache,
    GrokCredentialCatalogSeed, GrokCredentialFailure, GrokCredentialLeaseAcquisition,
    GrokCredentialLeaseCoordinator, GrokCredentialLeaseCoordinatorError,
    GrokCredentialLeaseRequest, GrokCredentialRepository, GrokSessionSelection,
    GrokSessionSelector, GrokSessionSelectorError, RotateManagedGrokCredential,
    UpdateGrokCredentialState,
};

use crate::support::{
    MemoryGrokCatalogCache, MemoryProviderAccountStore, account_id, create_input, instance_id,
    seed_input,
};

struct SchedulingCoordinator {
    signals: Mutex<BTreeMap<ProviderAccountId, AccountRuntimeSignals>>,
    denied: Mutex<BTreeSet<ProviderAccountId>>,
}

#[async_trait]
impl GrokCredentialLeaseCoordinator for SchedulingCoordinator {
    async fn load_scheduling_state(
        &self,
        _: &gateway_core::routing::ProviderInstanceId,
        _: &[ProviderAccountId],
    ) -> Result<GrokAccountSchedulingState, GrokCredentialLeaseCoordinatorError> {
        Ok(GrokAccountSchedulingState {
            signals: self.signals.lock().expect("signals").clone(),
            sticky_account: None,
            round_robin_cursor: 0,
        })
    }

    async fn try_acquire(
        &self,
        request: &GrokCredentialLeaseRequest,
    ) -> Result<GrokCredentialLeaseAcquisition, GrokCredentialLeaseCoordinatorError> {
        Ok(
            if self
                .denied
                .lock()
                .expect("denied")
                .contains(&request.account_id)
            {
                GrokCredentialLeaseAcquisition::Unavailable {
                    retry_after: Some(Duration::from_millis(25)),
                }
            } else {
                GrokCredentialLeaseAcquisition::Acquired(Box::new(()))
            },
        )
    }
}

struct SelectorFixture {
    store: Arc<MemoryProviderAccountStore>,
    selector: GrokAccountSessionSelector,
    coordinator: Arc<SchedulingCoordinator>,
}

impl SelectorFixture {
    async fn new(suffixes: &[&str]) -> Self {
        let store = MemoryProviderAccountStore::shared();
        let account_store: Arc<dyn ProviderAccountStore> = store.clone();
        let repository = GrokCredentialRepository::new(account_store);
        let cache = MemoryGrokCatalogCache::shared();
        let mut signals = BTreeMap::new();
        for suffix in suffixes {
            let input = create_input(suffix, &format!("subject-{suffix}"));
            seed_input(&store, &input).await.expect("create account");
            repository
                .update_state(&UpdateGrokCredentialState {
                    account_id: input.account_id.clone(),
                    expected_revision: CredentialRevision::new(1).expect("revision"),
                    availability: GrokCredentialAvailability::Ready,
                    availability_reason: None,
                    cooldown_until: None,
                    observed_at: Utc::now(),
                })
                .await
                .expect("ready account");
            cache
                .replace(GrokAccountCatalog::new(
                    input.account_id.clone(),
                    CredentialRevision::new(1).expect("revision"),
                    Utc::now(),
                    GrokCredentialCatalogSeed::new(["grok-4.5"], None).expect("catalog"),
                ))
                .await
                .expect("cache catalog");
            signals.insert(
                input.account_id,
                AccountRuntimeSignals {
                    in_flight: 0,
                    last_started_at: None,
                    quota_reset_at: None,
                    quota_remaining_rank: Some(100),
                },
            );
        }
        let coordinator = Arc::new(SchedulingCoordinator {
            signals: Mutex::new(signals),
            denied: Mutex::new(BTreeSet::new()),
        });
        let catalog_cache: Arc<dyn GrokCredentialCatalogCache> = cache;
        let lease_port: Arc<dyn GrokCredentialLeaseCoordinator> = coordinator.clone();
        let selector =
            GrokAccountSessionSelector::new(repository.clone(), catalog_cache, lease_port);
        Self {
            store,
            selector,
            coordinator,
        }
    }

    fn request(&self, excluded: BTreeSet<ProviderAccountId>) -> GrokSessionSelection {
        self.request_with_required(excluded, None)
    }

    fn request_with_required(
        &self,
        excluded: BTreeSet<ProviderAccountId>,
        required_account: Option<ProviderAccountId>,
    ) -> GrokSessionSelection {
        GrokSessionSelection::new(
            instance_id(),
            gateway_core::routing::UpstreamModelId::new("grok-4.5").expect("model"),
            excluded,
            required_account,
            AccountSelectionPolicy::new(
                RotationStrategy::Smart,
                std::num::NonZeroU32::new(2).expect("limit"),
                Duration::ZERO,
            ),
        )
    }
}

#[tokio::test]
async fn required_account_overrides_smart_selection_without_fallback() {
    let fixture = SelectorFixture::new(&["required-busy", "required-idle"]).await;
    fixture
        .coordinator
        .signals
        .lock()
        .expect("signals")
        .get_mut(&account_id("required-busy"))
        .expect("busy signal")
        .in_flight = 1;
    let required = account_id("required-busy");
    let session = fixture
        .selector
        .select(fixture.request_with_required(BTreeSet::new(), Some(required.clone())))
        .await
        .expect("required session");
    assert_eq!(session.account_id(), &required);

    fixture
        .coordinator
        .denied
        .lock()
        .expect("denied")
        .insert(required.clone());
    assert!(matches!(
        fixture
            .selector
            .select(fixture.request_with_required(BTreeSet::new(), Some(required)))
            .await,
        Err(GrokSessionSelectorError::CapacityUnavailable { .. })
    ));
}

#[tokio::test]
async fn unauthorized_feedback_marks_only_selected_account_invalid() {
    let fixture = SelectorFixture::new(&["feedback-a", "feedback-b"]).await;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    let selected = session.account_id().clone();
    fixture
        .selector
        .record_failure(&session, GrokCredentialFailure::Unauthorized)
        .await;
    assert_eq!(
        fixture
            .store
            .account(&selected)
            .expect("selected")
            .availability(),
        AccountAvailability::Invalid
    );
    let other = [account_id("feedback-a"), account_id("feedback-b")]
        .into_iter()
        .find(|id| id != &selected)
        .expect("other account");
    assert_eq!(
        fixture.store.account(&other).expect("other").availability(),
        AccountAvailability::Ready
    );
}

#[tokio::test]
async fn rate_limit_feedback_applies_bounded_cooldown() {
    let fixture = SelectorFixture::new(&["rate-limit"]).await;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    fixture
        .selector
        .record_failure(
            &session,
            GrokCredentialFailure::RateLimited {
                retry_after: Some(Duration::from_secs(5)),
            },
        )
        .await;
    let account = fixture
        .store
        .account(session.account_id())
        .expect("account");
    assert_eq!(account.availability(), AccountAvailability::Cooldown);
    assert!(
        account
            .cooldown_until()
            .is_some_and(|until| until > SystemTime::now())
    );
}

#[tokio::test]
async fn quota_feedback_uses_common_quota_exhausted_state() {
    let fixture = SelectorFixture::new(&["quota"]).await;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    fixture
        .selector
        .record_failure(&session, GrokCredentialFailure::QuotaExhausted)
        .await;
    assert_eq!(
        fixture
            .store
            .account(session.account_id())
            .expect("account")
            .availability(),
        AccountAvailability::QuotaExhausted
    );
}

#[tokio::test]
async fn excluded_account_is_never_selected_again() {
    let fixture = SelectorFixture::new(&["excluded"]).await;
    let excluded = BTreeSet::from([account_id("excluded")]);
    assert!(matches!(
        fixture.selector.select(fixture.request(excluded)).await,
        Err(GrokSessionSelectorError::NoEligibleSession)
    ));
}

#[tokio::test]
async fn capacity_denial_returns_minimum_retry_without_upstream_send() {
    let fixture = SelectorFixture::new(&["denied-a", "denied-b"]).await;
    fixture
        .coordinator
        .denied
        .lock()
        .expect("denied")
        .extend([account_id("denied-a"), account_id("denied-b")]);
    assert!(matches!(
        fixture
            .selector
            .select(fixture.request(BTreeSet::new()))
            .await,
        Err(GrokSessionSelectorError::CapacityUnavailable {
            retry_after: Some(value)
        }) if value == Duration::from_millis(25)
    ));
}

#[tokio::test]
async fn smart_strategy_prefers_lower_in_flight_account() {
    let fixture = SelectorFixture::new(&["busy", "idle"]).await;
    fixture
        .coordinator
        .signals
        .lock()
        .expect("signals")
        .get_mut(&account_id("busy"))
        .expect("busy signal")
        .in_flight = 1;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    assert_eq!(session.account_id(), &account_id("idle"));
}

#[tokio::test]
async fn stale_catalog_revision_fails_closed() {
    let fixture = SelectorFixture::new(&["catalog-stale"]).await;
    let id = account_id("catalog-stale");
    let current = fixture
        .store
        .load_credential(&id, CredentialRevision::new(1).expect("revision"))
        .await
        .expect("current credential");
    let prepared = GrokCredentialAdmin
        .prepare_rotation(&RotateManagedGrokCredential {
            current,
            secret: provider_xai::GrokOAuthSecret {
                access_token: provider_xai::SecretValue::new("new-access"),
                refresh_token: provider_xai::SecretValue::new("new-refresh"),
                id_token: None,
                scope: provider_xai::OFFICIAL_SCOPES.join(" "),
            },
            verified_account: crate::support::profile("subject-catalog-stale"),
        })
        .expect("rotate");
    assert!(matches!(
        fixture
            .store
            .compare_and_swap_credential(prepared.credential)
            .await
            .expect("persist rotation"),
        CredentialCasOutcome::Updated(revision) if revision.get() == 2
    ));
    assert!(matches!(
        fixture
            .selector
            .select(fixture.request(BTreeSet::new()))
            .await,
        Err(GrokSessionSelectorError::NoEligibleSession)
    ));
}
