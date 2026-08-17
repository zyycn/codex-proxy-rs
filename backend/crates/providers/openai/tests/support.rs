//! Codex Provider 测试用内存 ports；不依赖 SQL、Redis 或 secret 加密。

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_core::engine::credential::{
    AccountConcurrencyLimit, AccountErrorReason, AccountRuntimeSignals, AccountStateChange,
    AccountWeight, CredentialCasOutcome, CredentialCasUpdate, CredentialCasUpdateParts,
    CredentialRevision, CredentialState, LoadedCredential, NewProviderAccount, OpaqueProviderData,
    ProviderAccount, ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate,
    QuotaAccessChange, QuotaObservation, QuotaObservationTouch, QuotaState, QuotaWriteOutcome,
};
use gateway_core::error::{StoreError, StoreErrorKind};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderCooldown, ProviderCooldownPort,
    ProviderCooldownScope, ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest,
    ProviderRefreshPolicy, ProviderRuntimePolicyPort, ProviderSchedulingLeaseRequest,
    ProviderSchedulingState, ProviderScopedCooldown, ProviderSessionAffinityKey,
    ProviderSessionAffinityPort, ProviderSessionExclusionPort, ProviderSessionExclusions,
    ProviderStoreError,
};
use gateway_core::routing::ProviderKind;
use provider_openai::credential::{
    CodexAccountProfile, CodexAgentIdentityTaskService, CodexCredentialAdmin,
    CodexCredentialRepository, CodexOAuthSecret, ImportCodexOAuthCredential,
    OfficialCodexAgentIdentityTaskRegistrar,
};
use provider_openai::transport::CodexWebSocketPool;
use secrecy::SecretString;

#[derive(Clone)]
struct StoredAccount {
    account: ProviderAccount,
    credential: gateway_core::engine::credential::PlaintextCredential,
    quota: Option<QuotaObservation>,
    state_observed_at: Option<SystemTime>,
}

#[derive(Default)]
pub(crate) struct MemoryAccountStore {
    accounts: Mutex<BTreeMap<ProviderAccountId, StoredAccount>>,
    quota_reads: AtomicUsize,
    fail_provider_listing: AtomicBool,
}

impl MemoryAccountStore {
    pub(crate) fn repository(self: &Arc<Self>) -> CodexCredentialRepository {
        CodexCredentialRepository::new(self.clone())
    }

    pub(crate) async fn seed_oauth_credential(&self, input: ImportCodexOAuthCredential) {
        let account = CodexCredentialAdmin
            .prepare_import(input)
            .expect("prepare test OAuth credential");
        self.create_account(account)
            .await
            .expect("seed test OAuth credential");
    }

    pub(crate) fn account(&self, id: &str) -> Option<ProviderAccount> {
        let id = ProviderAccountId::new(id).ok()?;
        self.accounts
            .lock()
            .expect("account store lock")
            .get(&id)
            .map(|stored| stored.account.clone())
    }

    pub(crate) fn set_scheduling(
        &self,
        id: &str,
        concurrency_limit: Option<AccountConcurrencyLimit>,
        weight: AccountWeight,
    ) {
        let id = ProviderAccountId::new(id).expect("valid account ID");
        let mut accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts.get_mut(&id).expect("seeded account");
        stored.account = stored
            .account
            .clone()
            .with_scheduling(concurrency_limit, weight);
    }

    pub(crate) fn quota_reads(&self) -> usize {
        self.quota_reads.load(Ordering::SeqCst)
    }

    pub(crate) fn has_quota(&self, id: &str) -> bool {
        let Some(id) = ProviderAccountId::new(id).ok() else {
            return false;
        };
        self.accounts
            .lock()
            .expect("account store lock")
            .get(&id)
            .is_some_and(|stored| stored.quota.is_some())
    }

    pub(crate) fn quota_json(&self, id: &str) -> Option<serde_json::Value> {
        let id = ProviderAccountId::new(id).ok()?;
        self.accounts
            .lock()
            .expect("account store lock")
            .get(&id)
            .and_then(|stored| stored.quota.as_ref())
            .map(|observation| {
                serde_json::Value::Object(observation.quota.expose_to_provider().clone())
            })
    }

    pub(crate) fn fail_provider_listing(&self) {
        self.fail_provider_listing.store(true, Ordering::SeqCst);
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
                state_observed_at: None,
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

    async fn list_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        if self.fail_provider_listing.load(Ordering::SeqCst) {
            return Err(store_error(StoreErrorKind::Unavailable));
        }
        Ok(self
            .accounts
            .lock()
            .expect("account store lock")
            .values()
            .filter(|stored| stored.account.provider() == provider)
            .map(|stored| stored.account.clone())
            .collect())
    }

    async fn load_credential(
        &self,
        account: &ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, StoreError> {
        let loaded = self.load_current_credential(account).await?;
        if loaded.account.revision() != expected_revision {
            return Err(store_error(StoreErrorKind::Conflict));
        }
        Ok(loaded)
    }

    async fn load_current_credential(
        &self,
        account: &ProviderAccountId,
    ) -> Result<LoadedCredential, StoreError> {
        let accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts
            .get(account)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        Ok(LoadedCredential {
            account: stored.account.clone(),
            credential: stored.credential.clone(),
        })
    }

    async fn compare_and_swap_credential(
        &self,
        update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, StoreError> {
        let CredentialCasUpdateParts {
            account_id,
            expected_revision,
            profile,
            credential,
            has_refresh_token,
            access_token_expires_at,
            next_refresh_at,
            account_state,
        } = update.into_parts();
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
        let credential_state = account_state
            .as_ref()
            .map_or(stored.account.credential_state(), |state| {
                state.credential_state
            });
        let last_error_reason = account_state.as_ref().map_or_else(
            || stored.account.last_error_reason(),
            |state| state.error_reason,
        );
        let last_error_message = account_state.as_ref().map_or_else(
            || stored.account.last_error_message().map(str::to_owned),
            |state| state.message.clone(),
        );
        let state_observed_at = account_state.as_ref().map(|state| state.observed_at);
        stored.account = rebuild_account(
            &stored.account,
            AccountRebuild {
                revision: next,
                enabled: stored.account.enabled(),
                credential_state,
                quota: stored.account.quota(),
                last_error_reason,
                last_error_message,
                access_token_expires_at,
                has_refresh_token,
                next_refresh_at,
                profile: Some((profile.name, profile.email, profile.plan_type)),
            },
        );
        stored.credential = credential;
        stored.quota = None;
        if let Some(observed_at) = state_observed_at {
            stored.state_observed_at = Some(observed_at);
        }
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
        if stored
            .quota
            .as_ref()
            .is_some_and(|quota| quota.observed_at > observation.observed_at)
        {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        let quota = observation.state;
        stored.quota = Some(observation);
        stored.account = rebuild_account(
            &stored.account,
            AccountRebuild::preserving(&stored.account).with_quota(quota),
        );
        Ok(QuotaWriteOutcome::Updated)
    }

    async fn touch_quota_observation(
        &self,
        touch: QuotaObservationTouch,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let mut accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts
            .get_mut(&touch.account_id)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        if stored.account.revision() != touch.expected_revision {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        let Some(quota) = &mut stored.quota else {
            return Ok(QuotaWriteOutcome::Conflict);
        };
        if quota.observed_at > touch.observed_at {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        quota.observed_at = touch.observed_at;
        Ok(QuotaWriteOutcome::Updated)
    }

    async fn apply_quota_access(
        &self,
        change: QuotaAccessChange,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let mut accounts = self.accounts.lock().expect("account store lock");
        let stored = accounts
            .get_mut(&change.account_id)
            .ok_or_else(|| store_error(StoreErrorKind::InvalidData))?;
        if stored.account.revision() != change.expected_revision {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        stored.account = rebuild_account(
            &stored.account,
            AccountRebuild::preserving(&stored.account).with_quota(change.state),
        );
        if let Some(observation) = &mut stored.quota {
            observation.state = change.state;
        }
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
        if stored
            .state_observed_at
            .is_some_and(|current| current > change.observed_at)
        {
            return Err(store_error(StoreErrorKind::Conflict));
        }
        stored.account = rebuild_account(
            &stored.account,
            AccountRebuild {
                revision: stored.account.revision(),
                enabled: stored.account.enabled(),
                credential_state: change.credential_state,
                quota: stored.account.quota(),
                last_error_reason: change.error_reason,
                last_error_message: change.message,
                access_token_expires_at: stored.account.access_token_expires_at(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                profile: None,
            },
        );
        stored.state_observed_at = Some(change.observed_at);
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
                credential_state: stored.account.credential_state(),
                quota: stored.account.quota(),
                last_error_reason: stored.account.last_error_reason(),
                last_error_message: stored.account.last_error_message().map(str::to_owned),
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
                credential_state: stored.account.credential_state(),
                quota: stored.account.quota(),
                last_error_reason: stored.account.last_error_reason(),
                last_error_message: stored.account.last_error_message().map(str::to_owned),
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

struct AccountRebuild {
    revision: CredentialRevision,
    enabled: bool,
    credential_state: CredentialState,
    quota: QuotaState,
    last_error_reason: Option<AccountErrorReason>,
    last_error_message: Option<String>,
    access_token_expires_at: Option<SystemTime>,
    has_refresh_token: bool,
    next_refresh_at: Option<SystemTime>,
    profile: Option<(String, Option<String>, Option<String>)>,
}

impl AccountRebuild {
    fn preserving(account: &ProviderAccount) -> Self {
        Self {
            revision: account.revision(),
            enabled: account.enabled(),
            credential_state: account.credential_state(),
            quota: account.quota(),
            last_error_reason: account.last_error_reason(),
            last_error_message: account.last_error_message().map(str::to_owned),
            access_token_expires_at: account.access_token_expires_at(),
            has_refresh_token: account.has_refresh_token(),
            next_refresh_at: account.next_refresh_at(),
            profile: None,
        }
    }

    fn with_quota(mut self, quota: QuotaState) -> Self {
        self.quota = quota;
        self
    }
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
        current.provider().clone(),
        name,
        current.upstream_user_id().map(str::to_owned),
        current.authentication_kind().to_owned(),
        rebuild.revision,
        rebuild.access_token_expires_at,
    )
    .with_profile(
        email,
        current.upstream_account_id().map(str::to_owned),
        plan_type,
    )
    .with_account_facts(
        rebuild.enabled,
        rebuild.credential_state,
        rebuild.quota,
        rebuild.last_error_reason,
        rebuild.last_error_message,
    )
    .with_scheduling(current.concurrency_limit(), current.weight())
    .with_refresh_schedule(rebuild.has_refresh_token, rebuild.next_refresh_at)
}

pub(crate) fn agent_identity_service(
    store: &Arc<MemoryAccountStore>,
) -> Arc<CodexAgentIdentityTaskService> {
    agent_identity_service_with_pool(store, Arc::new(CodexWebSocketPool::default()))
}

pub(crate) fn agent_identity_service_with_pool(
    store: &Arc<MemoryAccountStore>,
    websocket_pool: Arc<CodexWebSocketPool>,
) -> Arc<CodexAgentIdentityTaskService> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("agent task client");
    let registrar = OfficialCodexAgentIdentityTaskRegistrar::new(
        client,
        provider_openai::OpenAiConfig::default().wire_profile_state(),
    )
    .expect("agent task registrar");
    Arc::new(CodexAgentIdentityTaskService::new(
        store.repository(),
        Arc::new(registrar),
        websocket_pool,
    ))
}

#[derive(Default)]
pub(crate) struct TestLeaseCoordinator {
    pub(crate) requests: Mutex<Vec<ProviderSchedulingLeaseRequest>>,
    pub(crate) busy: Mutex<bool>,
    pub(crate) busy_accounts: Mutex<BTreeSet<ProviderAccountId>>,
    round_robin_cursor: Mutex<u64>,
}

impl ProviderLeasePort for TestLeaseCoordinator {
    fn load_state<'a>(
        &'a self,
        _client_api_key_id: &'a ClientApiKeyId,
        _provider_kind: &'a ProviderKind,
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
                            rate_limited_until: None,
                            failure_rate_basis_points: None,
                            first_output_latency_ms: None,
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
            Ok(ProviderSchedulingState::new(signals, current))
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
            let account_busy = self
                .busy_accounts
                .lock()
                .expect("busy account lock")
                .contains(request.account_id());
            self.requests
                .lock()
                .expect("lease requests lock")
                .push(request);
            if *self.busy.lock().expect("lease busy lock") || account_busy {
                Ok(ProviderLeaseAcquisition::Busy {
                    retry_after: Some(Duration::from_millis(25)),
                })
            } else {
                Ok(ProviderLeaseAcquisition::Acquired(Box::new(())))
            }
        })
    }
}

#[derive(Default)]
pub(crate) struct MemorySessionAffinity {
    bindings: Mutex<BTreeMap<(String, String), ProviderAccountId>>,
    lookups: Mutex<Vec<String>>,
}

impl MemorySessionAffinity {
    pub(crate) fn lookup_keys(&self) -> Vec<String> {
        self.lookups.lock().expect("session affinity lock").clone()
    }

    pub(crate) fn binding_count(&self) -> usize {
        self.bindings.lock().expect("session affinity lock").len()
    }
}

impl ProviderSessionAffinityPort for MemorySessionAffinity {
    fn load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderAccountId>, ProviderStoreError>> {
        Box::pin(async move {
            self.lookups
                .lock()
                .expect("session affinity lookup lock")
                .push(key.expose_to_store().to_owned());
            Ok(self
                .bindings
                .lock()
                .expect("session affinity lock")
                .get(&(
                    provider_kind.as_str().to_owned(),
                    key.expose_to_store().to_owned(),
                ))
                .cloned())
        })
    }

    fn bind<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        account_id: &'a ProviderAccountId,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            self.bindings.lock().expect("session affinity lock").insert(
                (
                    provider_kind.as_str().to_owned(),
                    key.expose_to_store().to_owned(),
                ),
                account_id.clone(),
            );
            Ok(())
        })
    }

    fn clear<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            Ok(self
                .bindings
                .lock()
                .expect("session affinity lock")
                .remove(&(
                    provider_kind.as_str().to_owned(),
                    key.expose_to_store().to_owned(),
                ))
                .is_some())
        })
    }
}

#[derive(Default)]
pub(crate) struct MemorySessionExclusions {
    states: Mutex<BTreeMap<(String, String), ProviderSessionExclusions>>,
    revision: AtomicUsize,
}

impl ProviderSessionExclusionPort for MemorySessionExclusions {
    fn load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderSessionExclusions>, ProviderStoreError>> {
        Box::pin(async move {
            Ok(self
                .states
                .lock()
                .expect("session exclusion lock")
                .get(&(
                    provider_kind.as_str().to_owned(),
                    key.expose_to_store().to_owned(),
                ))
                .cloned())
        })
    }

    fn record_failure<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        account_id: &'a ProviderAccountId,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<ProviderSessionExclusions, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                key.expose_to_store().to_owned(),
            );
            let mut states = self.states.lock().expect("session exclusion lock");
            let mut excluded_accounts = states
                .get(&key)
                .map(|state| state.excluded_accounts().clone())
                .unwrap_or_default();
            excluded_accounts.insert(account_id.clone());
            let state = ProviderSessionExclusions::new(
                excluded_accounts,
                self.revision.fetch_add(1, Ordering::SeqCst).to_string(),
            );
            states.insert(key, state.clone());
            Ok(state)
        })
    }

    fn clear<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        expected_revision: &'a str,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                key.expose_to_store().to_owned(),
            );
            let mut states = self.states.lock().expect("session exclusion lock");
            if states
                .get(&key)
                .is_some_and(|state| state.revision() == expected_revision)
            {
                states.remove(&key);
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }
}

#[derive(Default)]
pub(crate) struct MemoryCatalogCache {
    values: Mutex<BTreeMap<String, OpaqueProviderData>>,
}

impl ProviderCatalogCachePort for MemoryCatalogCache {
    fn replace<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
        catalog: &'a OpaqueProviderData,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            self.values
                .lock()
                .expect("catalog cache")
                .insert(key.scope().as_str().to_owned(), catalog.clone());
            Ok(())
        })
    }

    fn read<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
    ) -> BoxFuture<'a, Result<Option<OpaqueProviderData>, ProviderStoreError>> {
        Box::pin(async move {
            Ok(self
                .values
                .lock()
                .expect("catalog cache")
                .get(key.scope().as_str())
                .cloned())
        })
    }
}

pub(crate) fn catalog_cache() -> Arc<dyn ProviderCatalogCachePort> {
    Arc::new(MemoryCatalogCache::default())
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

/// 内存 `ProviderCooldownPort`：只实现 `read`/`put_if_later`（openai selector
/// 429 冷却路径用），其余 scope 变体测试不涉及，返回占位。
#[derive(Clone, Default)]
pub(crate) struct MemoryCooldownPort {
    pub(crate) cooldowns: Arc<Mutex<BTreeMap<ProviderAccountId, ProviderCooldown>>>,
}

impl MemoryCooldownPort {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl ProviderCooldownPort for MemoryCooldownPort {
    fn put_if_later(
        &self,
        cooldown: ProviderCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let mut cooldowns = self.cooldowns.lock().expect("cooldown lock");
            let should_write = cooldowns.get(cooldown.account_id()).is_none_or(|current| {
                current.credential_revision() < cooldown.credential_revision()
                    || (current.credential_revision() == cooldown.credential_revision()
                        && current.until() < cooldown.until())
            });
            if should_write {
                cooldowns.insert(cooldown.account_id().clone(), cooldown);
            }
            Ok(should_write)
        })
    }

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async move {
            Ok(self
                .cooldowns
                .lock()
                .expect("cooldown lock")
                .get(account_id)
                .cloned())
        })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn put_scoped_if_later(
        &self,
        _cooldown: ProviderScopedCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn clear_all<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
}
