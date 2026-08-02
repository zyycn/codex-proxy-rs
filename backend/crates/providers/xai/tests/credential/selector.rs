use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::Utc;
use gateway_core::engine::credential::{
    AccountAttemptFeedback, AccountAvailability, AccountFeedbackStats, AccountRuntimeSignals,
    AccountSelectionPolicy, CredentialCasOutcome, CredentialRevision, OpaqueProviderData,
    ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate, QuotaObservation,
    QuotaWriteOutcome, RotationStrategy,
};
use gateway_core::provider_ports::{
    ProviderCooldownPort, ProviderCooldownScope, ProviderLeaseAcquisition, ProviderLeasePort,
    ProviderLeaseRequest, ProviderSchedulingState, ProviderStoreError,
};
use gateway_core::routing::UpstreamModelId;
use provider_xai::{
    GrokAccountSessionSelector, GrokBillingRequest, GrokBillingTransport,
    GrokBillingTransportError, GrokBillingTransportErrorKind, GrokBillingTransportFuture,
    GrokCatalogScope, GrokCredentialAdmin, GrokCredentialAvailability, GrokCredentialCatalogCache,
    GrokCredentialCatalogSeed, GrokCredentialFailure, GrokCredentialRepository, GrokPlanCatalog,
    GrokSessionSelection, GrokSessionSelector, GrokSessionSelectorError,
    RotateManagedGrokCredential, UpdateGrokCredentialState,
};

use crate::support::{
    MemoryCooldownPort, MemoryGrokCatalogCache, MemoryProviderAccountStore, account_id,
    create_input, seed_input,
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
        _: &'a gateway_core::routing::ProviderKind,
        _: &'a [ProviderAccountId],
    ) -> futures::future::BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>> {
        Box::pin(async move {
            Ok(ProviderSchedulingState::new(
                self.signals.lock().expect("signals").clone(),
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
    cooldowns: Arc<MemoryCooldownPort>,
    feedback: Arc<AccountFeedbackStats>,
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
                    observed_at: Utc::now(),
                })
                .await
                .expect("ready account");
            let account = store.account(&input.account_id).expect("created account");
            cache
                .replace(GrokPlanCatalog::new(
                    GrokCatalogScope::for_account(&account).expect("catalog scope"),
                    Utc::now(),
                    GrokCredentialCatalogSeed::new(["grok-4.5", "grok-4.6"], None)
                        .expect("catalog"),
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
                    quota_limit_reached: false,
                    failure_rate_basis_points: None,
                    first_output_latency_ms: None,
                },
            );
        }
        let coordinator = Arc::new(SchedulingCoordinator {
            signals: Mutex::new(signals),
            denied: Mutex::new(BTreeSet::new()),
        });
        let cooldowns = Arc::new(MemoryCooldownPort::default());
        let catalog_cache: Arc<dyn GrokCredentialCatalogCache> = cache.clone();
        let lease_port: Arc<dyn ProviderLeasePort> = coordinator.clone();
        let quota = Arc::new(crate::support::grok_quota_service(
            repository.clone(),
            Arc::new(UnavailableBillingTransport),
        ));
        let feedback = Arc::new(AccountFeedbackStats::default());
        let selector = GrokAccountSessionSelector::new(
            gateway_core::routing::ProviderKind::new("xai").expect("provider"),
            repository.clone(),
            catalog_cache,
            quota,
            lease_port,
            cooldowns.clone(),
            Arc::clone(&feedback),
        );
        Self {
            store,
            cache,
            selector,
            coordinator,
            cooldowns,
            feedback,
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
        self.request_for_model_with_policy("grok-4.5", excluded, required_account, strategy)
    }

    fn request_for_model(
        &self,
        upstream_model: &str,
        required_account: Option<ProviderAccountId>,
    ) -> GrokSessionSelection {
        self.request_for_model_with_policy(
            upstream_model,
            BTreeSet::new(),
            required_account,
            RotationStrategy::Smart,
        )
    }

    fn request_for_model_with_policy(
        &self,
        upstream_model: &str,
        excluded: BTreeSet<ProviderAccountId>,
        required_account: Option<ProviderAccountId>,
        strategy: RotationStrategy,
    ) -> GrokSessionSelection {
        GrokSessionSelection::new(
            UpstreamModelId::new(upstream_model).expect("model"),
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
async fn unauthorized_feedback_records_runtime_cooldown_without_persisting_account_state() {
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
        AccountAvailability::Ready
    );
    let other = [account_id("feedback-a"), account_id("feedback-b")]
        .into_iter()
        .find(|id| id != &selected)
        .expect("other account");
    assert_eq!(
        fixture.store.account(&other).expect("other").availability(),
        AccountAvailability::Ready
    );
    assert!(
        fixture
            .cooldowns
            .cooldown(&selected)
            .is_some_and(|cooldown| cooldown.until() > SystemTime::now())
    );
}

#[tokio::test]
async fn account_scoped_cooldown_survives_credential_rotation() {
    let fixture = SelectorFixture::new(&["revision-fence"]).await;
    let id = account_id("revision-fence");
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
                retry_after: Some(Duration::from_secs(60)),
            },
        )
        .await;

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
            verified_account: crate::support::profile("subject-revision-fence"),
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

    // 账号级限流冷却跨凭据轮换保留：轮换后仍被冷却排除。
    assert!(matches!(
        fixture
            .selector
            .select(fixture.request(BTreeSet::new()))
            .await,
        Err(GrokSessionSelectorError::AccountCoolingDown { .. })
    ));
    let account = fixture.store.account(&id).expect("rotated account");
    assert_eq!(account.revision().get(), 2);
    assert_eq!(account.availability(), AccountAvailability::Ready);
}

#[tokio::test]
async fn rate_limit_feedback_persists_the_grok2api_cooldown_state() {
    let fixture = SelectorFixture::new(&["rate-limit", "available"]).await;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    let selected = session.account_id().clone();
    fixture
        .selector
        .record_failure(
            &session,
            GrokCredentialFailure::RateLimited {
                retry_after: Some(Duration::from_secs(5)),
            },
        )
        .await;
    let account = fixture.store.account(&selected).expect("account");
    assert_eq!(account.availability(), AccountAvailability::Ready);
    assert!(
        fixture
            .cooldowns
            .cooldown(&selected)
            .is_some_and(|cooldown| cooldown.until() > SystemTime::now())
    );
    let next = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("runtime cooldown should leave the other account available");
    assert_ne!(next.account_id(), &selected);
}

#[tokio::test]
async fn successful_request_clears_the_persisted_cooldown_state() {
    let fixture = SelectorFixture::new(&["cooldown-success"]).await;
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

    fixture.selector.record_success(&session).await;

    let account = fixture
        .store
        .account(session.account_id())
        .expect("account");
    assert_eq!(account.availability(), AccountAvailability::Ready);
}

#[tokio::test]
async fn payment_required_feedback_writes_short_account_cooldown_without_persisting_exhaustion() {
    let fixture = SelectorFixture::new(&["payment-required", "available"]).await;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    let selected = session.account_id().clone();
    fixture
        .selector
        .record_failure(
            &session,
            GrokCredentialFailure::PaymentRequired {
                retry_after: Some(Duration::from_secs(5)),
            },
        )
        .await;

    // bare 402 无结构化 quota code：只写短期账号 runtime cooldown，
    // 不持久化 QuotaExhausted（避免长期错误状态）。
    assert_eq!(
        fixture
            .store
            .account(&selected)
            .expect("selected account")
            .availability(),
        AccountAvailability::Ready
    );
    assert!(
        fixture
            .cooldowns
            .cooldown(&selected)
            .is_some_and(|cooldown| cooldown.until() > SystemTime::now())
    );
    let next = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("another account remains available");
    assert_ne!(next.account_id(), &selected);
}

#[tokio::test]
async fn model_quota_feedback_writes_model_scoped_cooldown_without_blocking_other_models() {
    let fixture = SelectorFixture::new(&["model-quota", "available"]).await;
    let failed_model = UpstreamModelId::new("grok-4.5").expect("failed model");
    let session = fixture
        .selector
        .select(fixture.request_for_model(failed_model.as_str(), None))
        .await
        .expect("session");
    let selected = session.account_id().clone();
    fixture
        .selector
        .record_failure(
            &session,
            GrokCredentialFailure::ModelQuotaExhausted {
                upstream_model: failed_model.clone(),
                retry_after: None,
            },
        )
        .await;

    let account = fixture.store.account(&selected).expect("selected account");
    assert_eq!(account.availability(), AccountAvailability::Ready);
    let minimum_until = SystemTime::now() + Duration::from_secs(23 * 60 * 60);
    let scope = ProviderCooldownScope::upstream_model(failed_model);
    // model-scoped cooldown：目标模型被排除，账号级无 cooldown。
    assert!(
        fixture
            .cooldowns
            .scoped_cooldown(&selected, &scope)
            .is_some_and(|cooldown| cooldown.until() > minimum_until)
    );
    assert!(fixture.cooldowns.cooldown(&selected).is_none());
    // 失败模型不可选（ModelCoolingDown 由 model-scoped cooldown 派生）。
    assert!(matches!(
        fixture
            .selector
            .select(fixture.request_for_model("grok-4.5", Some(selected.clone())))
            .await,
        Err(GrokSessionSelectorError::ModelCoolingDown {
            retry_after: Some(_)
        })
    ));
    // 另一模型不受 model cooldown 影响：同一账号在 grok-4.6 下仍可选。
    let other_model = fixture
        .selector
        .select(fixture.request_for_model("grok-4.6", Some(selected.clone())))
        .await
        .expect("model-scoped cooldown must not block other models");
    assert_eq!(other_model.account_id(), &selected);
}

#[tokio::test]
async fn model_access_feedback_reports_model_cooldown_without_blocking_the_account() {
    let fixture = SelectorFixture::new(&["model-access"]).await;
    let model = UpstreamModelId::new("grok-4.5").expect("model");
    let session = fixture
        .selector
        .select(fixture.request_for_model(model.as_str(), None))
        .await
        .expect("session");
    fixture
        .selector
        .record_failure(
            &session,
            GrokCredentialFailure::ModelAccessDenied {
                upstream_model: model,
                retry_after: None,
            },
        )
        .await;

    assert_eq!(
        fixture
            .store
            .account(session.account_id())
            .expect("account")
            .availability(),
        AccountAvailability::Ready
    );
    assert!(matches!(
        fixture
            .selector
            .select(fixture.request_for_model("grok-4.5", None))
            .await,
        Err(GrokSessionSelectorError::ModelCoolingDown {
            retry_after: Some(_)
        })
    ));
}

#[tokio::test]
async fn interrupted_stream_feedback_persists_the_grok2api_cooldown_state() {
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
    assert_eq!(account.availability(), AccountAvailability::Ready);
    assert!(
        fixture
            .cooldowns
            .cooldown(session.account_id())
            .is_some_and(|cooldown| cooldown.until() > SystemTime::now())
    );
    let retry_after = match fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
    {
        Err(GrokSessionSelectorError::AccountCoolingDown { retry_after }) => retry_after,
        other => panic!("expected AccountCoolingDown, got {other:?}"),
    };
    let retry_after = retry_after.expect("retry_after");
    assert!(retry_after <= Duration::from_secs(30));
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
async fn quota_feedback_should_follow_the_account_across_a_credential_rotation() {
    let fixture = SelectorFixture::new(&["quota-rotation"]).await;
    let id = account_id("quota-rotation");
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
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
            verified_account: crate::support::profile("subject-quota-rotation"),
            next_refresh_at: chrono::Utc::now() + chrono::Duration::minutes(30),
        })
        .expect("rotate");
    fixture
        .store
        .compare_and_swap_credential(prepared.credential)
        .await
        .expect("persist rotation");

    fixture
        .selector
        .record_failure(&session, GrokCredentialFailure::FreeQuotaExhausted)
        .await;

    assert_eq!(
        fixture.store.account(&id).expect("account").availability(),
        AccountAvailability::QuotaExhausted
    );
}

#[tokio::test]
async fn ordinary_success_should_not_clear_a_confirmed_quota_exhaustion() {
    let fixture = SelectorFixture::new(&["quota-success"]).await;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    fixture
        .selector
        .record_failure(&session, GrokCredentialFailure::FreeQuotaExhausted)
        .await;

    fixture.selector.record_success(&session).await;

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
    let supported_id = account_id("zzz-supported");
    let supported = fixture
        .store
        .account(&supported_id)
        .expect("supported account");
    fixture
        .store
        .update_account(ProviderAccountUpdate {
            account_id: supported_id.clone(),
            name: supported.name().to_owned(),
            email: supported.email().map(str::to_owned),
            plan_type: Some("premium".to_owned()),
        })
        .await
        .expect("move supported account to another plan");
    let unsupported = fixture
        .store
        .account(&account_id("aaa-unsupported"))
        .expect("unsupported account");
    let supported = fixture
        .store
        .account(&supported_id)
        .expect("supported account");
    fixture
        .cache
        .replace(GrokPlanCatalog::new(
            GrokCatalogScope::for_account(&unsupported).expect("catalog scope"),
            Utc::now(),
            GrokCredentialCatalogSeed::new(["grok-other"], None).expect("catalog"),
        ))
        .await
        .expect("replace catalog");
    fixture
        .cache
        .replace(GrokPlanCatalog::new(
            GrokCatalogScope::for_account(&supported).expect("catalog scope"),
            Utc::now(),
            GrokCredentialCatalogSeed::new(["grok-4.5"], None).expect("catalog"),
        ))
        .await
        .expect("cache supported plan");

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
async fn smart_strategy_uses_common_account_health_feedback() {
    let fixture = SelectorFixture::new(&["aaa-unhealthy", "zzz-healthy"]).await;
    let provider = gateway_core::routing::ProviderKind::new("xai").expect("provider");
    let unhealthy = account_id("aaa-unhealthy");
    for _ in 0..4 {
        fixture.feedback.report(
            &provider,
            &unhealthy,
            AccountAttemptFeedback::Failed {
                first_output_ms: None,
            },
        );
    }

    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("healthy account");

    assert_eq!(session.account_id(), &account_id("zzz-healthy"));
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

#[tokio::test]
async fn deleting_account_clears_all_account_and_model_scoped_cooldowns() {
    // 删除账号后其 account/model scoped cooldown key 全部清除。
    let fixture = SelectorFixture::new(&["clear-cooldowns", "available"]).await;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    let selected = session.account_id().clone();
    let model = UpstreamModelId::new("grok-4.5").expect("model");
    // 写 account cooldown + model scoped cooldown。
    fixture
        .selector
        .record_failure(
            &session,
            GrokCredentialFailure::RateLimited {
                retry_after: Some(Duration::from_secs(60)),
            },
        )
        .await;
    fixture
        .selector
        .record_failure(
            &session,
            GrokCredentialFailure::ModelQuotaExhausted {
                upstream_model: model.clone(),
                retry_after: Some(Duration::from_secs(3600)),
            },
        )
        .await;
    assert!(fixture.cooldowns.cooldown(&selected).is_some());
    assert!(
        fixture
            .cooldowns
            .scoped_cooldown(
                &selected,
                &ProviderCooldownScope::upstream_model(model.clone())
            )
            .is_some()
    );

    // 删除账号（clear_all 清全部）。
    fixture
        .cooldowns
        .clear_all(&selected)
        .await
        .expect("clear all cooldowns");
    assert!(fixture.cooldowns.cooldown(&selected).is_none());
    assert!(
        fixture
            .cooldowns
            .scoped_cooldown(
                &selected,
                &ProviderCooldownScope::upstream_model(model.clone())
            )
            .is_none()
    );
}

#[tokio::test]
async fn bare_402_cooldown_expires_and_account_recovers_without_persisted_exhaustion() {
    // bare 402 只写短期 account cooldown，不持久化
    // QuotaExhausted；退避到期后账号自动恢复可选。
    let fixture = SelectorFixture::new(&["payment-recover", "available"]).await;
    let session = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("session");
    let selected = session.account_id().clone();
    fixture
        .selector
        .record_failure(
            &session,
            GrokCredentialFailure::PaymentRequired {
                retry_after: Some(Duration::from_secs(1)),
            },
        )
        .await;

    // 退避活跃期间该账号被排除，另一账号可选。
    assert_eq!(
        fixture
            .store
            .account(&selected)
            .expect("account")
            .availability(),
        AccountAvailability::Ready,
        "bare 402 must not persist QuotaExhausted"
    );
    let next = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("another account available during cooldown");
    assert_ne!(next.account_id(), &selected);

    // 等短 cooldown 到期。
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let recovered = fixture
        .selector
        .select(fixture.request(BTreeSet::new()))
        .await
        .expect("account recovers after cooldown expiry");
    assert_eq!(recovered.account_id(), &selected);
}
