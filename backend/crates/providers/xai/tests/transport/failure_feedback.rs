use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::Utc;
use gateway_core::engine::credential::{
    AccountAvailability, AccountRuntimeSignals, AccountSelectionPolicy, CredentialCasOutcome,
    CredentialRevision, OpaqueProviderData, ProviderAccountId, ProviderAccountStore,
    QuotaObservation, QuotaWriteOutcome, RotationStrategy,
};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest, ProviderSchedulingState,
    ProviderStoreError,
};
use provider_xai::{
    GrokAccountCatalog, GrokAccountSessionSelector, GrokBillingRequest, GrokBillingTransport,
    GrokBillingTransportError, GrokBillingTransportErrorKind, GrokBillingTransportFuture,
    GrokCredentialAdmin, GrokCredentialAvailability, GrokCredentialCatalogCache,
    GrokCredentialCatalogSeed, GrokCredentialFailure, GrokCredentialQuotaService,
    GrokCredentialRepository, GrokSessionSelection, GrokSessionSelector, GrokSessionSelectorError,
    RotateManagedGrokCredential, UpdateGrokCredentialState,
};

use crate::support::{
    MemoryGrokCatalogCache, MemoryProviderAccountStore, account_id, create_input, instance_id,
    seed_input,
};

struct SchedulingCoordinator {
    signals: Mutex<BTreeMap<ProviderAccountId, AccountRuntimeSignals>>,
    denied: Mutex<BTreeSet<ProviderAccountId>>,
}

struct UnavailableBillingTransport;

impl GrokBillingTransport for UnavailableBillingTransport {
    fn execute(&self, _: GrokBillingRequest) -> GrokBillingTransportFuture<'_> {
        Box::pin(async {
            Err(GrokBillingTransportError::new(
                GrokBillingTransportErrorKind::Unavailable,
            ))
        })
    }
}

impl ProviderLeasePort for SchedulingCoordinator {
    fn load_state<'a>(
        &'a self,
        _: &'a gateway_core::routing::ProviderInstanceId,
        _: &'a [ProviderAccountId],
    ) -> futures::future::BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>> {
        Box::pin(async move {
            Ok(ProviderSchedulingState::new(
                self.signals.lock().expect("signals").clone(),
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
            let ProviderLeaseRequest::Scheduling(request) = request else {
                panic!("expected scheduling lease request");
            };
            Ok(
                if self
                    .denied
                    .lock()
                    .expect("denied")
                    .contains(request.account_id())
                {
                    ProviderLeaseAcquisition::Busy {
                        retry_after: Some(Duration::from_millis(25)),
                    }
                } else {
                    ProviderLeaseAcquisition::Acquired(Box::new(()))
                },
            )
        })
    }
}

struct SelectorFixture {
    store: Arc<MemoryProviderAccountStore>,
    cache: Arc<MemoryGrokCatalogCache>,
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
                    quota_remaining_rank: None,
                },
            );
        }
        let coordinator = Arc::new(SchedulingCoordinator {
            signals: Mutex::new(signals),
            denied: Mutex::new(BTreeSet::new()),
        });
        let catalog_cache: Arc<dyn GrokCredentialCatalogCache> = cache.clone();
        let lease_port: Arc<dyn ProviderLeasePort> = coordinator.clone();
        let quota = Arc::new(GrokCredentialQuotaService::new(
            repository.clone(),
            Arc::new(UnavailableBillingTransport),
        ));
        let selector =
            GrokAccountSessionSelector::new(repository.clone(), catalog_cache, quota, lease_port);
        Self {
            store,
            cache,
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
        self.request_with_policy(excluded, required_account, RotationStrategy::Smart)
    }

    fn request_with_policy(
        &self,
        excluded: BTreeSet<ProviderAccountId>,
        required_account: Option<ProviderAccountId>,
        strategy: RotationStrategy,
    ) -> GrokSessionSelection {
        GrokSessionSelection::new(
            instance_id(),
            gateway_core::routing::UpstreamModelId::new("grok-4.5").expect("model"),
            excluded,
            required_account,
            AccountSelectionPolicy::new(
                strategy,
                std::num::NonZeroU32::new(2).expect("limit"),
                Duration::ZERO,
            ),
            SystemTime::now() + Duration::from_secs(30),
        )
    }

    async fn seed_quota(&self, id: &ProviderAccountId, used_percent: f64, reset_after: Duration) {
        let reset_at = (Utc::now()
            + chrono::Duration::from_std(reset_after).expect("valid reset duration"))
        .to_rfc3339();
        let document = serde_json::json!({
            "config": {
                "creditUsagePercent": used_percent,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": Utc::now().to_rfc3339(),
                    "end": reset_at
                }
            }
        });
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: id.clone(),
                expected_revision: CredentialRevision::new(1).expect("revision"),
                quota: Some(OpaqueProviderData::new(
                    document.as_object().expect("quota object").clone(),
                )),
                observed_at: Some(SystemTime::now()),
            })
            .await
            .expect("persist quota");
        assert_eq!(outcome, QuotaWriteOutcome::Updated);
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
async fn interrupted_stream_feedback_applies_account_cooldown() {
    let fixture = SelectorFixture::new(&["stream-interrupted"]).await;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    fixture
        .selector
        .record_failure(&session, GrokCredentialFailure::StreamInterrupted)
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
async fn stale_catalog_revision_does_not_block_transparent_request() {
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
            next_refresh_at: chrono::Utc::now() + chrono::Duration::minutes(30),
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
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("stale auxiliary catalog must not block selection");
    assert_eq!(session.account_id(), &id);
}

#[tokio::test]
async fn explicit_catalog_non_membership_excludes_only_unsupported_account() {
    let fixture = SelectorFixture::new(&["aaa-unsupported", "zzz-supported"]).await;
    fixture
        .cache
        .replace(GrokAccountCatalog::new(
            account_id("aaa-unsupported"),
            CredentialRevision::new(1).expect("revision"),
            Utc::now(),
            GrokCredentialCatalogSeed::new(["grok-other"], None).expect("catalog"),
        ))
        .await
        .expect("replace catalog");

    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("supported account");

    assert_eq!(session.account_id(), &account_id("zzz-supported"));
}

#[tokio::test]
async fn smart_strategy_uses_fresh_provider_quota_after_load_ties() {
    let fixture = SelectorFixture::new(&["aaa-low-quota", "zzz-high-quota"]).await;
    fixture
        .seed_quota(&account_id("aaa-low-quota"), 90.0, Duration::from_secs(600))
        .await;
    fixture
        .seed_quota(
            &account_id("zzz-high-quota"),
            10.0,
            Duration::from_secs(600),
        )
        .await;

    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("quota-ranked account");

    assert_eq!(session.account_id(), &account_id("zzz-high-quota"));
}

#[tokio::test]
async fn smart_strategy_never_reuses_quota_projection_after_credential_rotation() {
    let fixture = SelectorFixture::new(&["aaa-stale-high", "zzz-current-low"]).await;
    let stale = account_id("aaa-stale-high");
    fixture
        .seed_quota(&stale, 5.0, Duration::from_secs(600))
        .await;
    fixture
        .seed_quota(
            &account_id("zzz-current-low"),
            95.0,
            Duration::from_secs(600),
        )
        .await;
    let first = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("initial quota-ranked account");
    assert_eq!(first.account_id(), &stale);
    drop(first);

    let current = fixture
        .store
        .load_credential(&stale, CredentialRevision::new(1).expect("revision"))
        .await
        .expect("current credential");
    let prepared = GrokCredentialAdmin
        .prepare_rotation(&RotateManagedGrokCredential {
            current,
            secret: provider_xai::GrokOAuthSecret {
                access_token: provider_xai::SecretValue::new("rotated-access"),
                refresh_token: provider_xai::SecretValue::new("rotated-refresh"),
                id_token: None,
                scope: provider_xai::OFFICIAL_SCOPES.join(" "),
            },
            verified_account: crate::support::profile("subject-aaa-stale-high"),
            next_refresh_at: chrono::Utc::now() + chrono::Duration::minutes(30),
        })
        .expect("rotate");
    let outcome = fixture
        .store
        .compare_and_swap_credential(prepared.credential)
        .await
        .expect("persist rotation");
    assert!(matches!(outcome, CredentialCasOutcome::Updated(revision) if revision.get() == 2));

    let selected = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("current quota-ranked account");

    assert_eq!(selected.account_id(), &account_id("zzz-current-low"));
}

#[tokio::test]
async fn quota_reset_strategy_uses_provider_reported_earliest_reset() {
    let fixture = SelectorFixture::new(&["aaa-later-reset", "zzz-earlier-reset"]).await;
    fixture
        .seed_quota(
            &account_id("aaa-later-reset"),
            10.0,
            Duration::from_secs(1_200),
        )
        .await;
    fixture
        .seed_quota(
            &account_id("zzz-earlier-reset"),
            90.0,
            Duration::from_secs(600),
        )
        .await;
    let request =
        fixture.request_with_policy(BTreeSet::new(), None, RotationStrategy::QuotaResetPriority);

    let session = fixture
        .selector
        .select(request)
        .await
        .expect("reset-ranked account");

    assert_eq!(session.account_id(), &account_id("zzz-earlier-reset"));
}
