//! 公共互联按 Provider 声明发现连接，不将认证种类或调度观测绑在 OpenAI 上

mod refresh;

use super::super::UnavailableProvider;
use async_trait::async_trait;
use chrono::Utc;
use gateway_admin::{
    initialize_peer_upstream,
    ports::{
        peer::{PeerConnection, PeerFetchFailure, PeerTransport},
        provider::{ProviderAdmin, ProviderAdminRegistry},
    },
};
use gateway_core::{
    account::*,
    error::{StoreError, StoreErrorKind},
    lifecycle::CancellationToken,
    routing::ProviderKind,
    task::{WorkerContribution, WorkerCycleContext, WorkerRunnable},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

#[tokio::test]
async fn manual_and_periodic_refreshes_share_capacity_and_release_it_after_failure_or_cancellation()
{
    let providers = ["openai", "xai"].map(|kind| {
        let mut provider = UnavailableProvider::new(kind);
        provider.peer_enabled = true;
        Arc::new(provider) as Arc<dyn ProviderAdmin>
    });
    let registry = ProviderAdminRegistry::new(providers).unwrap();
    let records = (0..100)
        .map(|index| {
            account(
                &format!("acct_{index}"),
                if index % 2 == 0 { "openai" } else { "xai" },
                "peer_test",
            )
        })
        .collect::<Vec<_>>();
    let accounts = Arc::new(TestAccounts {
        accounts: Mutex::new(records.clone()),
        records: Mutex::default(),
        failed_provider: None,
    });
    let transport = Arc::new(GatedTransport::new());
    let mut bundle = initialize_peer_upstream(accounts, &registry, transport.clone()).unwrap();
    let service = bundle.service();
    let mut requests = Vec::new();
    for record in &records[..20] {
        let service = service.clone();
        let id = record.id().clone();
        requests.push(tokio::spawn(
            async move { service.refresh_quota(&id).await },
        ));
    }
    transport.wait_for_requests(4).await;
    let WorkerContribution::Registration(worker) = bundle.take_worker_contributions().remove(0)
    else {
        panic!("scheduled worker")
    };
    let worker = tokio::spawn(async move {
        let WorkerRunnable::Scheduled { task, .. } = worker.runnable else {
            panic!("scheduled worker")
        };
        task.run_cycle(WorkerCycleContext::new(
            worker.id,
            None,
            CancellationToken::new(),
        ))
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), transport.wait_for_requests(5))
            .await
            .is_err()
    );

    requests.remove(0).abort();
    transport.wait_for_requests(5).await;
    transport.gate.add_permits(100);
    tokio::time::timeout(Duration::from_secs(5), async {
        for (index, request) in requests.into_iter().enumerate() {
            let result = request.await.unwrap();
            assert_eq!(result.is_err(), index == 0);
        }
        worker.await.unwrap().unwrap();
    })
    .await
    .expect("all queued refreshes complete");
    assert_eq!(transport.peak.load(Ordering::SeqCst), 4);
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
    assert_eq!(transport.started_count.load(Ordering::SeqCst), 100);
}

#[tokio::test]
async fn queued_refresh_reads_current_account_after_capacity_becomes_available() {
    let mut provider = UnavailableProvider::new("openai");
    provider.peer_enabled = true;
    let registry =
        ProviderAdminRegistry::new([Arc::new(provider) as Arc<dyn ProviderAdmin>]).unwrap();
    let records = (0..5)
        .map(|index| account(&format!("acct_{index}"), "openai", "peer_test"))
        .collect::<Vec<_>>();
    let accounts = Arc::new(TestAccounts {
        accounts: Mutex::new(records.clone()),
        records: Mutex::default(),
        failed_provider: None,
    });
    let transport = Arc::new(GatedTransport::new());
    let bundle = initialize_peer_upstream(accounts.clone(), &registry, transport.clone()).unwrap();
    let service = bundle.service();
    let mut requests = Vec::new();
    for record in &records[..4] {
        let service = service.clone();
        let id = record.id().clone();
        requests.push(tokio::spawn(
            async move { service.refresh_quota(&id).await },
        ));
    }
    transport.wait_for_requests(4).await;
    let queued = tokio::spawn(async move { service.refresh_quota(records[4].id()).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), transport.wait_for_requests(5))
            .await
            .is_err()
    );
    accounts
        .accounts
        .lock()
        .unwrap()
        .retain(|account| account.id().as_str() != "acct_4");
    transport.gate.add_permits(4);
    for request in requests {
        let _result = request.await.unwrap();
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(1), queued)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(transport.started_count.load(Ordering::SeqCst), 4);
}

struct GatedTransport {
    gate: tokio::sync::Semaphore,
    started: tokio::sync::Notify,
    started_count: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    success: TestTransport,
}

impl GatedTransport {
    fn new() -> Self {
        Self {
            gate: tokio::sync::Semaphore::new(0),
            started: tokio::sync::Notify::new(),
            started_count: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            success: TestTransport(Mutex::default()),
        }
    }

    async fn wait_for_requests(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let notified = self.started.notified();
                if self.started_count.load(Ordering::SeqCst) >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("refresh reached transport");
    }
}

struct ActiveFetch<'a>(&'a AtomicUsize);

impl Drop for ActiveFetch<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl PeerTransport for GatedTransport {
    async fn fetch(
        &self,
        account: &ProviderAccount,
        connection: &PeerConnection,
    ) -> Result<Vec<u8>, PeerFetchFailure> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let _active = ActiveFetch(&self.active);
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.started_count.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.gate.acquire().await.unwrap().forget();
        if account.id().as_str() == "acct_1" {
            return Err(UpstreamInfoFailure::Unavailable.into());
        }
        self.success.fetch(account, connection).await
    }
}

#[tokio::test]
async fn discovery_uses_provider_capabilities_and_publishes_neutral_signals() {
    for failed_provider in [None, Some("openai")] {
        let providers = ["openai", "xai"].map(|kind| {
            let mut provider = UnavailableProvider::new(kind);
            provider.peer_enabled = true;
            Arc::new(provider) as Arc<dyn ProviderAdmin>
        });
        let registry = ProviderAdminRegistry::new(providers).unwrap();
        let accounts = Arc::new(TestAccounts {
            accounts: Mutex::new(vec![
                account("acct_first", "openai", "peer_test"),
                account("acct_second", "xai", "peer_test"),
                account("acct_native", "xai", "oauth"),
            ]),
            records: Mutex::default(),
            failed_provider,
        });
        let transport = Arc::new(TestTransport(Mutex::new(Vec::new())));
        let mut bundle =
            initialize_peer_upstream(accounts.clone(), &registry, transport.clone()).unwrap();
        let initial_accounts = accounts.accounts.lock().unwrap().clone();
        let reader = UpstreamInfoReader::default();
        assert!(
            reader
                .signal(&initial_accounts[0], Some("model"), SystemTime::now())
                .is_none()
        );
        let source = bundle.scheduling_source();
        reader.bind(&source).unwrap();
        assert!(reader.bind(&source).is_err());
        drop(source);
        let WorkerContribution::Registration(worker) = bundle.take_worker_contributions().remove(0)
        else {
            panic!("scheduled worker")
        };
        let WorkerRunnable::Scheduled { ref task, .. } = worker.runnable else {
            panic!("scheduled worker")
        };
        let result = task
            .run_cycle(WorkerCycleContext::new(
                worker.id.clone(),
                None,
                CancellationToken::new(),
            ))
            .await;
        assert_eq!(result.is_err(), failed_provider.is_some());
        assert_eq!(
            *transport.0.lock().unwrap(),
            if failed_provider.is_some() {
                vec!["acct_second"]
            } else {
                vec!["acct_first", "acct_second"]
            }
        );
        for account in &initial_accounts[..2] {
            if Some(account.provider().as_str()) == failed_provider {
                assert!(
                    reader
                        .signal(account, Some("model"), SystemTime::now())
                        .is_none()
                );
                continue;
            }
            let observed = bundle
                .service()
                .read_quota(account.id())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                observed.upstream.unwrap().connection_revision,
                UpstreamConnection::for_account(account).revision
            );
            assert!(
                reader
                    .signal(account, Some("model"), SystemTime::now())
                    .unwrap()
                    .exhausted
            );
        }
        assert!(
            reader
                .signal(&initial_accounts[2], Some("model"), SystemTime::now())
                .is_none()
        );
        drop(worker);
        drop(bundle);
        assert!(
            reader
                .signal(&initial_accounts[0], Some("model"), SystemTime::now())
                .is_none()
        );
    }
}

#[tokio::test]
async fn account_edits_preserve_observation_and_refresh_budget_until_connection_changes() {
    let mut provider = UnavailableProvider::new("openai");
    provider.peer_enabled = true;
    let registry =
        ProviderAdminRegistry::new([Arc::new(provider) as Arc<dyn ProviderAdmin>]).unwrap();
    let initial = account("acct_peer", "openai", "peer_test");
    let accounts = Arc::new(TestAccounts {
        accounts: Mutex::new(vec![initial.clone()]),
        records: Mutex::default(),
        failed_provider: None,
    });
    let transport = Arc::new(TestTransport(Mutex::new(Vec::new())));
    let bundle = initialize_peer_upstream(accounts.clone(), &registry, transport.clone()).unwrap();
    let service = bundle.service();
    service.refresh_quota(initial.id()).await.unwrap();
    let before = service.read_summary(initial.id()).await.unwrap().unwrap();

    let edited = initial.clone().with_notes(Some("仅修改备注".into()));
    *accounts.accounts.lock().unwrap() = vec![edited.clone()];
    service.account_changed(edited.id()).await.unwrap();
    assert!(
        bundle
            .scheduling_source()
            .signal(&edited, Some("model"), SystemTime::now())
            .is_some()
    );
    let after = service.refresh_quota(edited.id()).await.unwrap();
    assert_eq!(after.upstream, before.upstream);
    assert_eq!(transport.0.lock().unwrap().len(), 1);

    let reconnected = edited.with_outbound_proxy(Some(
        OutboundProxy::parse("http://proxy.invalid:8080").unwrap(),
    ));
    *accounts.accounts.lock().unwrap() = vec![reconnected.clone()];
    service.account_changed(reconnected.id()).await.unwrap();
    assert!(
        bundle
            .scheduling_source()
            .signal(&reconnected, Some("model"), SystemTime::now())
            .is_none()
    );
    service.refresh_quota(reconnected.id()).await.unwrap();
    assert_eq!(transport.0.lock().unwrap().len(), 2);
}

fn account(id: &str, provider: &str, authentication: &str) -> ProviderAccount {
    ProviderAccount::new(
        ProviderAccountId::new(id).unwrap(),
        ProviderKind::new(provider).unwrap(),
        id.to_owned(),
        None,
        authentication.to_owned(),
        CredentialRevision::new(1).unwrap(),
        None,
    )
    .with_account_facts(
        true,
        CredentialState::Ready,
        QuotaState::unknown(),
        None,
        None,
    )
}

struct TestTransport(Mutex<Vec<String>>);

#[async_trait]
impl PeerTransport for TestTransport {
    async fn fetch(
        &self,
        account: &ProviderAccount,
        connection: &PeerConnection,
    ) -> Result<Vec<u8>, PeerFetchFailure> {
        assert_eq!(
            connection.base_url,
            format!("https://{}.invalid/v1", account.provider())
        );
        self.0
            .lock()
            .unwrap()
            .push(account.id().as_str().to_owned());
        let now = Utc::now();
        Ok(serde_json::to_vec(&serde_json::json!({
            "object":"cpr.peer", "schema_version":3,
            "observed_at":now.to_rfc3339(),
            "expires_at":(now + chrono::Duration::minutes(5)).to_rfc3339(),
            "capabilities":["key_usage"],
            "usage":{"status":"available","observed_at":now.to_rfc3339(),"data":{"unit":"USD","allowed":false,
                "windows":[{"key":"daily","total":"1","used":"1","remaining":"0","reset_at":(now + chrono::Duration::days(1)).to_rfc3339()}]}},
            "models":[]
        })).unwrap())
    }
}

struct TestAccounts {
    accounts: Mutex<Vec<ProviderAccount>>,
    records: Mutex<BTreeMap<ProviderAccountId, QuotaObservation>>,
    failed_provider: Option<&'static str>,
}

fn loaded(account: &ProviderAccount) -> LoadedCredential {
    LoadedCredential {
        account: account.clone(),
        credential: PlaintextCredential::new(Default::default()),
    }
}

#[async_trait]
impl ProviderAccountStore for TestAccounts {
    async fn load_account_catalogs(
        &self,
        _: &gateway_core::routing::ProviderKind,
    ) -> Result<Vec<gateway_core::account::StoredAccountCatalog>, gateway_core::error::StoreError>
    {
        Ok(Vec::new())
    }
    async fn save_account_catalog(
        &self,
        _: &gateway_core::account::AccountCatalogObservation,
        _: &[u8],
    ) -> Result<bool, gateway_core::error::StoreError> {
        unreachable!("fixture does not discover account catalogs")
    }

    async fn create_account(&self, _account: NewProviderAccount) -> Result<(), StoreError> {
        panic!("unused account operation")
    }
    async fn get_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StoreError> {
        Ok(self
            .accounts
            .lock()
            .unwrap()
            .iter()
            .find(|account| account.id() == account_id)
            .cloned())
    }
    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, StoreError> {
        panic!("unused account operation")
    }
    async fn list_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        if Some(provider.as_str()) == self.failed_provider {
            return Err(StoreError::new(StoreErrorKind::Unavailable));
        }
        Ok(self
            .accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|account| account.provider() == provider)
            .cloned()
            .collect())
    }

    async fn list_for_provider_accounts(
        &self,
        provider: &ProviderKind,
        account_ids: &[ProviderAccountId],
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        if account_ids.is_empty() {
            return Ok(Vec::new());
        }
        if Some(provider.as_str()) == self.failed_provider {
            return Err(StoreError::new(StoreErrorKind::Unavailable));
        }
        Ok(self
            .accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|account| {
                account.provider() == provider
                    && account.enabled()
                    && account_ids.contains(account.id())
            })
            .cloned()
            .collect())
    }
    async fn list_refresh_candidates(
        &self,
        _query: ProviderRefreshQuery,
    ) -> Result<Vec<LoadedCredential>, StoreError> {
        panic!("unused account operation")
    }
    async fn load_credential(
        &self,
        _account: &ProviderAccountId,
        _expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, StoreError> {
        panic!("unused account operation")
    }
    async fn load_current_credential(
        &self,
        id: &ProviderAccountId,
    ) -> Result<LoadedCredential, StoreError> {
        self.accounts
            .lock()
            .unwrap()
            .iter()
            .find(|account| account.id() == id)
            .map(loaded)
            .ok_or_else(|| StoreError::new(StoreErrorKind::InvalidData))
    }
    async fn compare_and_swap_credential(
        &self,
        _update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, StoreError> {
        panic!("unused account operation")
    }
    async fn get_quotas(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, StoreError> {
        let stored = self.records.lock().unwrap();
        Ok(accounts
            .iter()
            .filter_map(|id| stored.get(id).cloned())
            .collect())
    }
    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        self.records
            .lock()
            .unwrap()
            .insert(observation.account_id.clone(), observation);
        Ok(QuotaWriteOutcome::Updated)
    }
    async fn touch_quota_observation(
        &self,
        _touch: QuotaObservationTouch,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        panic!("unused account operation")
    }
    async fn apply_quota_access(
        &self,
        _change: QuotaAccessChange,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        panic!("unused account operation")
    }
    async fn apply_state_change(&self, _change: AccountStateChange) -> Result<(), StoreError> {
        panic!("unused account operation")
    }
    async fn update_account(&self, _update: ProviderAccountUpdate) -> Result<(), StoreError> {
        panic!("unused account operation")
    }
    async fn set_enabled(
        &self,
        _account: &ProviderAccountId,
        _enabled: bool,
    ) -> Result<(), StoreError> {
        panic!("unused account operation")
    }
    async fn delete_account(&self, _account: &ProviderAccountId) -> Result<(), StoreError> {
        panic!("unused account operation")
    }
}
