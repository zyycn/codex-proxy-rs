//! 验证 xAI 额度观察、revision隔离、权限状态与调度投影

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use futures::future::join_all;
use gateway_core::account::{
    AccountStateChange, CredentialCasUpdate, CredentialRevision, CredentialState,
    OpaqueProviderData, ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate,
    QuotaAccessChange, QuotaEvidence, QuotaObservation, QuotaState,
};
use provider_xai::{
    GROK_SUBSCRIPTION_URL, GrokBillingRequest, GrokBillingTransport, GrokBillingTransportError,
    GrokBillingTransportErrorKind, GrokBillingTransportFuture, GrokBillingTransportResponse,
    GrokCredentialRepository, GrokQuotaError,
};

use crate::support::{MemoryProviderAccountStore, account_id, create_input, seed_input};

struct QueueBillingTransport {
    calls: AtomicUsize,
    subscription: Option<Vec<u8>>,
    responses: Mutex<VecDeque<Result<GrokBillingTransportResponse, GrokBillingTransportError>>>,
}

impl QueueBillingTransport {
    fn success(body: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            subscription: None,
            responses: Mutex::new(VecDeque::from([Ok(GrokBillingTransportResponse::new(
                body,
            ))])),
        })
    }

    fn failure() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            subscription: None,
            responses: Mutex::new(VecDeque::from([Err(GrokBillingTransportError::new(
                GrokBillingTransportErrorKind::Unavailable,
            ))])),
        })
    }
}

impl GrokBillingTransport for QueueBillingTransport {
    fn execute(&self, request: GrokBillingRequest) -> GrokBillingTransportFuture<'_> {
        if request.endpoint().as_str() == GROK_SUBSCRIPTION_URL {
            let response = self
                .subscription
                .clone()
                .map(GrokBillingTransportResponse::new)
                .ok_or_else(|| {
                    GrokBillingTransportError::new(GrokBillingTransportErrorKind::Unavailable)
                });
            return Box::pin(async move { response });
        }
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

/// 每次读取都返回同一份无法解码的 catalog 文档的存储端口
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
    fn execute(&self, request: GrokBillingRequest) -> GrokBillingTransportFuture<'_> {
        if request.endpoint().as_str() == GROK_SUBSCRIPTION_URL {
            return Box::pin(async {
                Err(GrokBillingTransportError::new(
                    GrokBillingTransportErrorKind::Unavailable,
                ))
            });
        }
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
    credential_state: CredentialState,
) {
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: id.clone(),
            expected_revision: CredentialRevision::new(1).expect("revision"),
            credential_state,
            error_reason: credential_state.error_reason(),
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
    let document = persisted.quota;

    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(snapshot.billing().used_percent(), Some(37.5));
    assert_eq!(snapshot.billing().plan_type(), None);
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
async fn quota_refresh_persists_subscription_without_user_profile_and_reads_it_from_cache() {
    let (store, repository) =
        repository_with_accounts(&[("subscription", "subject-subscription")]).await;
    let mut transport = QueueBillingTransport::success(br#"{"config":{"creditUsagePercent":25}}"#);
    Arc::get_mut(&mut transport).expect("unique transport").subscription = Some(
        br#"{"userId":"verified-user","email":"private@example.com","subscriptionTier":"SuperGrokPro"}"#.to_vec(),
    );
    let service = crate::support::grok_quota_service(repository, transport.clone());
    let snapshot = service
        .refresh_account(&account_id("subscription"))
        .await
        .expect("refresh quota");
    assert_eq!(snapshot.billing().plan_type(), Some("SuperGrokPro"));
    let cached = service
        .read_account(&account_id("subscription"))
        .await
        .expect("read quota")
        .expect("cached quota");
    assert_eq!(cached.billing().plan_type(), Some("SuperGrokPro"));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    let stored = store
        .get_quotas(&[account_id("subscription")])
        .await
        .expect("persisted quota");
    let document = stored[0].quota.expose_to_provider();
    assert_eq!(document["subscriptionTier"], "SuperGrokPro");
    assert!(document.get("email").is_none());
    assert!(document.get("userId").is_none());
}

#[tokio::test]
async fn billing_access_and_scheduling_use_current_quota_fields() {
    use gateway_core::account::QuotaAccessState::{Allowed, Exhausted};

    for (config, expected_percent, expected_access) in [
        (
            serde_json::json!({"creditUsagePercent": 0}),
            Some(0.0),
            Allowed,
        ),
        (
            serde_json::json!({"creditUsagePercent": 100}),
            Some(100.0),
            Exhausted,
        ),
        (
            serde_json::json!({"creditUsagePercent": 25}),
            Some(25.0),
            Allowed,
        ),
        (
            serde_json::json!({"onDemandCap": {"val": 500}, "onDemandUsed": {"val": 100}}),
            None,
            Allowed,
        ),
        (
            serde_json::json!({
                "creditUsagePercent": 100,
                "onDemandCap": {"val": 500}, "onDemandUsed": {"val": 100}
            }),
            Some(100.0),
            Allowed,
        ),
    ] {
        let (store, repository) = repository_with_accounts(&[("percent", "percent")]).await;
        let id = account_id("percent");
        store
            .apply_quota_access(QuotaAccessChange {
                account_id: id.clone(),
                expected_revision: CredentialRevision::new(1).expect("revision"),
                state: QuotaState::exhausted(
                    QuotaEvidence::ProviderDenied,
                    SystemTime::now(),
                    None,
                ),
            })
            .await
            .expect("mark quota exhausted");
        let body = serde_json::to_vec(&serde_json::json!({"config": config})).expect("billing");
        let service =
            crate::support::grok_quota_service(repository, QueueBillingTransport::success(&body));
        let snapshot = service.refresh_account(&id).await.expect("refresh quota");
        let account = store.account(&id).expect("refreshed account");

        assert_eq!(snapshot.billing().used_percent(), expected_percent);
        assert!(
            snapshot.billing().has_authoritative_quota(),
            "config: {config}"
        );
        assert_eq!(
            account.quota().access(),
            expected_access,
            "config: {config}"
        );
        assert_eq!(
            service
                .scheduling_signals(&account)
                .and_then(|signals| signals.remaining_rank()),
            expected_percent.map(|percent| ((100.0 - percent) * 100.0).round() as u64)
        );
    }
}

#[tokio::test]
async fn signed_prepaid_balance_preserves_wire_and_recovers_access_when_current() {
    use gateway_core::account::QuotaAccessState::{Allowed, Exhausted};

    for (balance, period_end, expected_access) in [
        (-500, None, Allowed),
        (i64::MIN, None, Allowed),
        (0, None, Exhausted),
        (500, None, Allowed),
        (-500, Some("2000-01-01T00:00:00Z"), Exhausted),
        (-500, Some("2099-01-01T00:00:00Z"), Allowed),
    ] {
        let (store, repository) = repository_with_accounts(&[("signed", "signed")]).await;
        let id = account_id("signed");
        store
            .apply_quota_access(QuotaAccessChange {
                account_id: id.clone(),
                expected_revision: CredentialRevision::new(1).expect("revision"),
                state: QuotaState::exhausted(
                    QuotaEvidence::ProviderDenied,
                    SystemTime::now(),
                    None,
                ),
            })
            .await
            .expect("mark quota exhausted");
        let body = serde_json::to_vec(&serde_json::json!({
            "config": {
                "prepaidBalance": {"val": balance}, "currentPeriod": {"end": period_end}
            }
        }))
        .expect("billing");
        let transport = QueueBillingTransport::success(&body);
        let service = crate::support::grok_quota_service(repository, transport.clone());

        let snapshot = service
            .refresh_account(&id)
            .await
            .expect("refresh signed balance");
        let cached = service
            .read_account(&id)
            .await
            .expect("read quota")
            .expect("cached quota");
        let persisted = store
            .get_quotas(std::slice::from_ref(&id))
            .await
            .expect("stored quota");

        assert_eq!(snapshot.billing().prepaid_balance_cents(), Some(balance));
        assert_eq!(snapshot.billing().has_authoritative_quota(), balance != 0);
        assert_eq!(cached.billing().prepaid_balance_cents(), Some(balance));
        assert_eq!(
            persisted[0].quota.expose_to_provider()["config"]["prepaidBalance"]["val"],
            balance
        );
        assert_eq!(
            store.account(&id).expect("account").quota().access(),
            expected_access,
            "balance: {balance}, period_end: {period_end:?}"
        );
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn authoritative_billing_refresh_clears_existing_quota_exhaustion() {
    let (store, repository) =
        repository_with_accounts(&[("recovered-quota", "recovered-quota")]).await;
    let id = account_id("recovered-quota");
    store
        .apply_quota_access(QuotaAccessChange {
            account_id: id.clone(),
            expected_revision: CredentialRevision::new(1).expect("revision"),
            state: QuotaState::exhausted(QuotaEvidence::ProviderDenied, SystemTime::now(), None),
        })
        .await
        .expect("mark quota exhausted");

    crate::support::grok_quota_service(
        repository,
        QueueBillingTransport::success(
            br#"{"config":{"creditUsagePercent":12.5,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-07-15T00:00:00Z","end":"2099-07-22T00:00:00Z"}}}"#,
        ),
    )
    .refresh_account(&id)
    .await
    .expect("refresh recovered quota");

    assert_eq!(
        store.account(&id).expect("account").quota().access(),
        gateway_core::account::QuotaAccessState::Allowed
    );
}

#[tokio::test]
async fn full_usage_display_does_not_invent_quota_exhaustion() {
    let (store, repository) =
        repository_with_accounts(&[("exhausted-quota", "exhausted-quota")]).await;
    let id = account_id("exhausted-quota");

    crate::support::grok_quota_service(
        repository,
        QueueBillingTransport::success(
            br#"{"config":{"creditUsagePercent":100,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-07-15T00:00:00Z","end":"2099-07-22T00:00:00Z"}}}"#,
        ),
    )
    .refresh_account(&id)
    .await
    .expect("refresh exhausted quota");

    let quota = store.account(&id).expect("account").quota();
    assert_eq!(
        quota.access(),
        gateway_core::account::QuotaAccessState::Unknown
    );
    assert!(quota.reset_at().is_none());
}

#[tokio::test]
async fn recovered_quota_does_not_clear_terminal_account_states() {
    for (suffix, credential_state) in [
        ("keep-banned", CredentialState::Banned),
        ("keep-expired", CredentialState::Expired),
        ("keep-invalid", CredentialState::Invalid),
    ] {
        let (store, repository) = repository_with_accounts(&[(suffix, suffix)]).await;
        let id = account_id(suffix);
        set_account_state(&store, &id, credential_state).await;
        crate::support::grok_quota_service(
            repository,
            QueueBillingTransport::success(br#"{"config":{"creditUsagePercent":10}}"#),
        )
        .refresh_account(&id)
        .await
        .expect("refresh terminal account quota");

        assert_eq!(
            store.account(&id).expect("account").credential_state(),
            credential_state
        );
    }
}

#[tokio::test]
async fn quota_refresh_preserves_ready_credential_state_across_concurrent_write() {
    let (store, repository) = repository_with_accounts(&[("new-cooldown", "new-cooldown")]).await;
    let id = account_id("new-cooldown");
    let transport = Arc::new(MutatingBillingTransport {
        store: Arc::clone(&store),
        mutation: Mutex::new(Some(BillingMutation::State(AccountStateChange {
            message: None,
            account_id: id.clone(),
            expected_revision: CredentialRevision::new(1).expect("revision"),
            credential_state: CredentialState::Ready,
            error_reason: None,
            observed_at: SystemTime::now(),
        }))),
        body: br#"{"config":{"creditUsagePercent":10}}"#.to_vec(),
    });

    crate::support::grok_quota_service(repository, transport)
        .refresh_account(&id)
        .await
        .expect("refresh around concurrent state write");

    let account = store.account(&id).expect("account");
    assert_eq!(account.credential_state(), CredentialState::Ready);
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
async fn quota_period_dates_remain_absent_when_not_reported() {
    let (_, repository) = repository_with_accounts(&[("current-period", "current-period")]).await;
    let transport = QueueBillingTransport::success(
        br#"{"config":{"creditUsagePercent":25,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY"}}}"#,
    );
    let snapshot = crate::support::grok_quota_service(repository, transport)
        .refresh_account(&account_id("current-period"))
        .await
        .expect("current quota");

    assert_eq!(snapshot.billing().used_percent(), Some(25.0));
    assert_eq!(
        snapshot.billing().period_kind(),
        provider_xai::GrokQuotaPeriodKind::Weekly
    );
    assert_eq!(snapshot.billing().period_start(), None);
    assert_eq!(snapshot.billing().period_end(), None);
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
async fn expired_billing_window_does_not_participate_in_scheduling_rank() {
    let (store, repository) =
        repository_with_accounts(&[("expired-billing", "expired-billing")]).await;
    let id = account_id("expired-billing");
    let account = store.account(&id).expect("account");
    let service = crate::support::grok_quota_service(
        repository,
        QueueBillingTransport::success(
            br#"{"config":{"creditUsagePercent":25,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2000-01-01T00:00:00Z","end":"2000-01-08T00:00:00Z"}}}"#,
        ),
    );

    service
        .refresh_account(&id)
        .await
        .expect("refresh expired billing window");

    assert_eq!(service.scheduling_signals(&account), None);
    assert_eq!(
        store
            .account(&id)
            .expect("refreshed account")
            .quota()
            .access(),
        gateway_core::account::QuotaAccessState::Unknown
    );
}

#[tokio::test]
async fn non_authoritative_quota_refresh_preserves_exhaustion_without_scheduling_evidence() {
    use provider_xai::GrokQuotaPeriodKind::{Other, Weekly};

    for (config, expected_period) in [
        (
            serde_json::json!({"future_quota": {"remaining": 100}}),
            Other,
        ),
        (
            serde_json::json!({
                "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY"},
                "onDemandCap": {"val": 0},
                "onDemandUsed": {"val": 0},
                "prepaidBalance": {"val": 0}
            }),
            Weekly,
        ),
    ] {
        let (store, repository) =
            repository_with_accounts(&[("free-quota", "subject-free-quota")]).await;
        let id = account_id("free-quota");
        store
            .apply_quota_access(QuotaAccessChange {
                account_id: id.clone(),
                expected_revision: CredentialRevision::new(1).expect("revision"),
                state: QuotaState::exhausted(
                    QuotaEvidence::ProviderDenied,
                    SystemTime::now(),
                    None,
                ),
            })
            .await
            .expect("mark quota exhausted");
        let body = serde_json::to_vec(&serde_json::json!({"config": config})).expect("billing");
        let service =
            crate::support::grok_quota_service(repository, QueueBillingTransport::success(&body));
        let snapshot = service
            .refresh_account(&id)
            .await
            .expect("refresh Free quota");
        let account = store.account(&id).expect("account");

        assert!(!snapshot.billing().has_authoritative_quota());
        assert_eq!(snapshot.billing().used_percent(), None);
        assert_eq!(snapshot.billing().period_kind(), expected_period);
        assert_eq!(snapshot.billing().period_start(), None);
        assert_eq!(snapshot.billing().period_end(), None);
        assert_eq!(service.scheduling_signals(&account), None);
        assert_eq!(
            account.quota().access(),
            gateway_core::account::QuotaAccessState::Exhausted
        );
    }
}

#[tokio::test]
async fn quota_read_rejects_corrupt_provider_document() {
    let (store, repository) = repository_with_accounts(&[("corrupt", "subject-corrupt")]).await;
    let mut document = serde_json::Map::new();
    document.insert("config".to_owned(), serde_json::json!([]));
    store
        .compare_and_swap_quota(QuotaObservation {
            plan_type: None,
            account_id: account_id("corrupt"),
            expected_revision: CredentialRevision::new(1).expect("revision"),
            quota: OpaqueProviderData::new(document),
            observed_at: SystemTime::now(),
            state: QuotaState::unknown(),
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
        .quota;
    assert_eq!(
        persisted.expose_to_provider()["config"]["creditUsagePercent"].as_f64(),
        Some(10.0),
    );
}
