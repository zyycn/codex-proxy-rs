use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use chrono::Utc;
use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, CredentialCasOutcome, CredentialCasUpdate,
    CredentialRevision, LoadedCredential, NewProviderAccount, PlaintextCredential, ProviderAccount,
    ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate, QuotaObservation,
    QuotaWriteOutcome,
};
use gateway_core::error::{StoreError, StoreErrorKind};
use gateway_core::routing::ProviderInstanceId;
use provider_xai::{
    CreateGrokCredential, GrokAccountCatalog, GrokAccountProfile, GrokCatalogCacheError,
    GrokCredentialAdmin, GrokCredentialAvailability, GrokCredentialCatalogCache,
    GrokCredentialRepositoryError, GrokOAuthSecret, SecretValue,
};

#[derive(Clone)]
struct StoredAccount {
    account: ProviderAccount,
    credential: PlaintextCredential,
    quota: Option<QuotaObservation>,
}

#[derive(Default)]
pub struct MemoryProviderAccountStore {
    accounts: Mutex<BTreeMap<ProviderAccountId, StoredAccount>>,
}

impl MemoryProviderAccountStore {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn account(&self, id: &ProviderAccountId) -> Option<ProviderAccount> {
        lock(&self.accounts)
            .get(id)
            .map(|stored| stored.account.clone())
    }

    pub fn credential(&self, id: &ProviderAccountId) -> Option<PlaintextCredential> {
        lock(&self.accounts)
            .get(id)
            .map(|stored| stored.credential.clone())
    }

    pub fn len(&self) -> usize {
        lock(&self.accounts).len()
    }
}

#[async_trait]
impl ProviderAccountStore for MemoryProviderAccountStore {
    async fn create_account(&self, account: NewProviderAccount) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        if accounts.contains_key(account.account.id()) {
            return Err(conflict());
        }
        accounts.insert(
            account.account.id().clone(),
            StoredAccount {
                account: account.account,
                credential: account.credential,
                quota: None,
            },
        );
        Ok(())
    }

    async fn get_account(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StoreError> {
        Ok(self.account(account))
    }

    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, StoreError> {
        Ok(lock(&self.accounts)
            .values()
            .map(|stored| stored.account.clone())
            .collect())
    }

    async fn list_for_instance(
        &self,
        instance: &ProviderInstanceId,
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        Ok(lock(&self.accounts)
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
        let accounts = lock(&self.accounts);
        let stored = accounts.get(account).ok_or_else(invalid)?;
        if stored.account.revision() != expected_revision {
            return Err(conflict());
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
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&account_id).ok_or_else(invalid)?;
        if stored.account.revision() != expected_revision {
            return Ok(CredentialCasOutcome::Conflict);
        }
        let next = expected_revision.next().map_err(|_| invalid())?;
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: next,
                access_token_expires_at,
                availability: stored.account.availability(),
                cooldown_until: stored.account.cooldown_until(),
                enabled: stored.account.enabled(),
                has_refresh_token,
                next_refresh_at,
                name: profile.name,
                email: profile.email,
                plan_type: profile.plan_type,
            },
        );
        stored.credential = credential;
        Ok(CredentialCasOutcome::Updated(next))
    }

    async fn get_quota(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<QuotaObservation>, StoreError> {
        Ok(lock(&self.accounts)
            .get(account)
            .and_then(|stored| stored.quota.clone()))
    }

    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts
            .get_mut(&observation.account_id)
            .ok_or_else(invalid)?;
        if stored.account.revision() != observation.expected_revision {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        stored.quota = Some(observation);
        Ok(QuotaWriteOutcome::Updated)
    }

    async fn apply_state_change(&self, change: AccountStateChange) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&change.account_id).ok_or_else(invalid)?;
        if stored.account.revision() != change.expected_revision {
            return Err(conflict());
        }
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: stored.account.revision(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                availability: change.availability,
                cooldown_until: change.cooldown_until,
                enabled: stored.account.enabled(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                name: stored.account.name().to_owned(),
                email: stored.account.email().map(str::to_owned),
                plan_type: stored.account.plan_type().map(str::to_owned),
            },
        );
        Ok(())
    }

    async fn update_account(&self, update: ProviderAccountUpdate) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&update.account_id).ok_or_else(invalid)?;
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: stored.account.revision(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                availability: stored.account.availability(),
                cooldown_until: stored.account.cooldown_until(),
                enabled: stored.account.enabled(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                name: update.name,
                email: update.email,
                plan_type: update.plan_type,
            },
        );
        Ok(())
    }

    async fn set_enabled(
        &self,
        account: &ProviderAccountId,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(account).ok_or_else(invalid)?;
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: stored.account.revision(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                availability: stored.account.availability(),
                cooldown_until: stored.account.cooldown_until(),
                enabled,
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                name: stored.account.name().to_owned(),
                email: stored.account.email().map(str::to_owned),
                plan_type: stored.account.plan_type().map(str::to_owned),
            },
        );
        Ok(())
    }

    async fn delete_account(&self, account: &ProviderAccountId) -> Result<(), StoreError> {
        lock(&self.accounts).remove(account).ok_or_else(invalid)?;
        Ok(())
    }
}

struct AccountReplacement {
    revision: CredentialRevision,
    access_token_expires_at: SystemTime,
    availability: AccountAvailability,
    cooldown_until: Option<SystemTime>,
    enabled: bool,
    has_refresh_token: bool,
    next_refresh_at: Option<SystemTime>,
    name: String,
    email: Option<String>,
    plan_type: Option<String>,
}

fn rebuild_account(previous: &ProviderAccount, replacement: AccountReplacement) -> ProviderAccount {
    ProviderAccount::new(
        previous.id().clone(),
        previous.instance().clone(),
        previous.provider().clone(),
        replacement.name,
        previous.upstream_user_id().to_owned(),
        replacement.revision,
        replacement.access_token_expires_at,
    )
    .with_profile(
        replacement.email,
        previous.upstream_account_id().map(str::to_owned),
        replacement.plan_type,
    )
    .with_runtime_state(
        replacement.enabled,
        replacement.availability,
        replacement.cooldown_until,
    )
    .with_refresh_schedule(replacement.has_refresh_token, replacement.next_refresh_at)
}

#[derive(Default)]
pub struct MemoryGrokCatalogCache {
    entries: Mutex<BTreeMap<(ProviderAccountId, CredentialRevision), GrokAccountCatalog>>,
}

impl MemoryGrokCatalogCache {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl GrokCredentialCatalogCache for MemoryGrokCatalogCache {
    async fn replace(&self, catalog: GrokAccountCatalog) -> Result<(), GrokCatalogCacheError> {
        lock(&self.entries).insert((catalog.account_id().clone(), catalog.revision()), catalog);
        Ok(())
    }

    async fn read(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> Result<Option<GrokAccountCatalog>, GrokCatalogCacheError> {
        Ok(lock(&self.entries)
            .get(&(account_id.clone(), revision))
            .cloned())
    }

    async fn permits(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
        model: &str,
    ) -> Result<bool, GrokCatalogCacheError> {
        Ok(lock(&self.entries)
            .get(&(account_id.clone(), revision))
            .is_some_and(|catalog| catalog.seed().permits(model)))
    }
}

pub fn account_id(suffix: &str) -> ProviderAccountId {
    ProviderAccountId::new(format!("acct_{suffix}")).expect("valid test account ID")
}

pub fn instance_id() -> ProviderInstanceId {
    ProviderInstanceId::new("inst_xai").expect("valid test instance ID")
}

pub fn profile(subject: &str) -> GrokAccountProfile {
    let now = Utc::now();
    GrokAccountProfile {
        subject: subject.to_owned(),
        email: Some(format!("{subject}@example.com")),
        upstream_account_id: None,
        plan_type: Some("standard".to_owned()),
        access_token_expires_at: now + chrono::Duration::hours(1),
        refresh_token_expires_at: Some(now + chrono::Duration::days(30)),
        next_refresh_at: Some(now + chrono::Duration::minutes(30)),
    }
}

pub fn create_input(suffix: &str, subject: &str) -> CreateGrokCredential {
    CreateGrokCredential {
        account_id: account_id(suffix),
        provider_instance_id: instance_id(),
        name: format!("xAI {suffix}"),
        secret: GrokOAuthSecret {
            access_token: SecretValue::new(format!("access-{suffix}")),
            refresh_token: SecretValue::new(format!("refresh-{suffix}")),
            id_token: Some(SecretValue::new(format!("id-{suffix}"))),
            scope: provider_xai::OFFICIAL_SCOPES.join(" "),
        },
        account: profile(subject),
        enabled: true,
        initial_availability: GrokCredentialAvailability::Unknown,
        initial_availability_reason: None,
        initial_cooldown_until: None,
    }
}

pub fn prepare_input(
    input: &CreateGrokCredential,
) -> Result<NewProviderAccount, GrokCredentialRepositoryError> {
    GrokCredentialAdmin.prepare_import(input)
}

pub async fn seed_input(
    store: &MemoryProviderAccountStore,
    input: &CreateGrokCredential,
) -> Result<(), StoreError> {
    let prepared = prepare_input(input).expect("valid xAI account fixture");
    store.create_account(prepared).await
}

pub fn credential_object(
    credential: &PlaintextCredential,
) -> &serde_json::Map<String, serde_json::Value> {
    credential.expose_to_provider()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn conflict() -> StoreError {
    StoreError::new(StoreErrorKind::Conflict)
}

fn invalid() -> StoreError {
    StoreError::new(StoreErrorKind::InvalidData)
}
