//! 公共互联按 Provider 声明发现连接，不将认证种类或调度观测绑在 OpenAI 上

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
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

#[tokio::test]
async fn discovery_uses_provider_capabilities_and_publishes_neutral_signals() {
    for failed_provider in [None, Some("openai")] {
        let providers = ["openai", "xai"].map(|kind| {
            let mut provider = UnavailableProvider::new(kind);
            provider.peer_enabled = true;
            Arc::new(provider) as Arc<dyn ProviderAdmin>
        });
        let registry = ProviderAdminRegistry::new(providers).unwrap();
        let accounts = Arc::new(TestAccounts(vec![
            account("acct_first", "openai", "peer_test"),
            account("acct_second", "xai", "peer_test"),
            account("acct_native", "xai", "oauth"),
        ]));
        let store = Arc::new(TestPlans {
            accounts: accounts.clone(),
            records: Mutex::default(),
            failed_provider,
        });
        let transport = Arc::new(TestTransport(Mutex::new(Vec::new())));
        let mut bundle =
            initialize_peer_upstream(accounts.clone(), store, &registry, transport.clone())
                .unwrap();
        let reader = UpstreamInfoReader::default();
        assert!(
            reader
                .signal(&accounts.0[0], "model", SystemTime::now())
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
        for account in &accounts.0[..2] {
            if Some(account.provider().as_str()) == failed_provider {
                assert!(reader.signal(account, "model", SystemTime::now()).is_none());
                continue;
            }
            let observed = bundle
                .service()
                .read_quota(account.id())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                observed.upstream.unwrap().node.unwrap().name,
                account.provider().as_str()
            );
            assert!(
                reader
                    .signal(account, "model", SystemTime::now())
                    .unwrap()
                    .exhausted
            );
        }
        assert!(
            reader
                .signal(&accounts.0[2], "model", SystemTime::now())
                .is_none()
        );
        drop(worker);
        drop(bundle);
        assert!(
            reader
                .signal(&accounts.0[0], "model", SystemTime::now())
                .is_none()
        );
    }
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
            "object":"cpr.peer", "schema_version":1,
            "node":{"id":account.id().as_str(),"name":account.provider().as_str(),"software":"cpr"},
            "observed_at":now.to_rfc3339(),
            "expires_at":(now + chrono::Duration::minutes(5)).to_rfc3339(),
            "capabilities":["key_usage"],
            "usage":{"status":"available","observed_at":now.to_rfc3339(),"data":{"unit":"USD",
                "daily":{"total":"1","used":"1","remaining":"0","resetsAt":(now + chrono::Duration::days(1)).to_rfc3339()},
                "weekly":{"total":"0","used":"0","remaining":"0"}}},
            "pool":{"disclosure":"none"}, "quota":{"disclosure":"none"}, "models":[]
        })).unwrap())
    }
}

struct TestPlans {
    failed_provider: Option<&'static str>,
    accounts: Arc<TestAccounts>,
    records: Mutex<BTreeMap<ProviderAccountId, UpstreamInfoRecord>>,
}

#[async_trait]
impl UpstreamInfoStore for TestPlans {
    async fn list_candidates(
        &self,
        provider: &ProviderKind,
        kinds: &[&str],
        after: Option<&ProviderAccountId>,
        limit: u32,
    ) -> Result<Vec<LoadedCredential>, StoreError> {
        if Some(provider.as_str()) == self.failed_provider {
            return Err(StoreError::new(StoreErrorKind::Unavailable));
        }
        Ok(self
            .accounts
            .0
            .iter()
            .filter(|account| {
                account.provider() == provider
                    && kinds.contains(&account.authentication_kind())
                    && after.is_none_or(|id| account.id() > id)
            })
            .take(limit as usize)
            .map(loaded)
            .collect())
    }
    async fn reconcile(
        &self,
        connection: &UpstreamConnection,
        enabled: bool,
        now: SystemTime,
    ) -> Result<(), StoreError> {
        assert!(enabled);
        self.records
            .lock()
            .unwrap()
            .entry(connection.account_id.clone())
            .or_insert_with(|| UpstreamInfoRecord {
                plan: UpstreamInfoPlan {
                    connection: connection.clone(),
                    request_generation: 1,
                    completed_generation: 0,
                    next_attempt_at: now,
                    claimed_until: None,
                    last_attempt_at: None,
                    last_success_at: None,
                    last_error: None,
                    consecutive_failures: 0,
                },
                snapshot: None,
            });
        Ok(())
    }
    async fn request_refresh(
        &self,
        _: &UpstreamConnection,
        _: SystemTime,
        _: Duration,
    ) -> Result<UpstreamInfoPlan, StoreError> {
        panic!("automatic discovery does not enqueue a manual refresh")
    }
    async fn read_plans(
        &self,
        ids: &[ProviderAccountId],
    ) -> Result<Vec<UpstreamInfoPlan>, StoreError> {
        Ok(self
            .read(ids)
            .await?
            .into_iter()
            .map(|record| record.plan)
            .collect())
    }
    async fn read(&self, ids: &[ProviderAccountId]) -> Result<Vec<UpstreamInfoRecord>, StoreError> {
        let records = self.records.lock().unwrap();
        Ok(ids
            .iter()
            .filter_map(|id| records.get(id).cloned())
            .collect())
    }
    async fn claim_due(
        &self,
        provider: &ProviderKind,
        now: SystemTime,
        limit: u32,
        ttl: Duration,
    ) -> Result<Vec<UpstreamInfoClaim>, StoreError> {
        Ok(self
            .records
            .lock()
            .unwrap()
            .values_mut()
            .filter(|record| {
                self.accounts.0.iter().any(|account| {
                    account.id() == &record.plan.connection.account_id
                        && account.provider() == provider
                }) && record.plan.next_attempt_at <= now
            })
            .take(limit as usize)
            .map(|record| {
                record.plan.claimed_until = Some(now + ttl);
                record.plan.last_attempt_at = Some(now);
                UpstreamInfoClaim {
                    plan: record.plan.clone(),
                    claim_token: uuid::Uuid::new_v4(),
                }
            })
            .collect())
    }
    async fn complete(&self, completion: UpstreamInfoCompletion) -> Result<bool, StoreError> {
        let mut records = self.records.lock().unwrap();
        let record = records.get_mut(&completion.connection.account_id).unwrap();
        assert_eq!(record.plan.connection, completion.connection);
        record.snapshot = Some(completion.result.unwrap());
        record.plan.completed_generation = completion.request_generation;
        record.plan.last_success_at = Some(completion.completed_at);
        record.plan.claimed_until = None;
        record.plan.next_attempt_at = completion.next_attempt_at;
        Ok(true)
    }
}

struct TestAccounts(Vec<ProviderAccount>);

fn loaded(account: &ProviderAccount) -> LoadedCredential {
    LoadedCredential {
        account: account.clone(),
        credential: PlaintextCredential::new(Default::default()),
    }
}

#[async_trait]
impl ProviderAccountStore for TestAccounts {
    async fn create_account(&self, _account: NewProviderAccount) -> Result<(), StoreError> {
        panic!("unused account operation")
    }
    async fn get_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StoreError> {
        Ok(self
            .0
            .iter()
            .find(|account| account.id() == account_id)
            .cloned())
    }
    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, StoreError> {
        panic!("unused account operation")
    }
    async fn list_for_provider(
        &self,
        _provider: &ProviderKind,
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        panic!("unused account operation")
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
        self.0
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
        _accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, StoreError> {
        panic!("unused account operation")
    }
    async fn compare_and_swap_quota(
        &self,
        _observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        panic!("unused account operation")
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
