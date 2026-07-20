//! Codex Provider 测试用内存 ports；不依赖 SQL、Redis 或 secret 加密。

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_core::engine::credential::{
    AccountAvailability, AccountRuntimeSignals, AccountStateChange, CredentialCasOutcome,
    CredentialCasUpdate, CredentialRevision, LoadedCredential, NewProviderAccount, ProviderAccount,
    ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate, QuotaObservation,
    QuotaWriteOutcome,
};
use gateway_core::error::{StoreError, StoreErrorKind};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshPolicy,
    ProviderRuntimePolicyPort, ProviderSchedulingLeaseRequest, ProviderSchedulingState,
    ProviderStoreError,
};
use gateway_core::routing::{ProviderInstanceId, ProviderKind};
use provider_openai::CodexOriginPolicy;
use provider_openai::credential::{
    CodexAccountProfile, CodexCredentialRepository, CodexOAuthSecret,
};
use secrecy::SecretString;
use url::{Host, Url};

#[derive(Clone)]
struct StoredAccount {
    account: ProviderAccount,
    credential: gateway_core::engine::credential::PlaintextCredential,
    quota: Option<QuotaObservation>,
}

#[derive(Default)]
pub(crate) struct MemoryAccountStore {
    accounts: Mutex<BTreeMap<ProviderAccountId, StoredAccount>>,
    quota_reads: AtomicUsize,
}

impl MemoryAccountStore {
    pub(crate) fn repository(self: &Arc<Self>) -> CodexCredentialRepository {
        CodexCredentialRepository::new(self.clone())
    }

    pub(crate) fn account(&self, id: &str) -> Option<ProviderAccount> {
        let id = ProviderAccountId::new(id).ok()?;
        self.accounts
            .lock()
            .expect("account store lock")
            .get(&id)
            .map(|stored| stored.account.clone())
    }

    pub(crate) fn quota_reads(&self) -> usize {
        self.quota_reads.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ProviderAccountStore for MemoryAccountStore {
    async fn create_account(&self, input: NewProviderAccount) -> Result<(), StoreError> {
        let mut accounts = self.accounts.lock().expect("account store lock");
        if accounts.contains_key(input.account.id()) {
            return Err(store_error(StoreErrorKind::Conflict));
        }
        accounts.insert(
            input.account.id().clone(),
            StoredAccount {
                account: input.account,
                credential: input.credential,
                quota: None,
            },
        );
        Ok(())
    }

    async fn get_account(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StoreError> {
        Ok(self
            .accounts
            .lock()
            .expect("account store lock")
            .get(account)
            .map(|stored| stored.account.clone()))
    }

    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, StoreError> {
        Ok(self
            .accounts
            .lock()
            .expect("account store lock")
            .values()
            .map(|stored| stored.account.clone())
            .collect())
    }

    async fn list_for_instance(
        &self,
        instance: &ProviderInstanceId,
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        Ok(self
            .accounts
            .lock()
            .expect("account store lock")
            .values()
            .filter(|stored| stored.account.instance() == instance)
            .map(|stored| stored.account.clone())
            .collect())
    }

    async fn load_credential(
        &self,
        account: &ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, StoreError> {
        let accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts
            .get(account)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        if stored.account.revision() != expected_revision {
            return Err(store_error(StoreErrorKind::Conflict));
        }
        Ok(LoadedCredential {
            account: stored.account.clone(),
            credential: stored.credential.clone(),
        })
    }

    async fn compare_and_swap_credential(
        &self,
        update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, StoreError> {
        let (
            account_id,
            expected_revision,
            profile,
            credential,
            has_refresh_token,
            access_token_expires_at,
            next_refresh_at,
        ) = update.into_parts();
        let mut accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts
            .get_mut(&account_id)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        if stored.account.revision() != expected_revision {
            return Ok(CredentialCasOutcome::Conflict);
        }
        let next = expected_revision
            .next()
            .map_err(|_| store_error(StoreErrorKind::Conflict))?;
        stored.account = rebuild_account(
            &stored.account,
            AccountRebuild {
                revision: next,
                enabled: stored.account.enabled(),
                availability: stored.account.availability(),
                cooldown_until: stored.account.cooldown_until(),
                access_token_expires_at,
                has_refresh_token,
                next_refresh_at,
                profile: Some((profile.name, profile.email, profile.plan_type)),
            },
        );
        stored.credential = credential;
        stored.quota = None;
        Ok(CredentialCasOutcome::Updated(next))
    }

    async fn get_quotas(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, StoreError> {
        self.quota_reads.fetch_add(1, Ordering::SeqCst);
        let stored = self.accounts.lock().expect("account store lock");
        Ok(accounts
            .iter()
            .filter_map(|account| stored.get(account)?.quota.clone())
            .collect())
    }

    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let mut accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts
            .get_mut(&observation.account_id)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        if stored.account.revision() != observation.expected_revision {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        stored.quota = Some(observation);
        Ok(QuotaWriteOutcome::Updated)
    }

    async fn apply_state_change(&self, change: AccountStateChange) -> Result<(), StoreError> {
        let mut accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts
            .get_mut(&change.account_id)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        if stored.account.revision() != change.expected_revision {
            return Err(store_error(StoreErrorKind::Conflict));
        }
        stored.account = rebuild_account(
            &stored.account,
            AccountRebuild {
                revision: stored.account.revision(),
                enabled: stored.account.enabled(),
                availability: change.availability,
                cooldown_until: change.cooldown_until,
                access_token_expires_at: stored.account.access_token_expires_at(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                profile: None,
            },
        );
        Ok(())
    }

    async fn update_account(&self, update: ProviderAccountUpdate) -> Result<(), StoreError> {
        let mut accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts
            .get_mut(&update.account_id)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        stored.account = rebuild_account(
            &stored.account,
            AccountRebuild {
                revision: stored.account.revision(),
                enabled: stored.account.enabled(),
                availability: stored.account.availability(),
                cooldown_until: stored.account.cooldown_until(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                profile: Some((update.name, update.email, update.plan_type)),
            },
        );
        Ok(())
    }

    async fn set_enabled(
        &self,
        account: &ProviderAccountId,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let mut accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts
            .get_mut(account)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        stored.account = rebuild_account(
            &stored.account,
            AccountRebuild {
                revision: stored.account.revision(),
                enabled,
                availability: stored.account.availability(),
                cooldown_until: stored.account.cooldown_until(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                profile: None,
            },
        );
        Ok(())
    }

    async fn delete_account(&self, account: &ProviderAccountId) -> Result<(), StoreError> {
        self.accounts
            .lock()
            .expect("account store lock")
            .remove(account)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        Ok(())
    }
}

const fn store_error(kind: StoreErrorKind) -> StoreError {
    StoreError::new(kind)
}

#[derive(Debug)]
pub(crate) struct LoopbackCodexOriginPolicy;

impl CodexOriginPolicy for LoopbackCodexOriginPolicy {
    fn allows(&self, url: &Url) -> bool {
        url.scheme() == "http"
            && matches!(url.host(), Some(Host::Ipv4(host)) if host.is_loopback())
            && url.port().is_some()
    }
}

pub(crate) fn loopback_origin_policy() -> Arc<dyn CodexOriginPolicy> {
    Arc::new(LoopbackCodexOriginPolicy)
}

struct AccountRebuild {
    revision: CredentialRevision,
    enabled: bool,
    availability: AccountAvailability,
    cooldown_until: Option<SystemTime>,
    access_token_expires_at: SystemTime,
    has_refresh_token: bool,
    next_refresh_at: Option<SystemTime>,
    profile: Option<(String, Option<String>, Option<String>)>,
}

fn rebuild_account(current: &ProviderAccount, rebuild: AccountRebuild) -> ProviderAccount {
    let (name, email, plan_type) = rebuild.profile.unwrap_or_else(|| {
        (
            current.name().to_owned(),
            current.email().map(str::to_owned),
            current.plan_type().map(str::to_owned),
        )
    });
    ProviderAccount::new(
        current.id().clone(),
        current.instance().clone(),
        current.provider().clone(),
        name,
        current.upstream_user_id().to_owned(),
        rebuild.revision,
        rebuild.access_token_expires_at,
    )
    .with_profile(
        email,
        current.upstream_account_id().map(str::to_owned),
        plan_type,
    )
    .with_runtime_state(
        rebuild.enabled,
        rebuild.availability,
        rebuild.cooldown_until,
    )
    .with_refresh_schedule(rebuild.has_refresh_token, rebuild.next_refresh_at)
}

#[derive(Default)]
pub(crate) struct TestLeaseCoordinator {
    pub(crate) requests: Mutex<Vec<ProviderSchedulingLeaseRequest>>,
    pub(crate) busy: Mutex<bool>,
    round_robin_cursor: Mutex<u64>,
}

impl ProviderLeasePort for TestLeaseCoordinator {
    fn load_state<'a>(
        &'a self,
        _provider_instance_id: &'a ProviderInstanceId,
        accounts: &'a [ProviderAccountId],
    ) -> BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>> {
        Box::pin(async move {
            let signals = accounts
                .iter()
                .cloned()
                .map(|account| {
                    (
                        account,
                        AccountRuntimeSignals {
                            in_flight: 0,
                            last_started_at: None,
                            quota_reset_at: None,
                            quota_remaining_rank: None,
                        },
                    )
                })
                .collect();
            let mut cursor = self
                .round_robin_cursor
                .lock()
                .expect("round robin cursor lock");
            let current = *cursor;
            *cursor = cursor.wrapping_add(1);
            Ok(ProviderSchedulingState::new(signals, None, current))
        })
    }

    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async move {
            let ProviderLeaseRequest::Scheduling(request) = request else {
                panic!("expected scheduling lease request");
            };
            self.requests
                .lock()
                .expect("lease requests lock")
                .push(request);
            if *self.busy.lock().expect("lease busy lock") {
                Ok(ProviderLeaseAcquisition::Busy {
                    retry_after: Some(Duration::from_millis(25)),
                })
            } else {
                Ok(ProviderLeaseAcquisition::Acquired(Box::new(())))
            }
        })
    }
}

pub(crate) fn profile(account_id: &str) -> CodexAccountProfile {
    let now = chrono::Utc::now();
    CodexAccountProfile {
        oauth_subject: format!("subject-{account_id}"),
        poid: Some(format!("poid-{account_id}")),
        chatgpt_account_id: account_id.to_owned(),
        chatgpt_user_id: format!("user-{account_id}"),
        email: Some(format!("{account_id}@example.com")),
        plan_type: Some("pro".to_owned()),
        access_token_expires_at: Some(now + chrono::Duration::hours(1)),
    }
}

pub(crate) struct StaticRuntimePolicy;

impl ProviderRuntimePolicyPort for StaticRuntimePolicy {
    fn load_refresh_policy(
        &self,
    ) -> BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        Box::pin(async {
            ProviderRefreshPolicy::try_new(
                Duration::from_secs(60 * 60),
                NonZeroU32::new(2).expect("positive concurrency"),
            )
        })
    }
}

pub(crate) fn runtime_policy() -> Arc<dyn ProviderRuntimePolicyPort> {
    Arc::new(StaticRuntimePolicy)
}

pub(crate) fn secret(access_token: &str) -> CodexOAuthSecret {
    CodexOAuthSecret {
        access_token: SecretString::from(access_token.to_owned()),
        refresh_token: Some(SecretString::from(format!("rt-{access_token}"))),
        id_token: None,
    }
}

pub(crate) fn account_policy() -> gateway_core::engine::credential::AccountSelectionPolicy {
    gateway_core::engine::credential::AccountSelectionPolicy::new(
        gateway_core::engine::credential::RotationStrategy::Smart,
        NonZeroU32::new(2).expect("nonzero concurrency"),
        Duration::from_millis(10),
    )
}

pub(crate) fn instance_id() -> ProviderInstanceId {
    ProviderInstanceId::new("inst_openai_primary").expect("instance id")
}

pub(crate) fn codex_account(id: &str) -> ProviderAccount {
    ProviderAccount::new(
        ProviderAccountId::new(id).expect("account id"),
        instance_id(),
        ProviderKind::new("openai").expect("provider"),
        id.to_owned(),
        format!("user-chatgpt-{id}"),
        CredentialRevision::new(1).expect("revision"),
        SystemTime::now() + Duration::from_secs(3_600),
    )
    .with_profile(
        Some(format!("{id}@example.com")),
        Some(format!("chatgpt-{id}")),
        Some("pro".to_owned()),
    )
    .with_runtime_state(true, AccountAvailability::Ready, None)
    .with_refresh_schedule(true, Some(SystemTime::now() + Duration::from_secs(2_700)))
}
