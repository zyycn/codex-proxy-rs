//! 验证 installation id 的启用条件、持久复用、并发初始化与失败取消恢复

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
use gateway_core::account::{AccountFeedbackStats, OpaqueProviderData, ProviderAccountId};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, ModelRequestId, RequestAttemptContext,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    ProviderClientInstallationKey, ProviderClientInstallationPort, ProviderLeasePort,
    ProviderStoreError, ProviderStoreErrorKind,
};
use gateway_core::routing::{
    ClientRoutingScope, FrozenAccountScope, ProviderKind, RuntimeAccount, RuntimeAccountDirectory,
};
use provider_openai::OFFICIAL_CODEX_BASE_URL;
use provider_openai::credential::{
    CodexCookiePolicy, CodexCredentialLease, CodexCredentialQuotaService, CodexCredentialSelector,
    CredentialSelectionError, ImportCodexOAuthCredential, SelectCodexCredential,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use tokio::sync::{Notify, Semaphore};
use url::Url;
use uuid::Uuid;

use crate::support::{
    MemoryAccountStore, MemoryClientInstallations, MemoryCooldownPort, MemorySessionAffinity,
    MemorySessionExclusions, TestLeaseCoordinator, account_policy, profile, secret,
};

struct Fixture {
    accounts: Arc<MemoryAccountStore>,
    leases: Arc<TestLeaseCoordinator>,
    installations: Arc<ControlledInstallations>,
}

impl Fixture {
    async fn new() -> Self {
        let accounts = Arc::new(MemoryAccountStore::default());
        for id in ["acct_first", "acct_second"] {
            accounts
                .seed_oauth_credential(ImportCodexOAuthCredential {
                    account_id: id.to_owned(),
                    name: id.to_owned(),
                    secret: secret("installation-test"),
                    verified_account: profile(id),
                    next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
                    enabled: true,
                })
                .await;
        }
        Self {
            accounts,
            leases: Arc::default(),
            installations: Arc::default(),
        }
    }

    fn selector(&self) -> Arc<CodexCredentialSelector> {
        let leases = self.leases.clone() as Arc<dyn ProviderLeasePort>;
        let quota = Arc::new(CodexCredentialQuotaService::new(
            self.accounts.repository(),
            CodexWireProfileState::new(CodexWireProfile::default()),
            reqwest::Client::new(),
            OFFICIAL_CODEX_BASE_URL.to_owned(),
            Arc::new(MemoryCooldownPort::new()),
            Arc::clone(&leases),
            crate::support::runtime_policy(),
        ));
        Arc::new(CodexCredentialSelector::new(
            ProviderKind::new("openai").unwrap(),
            self.accounts.repository(),
            self.installations.clone(),
            leases,
            Arc::new(MemorySessionAffinity::default()),
            Arc::new(MemorySessionExclusions::default()),
            quota,
            Arc::new(AccountFeedbackStats::default()),
            CodexCookiePolicy::official().unwrap(),
        ))
    }
}

#[derive(Default)]
struct ControlledInstallations {
    persisted: MemoryClientInstallations,
    fail_read: AtomicBool,
    fail_after_commit: AtomicBool,
    read_gate: Mutex<Option<Arc<Semaphore>>>,
    read_started: AtomicUsize,
    changed: Notify,
}

impl ControlledInstallations {
    async fn wait_for_reads(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.read_started.load(Ordering::SeqCst) >= count {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("installation read started");
    }
}

impl ProviderClientInstallationPort for ControlledInstallations {
    fn read<'a>(
        &'a self,
        key: &'a ProviderClientInstallationKey,
    ) -> BoxFuture<'a, Result<Option<Uuid>, ProviderStoreError>> {
        Box::pin(async move {
            self.read_started.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_waiters();
            let gate = self.read_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            if self.fail_read.load(Ordering::SeqCst) {
                return Err(ProviderStoreError::new(
                    ProviderStoreErrorKind::Unavailable,
                    "test_read",
                ));
            }
            self.persisted.read(key).await
        })
    }

    fn get_or_insert<'a>(
        &'a self,
        key: &'a ProviderClientInstallationKey,
        candidate: Uuid,
    ) -> BoxFuture<'a, Result<Uuid, ProviderStoreError>> {
        Box::pin(async move {
            let winner = self.persisted.get_or_insert(key, candidate).await?;
            if self.fail_after_commit.swap(false, Ordering::SeqCst) {
                return Err(ProviderStoreError::new(
                    ProviderStoreErrorKind::Unavailable,
                    "test_commit_reply",
                ));
            }
            Ok(winner)
        })
    }
}

fn attempt(key: &str, account: &str, independent: bool, enabled: bool, ua: &str) -> AttemptContext {
    let account_id = ProviderAccountId::new(account).unwrap();
    let provider = ProviderKind::new("openai").unwrap();
    let mut profile = serde_json::to_value(CodexWireProfile {
        exact_user_agent: Some(ua.to_owned()),
        ..Default::default()
    })
    .unwrap();
    profile["independent_ua_configuration"] = independent.into();
    profile["per_key_installation_id_enabled"] = enabled.into();
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_installation").unwrap(),
            ClientApiKeyId::new(key).unwrap(),
        )
        .with_request_profile(Some(OpaqueProviderData::new(
            profile.as_object().unwrap().clone(),
        ))),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(60),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), Some(account_id.clone()), None)
            .with_account_scope(Arc::new(FrozenAccountScope::new(
                Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
                    account_id,
                    RuntimeAccount::new(provider, BTreeSet::new()),
                )]))),
                ClientRoutingScope::all_accounts(),
            ))),
        None,
        CancellationToken::new(),
    )
}

async fn select(
    selector: &CodexCredentialSelector,
    attempt: &AttemptContext,
) -> Result<CodexCredentialLease, CredentialSelectionError> {
    selector
        .select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap(),
            attempt,
            session_affinity_key: None,
        })
        .await
}

#[tokio::test]
async fn account_mode_bypasses_existing_mapping_and_store_failures() {
    let fixture = Fixture::new().await;
    let selector = fixture.selector();
    fixture
        .installations
        .fail_read
        .store(true, Ordering::SeqCst);
    let persisted = Uuid::new_v4();
    fixture
        .installations
        .persisted
        .mappings
        .lock()
        .unwrap()
        .insert(
            ProviderClientInstallationKey::new(
                ClientApiKeyId::new("key_first").unwrap(),
                ProviderAccountId::new("acct_first").unwrap(),
            ),
            persisted,
        );
    let mut account_identity = None;
    for (independent, enabled) in [(false, false), (false, true), (true, false)] {
        let lease = select(
            &selector,
            &attempt("key_first", "acct_first", independent, enabled, "same-UA"),
        )
        .await
        .unwrap();
        assert_ne!(lease.installation_id(), persisted.to_string());
        if let Some(identity) = &account_identity {
            assert_eq!(lease.installation_id(), identity);
        } else {
            account_identity = Some(lease.installation_id().to_owned());
        }
    }
    assert_eq!(fixture.installations.read_started.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture
            .installations
            .persisted
            .inserts
            .load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn pairs_stay_stable_across_ua_changes_toggles_and_restart() {
    let fixture = Fixture::new().await;
    let selector = fixture.selector();
    let mut identities = BTreeSet::new();
    for key in ["key_first", "key_second"] {
        for account in ["acct_first", "acct_second"] {
            let lease = select(&selector, &attempt(key, account, true, true, "same-UA"))
                .await
                .unwrap();
            let id = Uuid::parse_str(lease.installation_id()).unwrap();
            assert_eq!(id.get_version(), Some(uuid::Version::Random));
            assert!(identities.insert(id));
        }
    }
    let original = select(
        &selector,
        &attempt("key_first", "acct_first", true, true, "same-UA"),
    )
    .await
    .unwrap()
    .installation_id()
    .to_owned();
    for (independent, enabled) in [(true, false), (false, false), (true, false)] {
        let lease = select(
            &selector,
            &attempt("key_first", "acct_first", independent, enabled, "new-UA"),
        )
        .await
        .unwrap();
        assert_ne!(lease.installation_id(), original);
    }
    assert_eq!(fixture.installations.read_started.load(Ordering::SeqCst), 4);
    assert_eq!(
        select(
            &selector,
            &attempt("key_first", "acct_first", true, true, "new-UA")
        )
        .await
        .unwrap()
        .installation_id(),
        original
    );
    drop(selector);
    assert_eq!(
        select(
            &fixture.selector(),
            &attempt("key_first", "acct_first", true, true, "new-UA")
        )
        .await
        .unwrap()
        .installation_id(),
        original
    );
    assert_eq!(
        fixture
            .installations
            .persisted
            .inserts
            .load(Ordering::SeqCst),
        4
    );
    assert_eq!(fixture.installations.read_started.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn concurrent_requests_share_initialization_and_persisted_winner() {
    let fixture = Fixture::new().await;
    let selector = fixture.selector();
    let gate = Arc::new(Semaphore::new(0));
    *fixture.installations.read_gate.lock().unwrap() = Some(gate.clone());
    let mut requests = Vec::new();
    for _ in 0..32 {
        let selector = selector.clone();
        requests.push(tokio::spawn(async move {
            select(
                &selector,
                &attempt("key_first", "acct_first", true, true, "same-UA"),
            )
            .await
            .unwrap()
            .installation_id()
            .to_owned()
        }));
    }
    fixture.installations.wait_for_reads(1).await;
    tokio::task::yield_now().await;
    assert_eq!(fixture.installations.read_started.load(Ordering::SeqCst), 1);
    gate.add_permits(1);
    let mut results = BTreeSet::new();
    for request in requests {
        results.insert(request.await.unwrap());
    }
    assert_eq!(results.len(), 1);
    assert_eq!(
        fixture
            .installations
            .persisted
            .inserts
            .load(Ordering::SeqCst),
        1
    );
    assert_eq!(fixture.leases.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn store_failures_release_scheduling_lease_and_retry_committed_mapping() {
    let fixture = Fixture::new().await;
    let selector = fixture.selector();
    let request = attempt("key_first", "acct_first", true, true, "same-UA");
    fixture
        .installations
        .fail_read
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        select(&selector, &request).await,
        Err(CredentialSelectionError::Store(_))
    ));
    assert_eq!(fixture.leases.active.load(Ordering::SeqCst), 0);
    fixture
        .installations
        .fail_read
        .store(false, Ordering::SeqCst);
    fixture
        .installations
        .fail_after_commit
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        select(&selector, &request).await,
        Err(CredentialSelectionError::Store(_))
    ));
    assert_eq!(fixture.leases.active.load(Ordering::SeqCst), 0);
    let persisted = *fixture
        .installations
        .persisted
        .mappings
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(
        select(&selector, &request).await.unwrap().installation_id(),
        persisted.to_string()
    );
    assert_eq!(fixture.installations.read_started.load(Ordering::SeqCst), 3);
    assert_eq!(
        fixture
            .installations
            .persisted
            .inserts
            .load(Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn cancelled_initializer_releases_guard_and_waiter_retries_same_pair() {
    let fixture = Fixture::new().await;
    let selector = fixture.selector();
    let gate = Arc::new(Semaphore::new(0));
    *fixture.installations.read_gate.lock().unwrap() = Some(gate.clone());
    let cancelled = attempt("key_first", "acct_first", true, true, "same-UA");
    let cancellation = cancelled.cancellation().clone();
    let leader = tokio::spawn({
        let selector = selector.clone();
        async move { select(&selector, &cancelled).await }
    });
    fixture.installations.wait_for_reads(1).await;
    let follower = tokio::spawn({
        let selector = selector.clone();
        async move {
            select(
                &selector,
                &attempt("key_first", "acct_first", true, true, "same-UA"),
            )
            .await
        }
    });
    tokio::task::yield_now().await;
    cancellation.cancel();
    assert!(matches!(
        leader.await.unwrap(),
        Err(CredentialSelectionError::Cancelled)
    ));
    fixture.installations.wait_for_reads(2).await;
    assert_eq!(
        fixture
            .installations
            .persisted
            .inserts
            .load(Ordering::SeqCst),
        0
    );
    gate.add_permits(1);
    let identity = follower
        .await
        .unwrap()
        .unwrap()
        .installation_id()
        .to_owned();
    assert_eq!(fixture.leases.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        select(
            &selector,
            &attempt("key_first", "acct_first", true, true, "same-UA")
        )
        .await
        .unwrap()
        .installation_id(),
        identity
    );
    assert_eq!(fixture.installations.read_started.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn eviction_reloads_persisted_identities_without_generating_new_values() {
    let fixture = Fixture::new().await;
    let selector = fixture.selector();
    let mut identities = Vec::new();
    // 超过生产缓存容量，通过真正的选号入口验证驱逐后的持久复用
    for index in 0..4097 {
        identities.push(
            select(
                &selector,
                &attempt(
                    &format!("key_eviction_{index}"),
                    "acct_first",
                    true,
                    true,
                    "same-UA",
                ),
            )
            .await
            .unwrap()
            .installation_id()
            .to_owned(),
        );
    }
    assert_eq!(
        fixture.installations.read_started.load(Ordering::SeqCst),
        4097
    );
    for (index, identity) in identities.iter().enumerate() {
        assert_eq!(
            select(
                &selector,
                &attempt(
                    &format!("key_eviction_{index}"),
                    "acct_first",
                    true,
                    true,
                    "same-UA"
                )
            )
            .await
            .unwrap()
            .installation_id(),
            identity
        );
    }
    assert!(fixture.installations.read_started.load(Ordering::SeqCst) > 4097);
    assert_eq!(
        fixture
            .installations
            .persisted
            .inserts
            .load(Ordering::SeqCst),
        4097
    );
}

#[tokio::test]
async fn full_cache_waits_for_capacity_without_evicting_inflight_initialization() {
    let fixture = Fixture::new().await;
    let selector = fixture.selector();
    let gate = Arc::new(Semaphore::new(0));
    *fixture.installations.read_gate.lock().unwrap() = Some(gate.clone());
    let mut requests = Vec::new();
    let mut cancel_second = None;
    for index in 0..4096 {
        let request = attempt(
            &format!("key_inflight_{index}"),
            "acct_first",
            true,
            true,
            "same-UA",
        );
        if index == 1 {
            cancel_second = Some(request.cancellation().clone());
        }
        let selector = selector.clone();
        requests.push(tokio::spawn(async move {
            select(&selector, &request)
                .await
                .map(|lease| lease.installation_id().to_owned())
        }));
    }
    fixture.installations.wait_for_reads(4096).await;
    let extra = tokio::spawn({
        let selector = selector.clone();
        async move {
            select(
                &selector,
                &attempt("key_inflight_extra", "acct_first", true, true, "same-UA"),
            )
            .await
        }
    });
    let same_pair = tokio::spawn({
        let selector = selector.clone();
        async move {
            select(
                &selector,
                &attempt("key_inflight_0", "acct_first", true, true, "same-UA"),
            )
            .await
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(
        fixture.installations.read_started.load(Ordering::SeqCst),
        4096
    );
    assert!(!extra.is_finished());
    cancel_second.unwrap().cancel();
    fixture.installations.wait_for_reads(4097).await;
    gate.add_permits(4097);
    let first = requests.remove(0).await.unwrap().unwrap();
    assert!(matches!(
        requests.remove(0).await.unwrap(),
        Err(CredentialSelectionError::Cancelled)
    ));
    for request in requests {
        request.await.unwrap().unwrap();
    }
    assert_eq!(same_pair.await.unwrap().unwrap().installation_id(), first);
    extra.await.unwrap().unwrap();
    assert_eq!(
        fixture.installations.read_started.load(Ordering::SeqCst),
        4097
    );
    assert_eq!(
        fixture
            .installations
            .persisted
            .inserts
            .load(Ordering::SeqCst),
        4096
    );
    assert_eq!(fixture.leases.active.load(Ordering::SeqCst), 0);
}
