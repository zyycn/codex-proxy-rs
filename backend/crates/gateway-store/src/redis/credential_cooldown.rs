//! PostgreSQL 账号 cooldown 事实的可丢失 Redis 热缓存。

use std::{sync::Arc, time::SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::{
    engine::credential::{
        AccountAvailability, AccountStateChange, CredentialCasOutcome, CredentialCasUpdate,
        CredentialRevision, LoadedCredential, NewProviderAccount, ProviderAccount,
        ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate, QuotaObservation,
        QuotaWriteOutcome,
    },
    error::StoreError as CoreStoreError,
    routing::ProviderInstanceId,
};
use redis::{Script, aio::ConnectionManager};

use crate::{Revision, StoreError, StoreResult, redis_unavailable, require_nonempty};

use super::{namespace, resource_fingerprint};

const WRITE_SCRIPT: &str = r#"
local current = tonumber(redis.call('HGET', KEYS[1], 'revision') or '0')
local incoming = tonumber(ARGV[1])
local incoming_until = tonumber(ARGV[2])
if current > incoming then return 0 end
local current_until = tonumber(redis.call('HGET', KEYS[1], 'until_ms') or '0')
if current == incoming and current_until >= incoming_until then return 0 end
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
if incoming_until <= now_ms then
  redis.call('DEL', KEYS[1])
  return 0
end
redis.call('HSET', KEYS[1], 'revision', ARGV[1], 'until_ms', ARGV[2])
local ttl = incoming_until - now_ms + 60000
redis.call('PEXPIRE', KEYS[1], ttl)
return 1
"#;

const READ_SCRIPT: &str = r#"
local revision = redis.call('HGET', KEYS[1], 'revision')
local until_ms = redis.call('HGET', KEYS[1], 'until_ms')
if revision == false or until_ms == false then
  redis.call('DEL', KEYS[1])
  return {0, '0', '0'}
end
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
if tonumber(until_ms) <= now_ms then
  redis.call('DEL', KEYS[1])
  return {0, '0', '0'}
end
return {1, revision, until_ms}
"#;

const INVALIDATE_SCRIPT: &str = r#"
local current = tonumber(redis.call('HGET', KEYS[1], 'revision') or '0')
if current > tonumber(ARGV[1]) then return 0 end
redis.call('DEL', KEYS[1])
return 1
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialCooldown {
    pub provider_account_id: String,
    pub credential_revision: Revision,
    pub cooldown_until: DateTime<Utc>,
}

#[async_trait]
pub trait CredentialCooldownRepository: Send + Sync {
    async fn cache_credential_cooldown(&self, cooldown: &CredentialCooldown) -> StoreResult<bool>;
    async fn read_credential_cooldown(
        &self,
        provider_account_id: &str,
    ) -> StoreResult<Option<CredentialCooldown>>;
    async fn invalidate_credential_cooldown(
        &self,
        provider_account_id: &str,
        through_revision: Revision,
    ) -> StoreResult<bool>;
}

#[derive(Clone)]
pub struct RedisCredentialCooldownRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisCredentialCooldownRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    fn key(&self, provider_account_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("credential cooldown", provider_account_id)?;
        Ok(format!("{}:account:{fingerprint}:cooldown", self.namespace))
    }
}

#[async_trait]
impl CredentialCooldownRepository for RedisCredentialCooldownRepository {
    async fn cache_credential_cooldown(&self, cooldown: &CredentialCooldown) -> StoreResult<bool> {
        require_nonempty(
            "credential cooldown",
            "provider_account_id",
            &cooldown.provider_account_id,
        )?;
        let until_ms = cooldown.cooldown_until.timestamp_millis();
        if until_ms <= 0 {
            return Err(invalid("cooldown expiry must be positive"));
        }
        let mut connection = self.connection.clone();
        let written = Script::new(WRITE_SCRIPT)
            .key(self.key(&cooldown.provider_account_id)?)
            .arg(cooldown.credential_revision.get())
            .arg(until_ms)
            .invoke_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("cache credential cooldown"))?;
        Ok(written == 1)
    }

    async fn read_credential_cooldown(
        &self,
        provider_account_id: &str,
    ) -> StoreResult<Option<CredentialCooldown>> {
        require_nonempty(
            "credential cooldown",
            "provider_account_id",
            provider_account_id,
        )?;
        let mut connection = self.connection.clone();
        let (present, revision, until_ms): (i64, String, String) = Script::new(READ_SCRIPT)
            .key(self.key(provider_account_id)?)
            .invoke_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("read credential cooldown"))?;
        if present == 0 {
            return Ok(None);
        }
        let revision = revision
            .parse::<u64>()
            .map_err(|_| invalid("cached cooldown revision is invalid"))?;
        let until_ms = until_ms
            .parse::<i64>()
            .map_err(|_| invalid("cached cooldown expiry is invalid"))?;
        let cooldown_until = DateTime::from_timestamp_millis(until_ms)
            .ok_or_else(|| invalid("cached cooldown expiry is invalid"))?;
        Ok(Some(CredentialCooldown {
            provider_account_id: provider_account_id.to_owned(),
            credential_revision: Revision::new(revision)?,
            cooldown_until,
        }))
    }

    async fn invalidate_credential_cooldown(
        &self,
        provider_account_id: &str,
        through_revision: Revision,
    ) -> StoreResult<bool> {
        require_nonempty(
            "credential cooldown",
            "provider_account_id",
            provider_account_id,
        )?;
        let mut connection = self.connection.clone();
        let removed = Script::new(INVALIDATE_SCRIPT)
            .key(self.key(provider_account_id)?)
            .arg(through_revision.get())
            .invoke_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("invalidate credential cooldown"))?;
        Ok(removed == 1)
    }
}

/// PostgreSQL 账号事实到可丢失 cooldown cache 的 write-through adapter。
pub struct CooldownCachingProviderAccountStore {
    authoritative: Arc<dyn ProviderAccountStore>,
    cooldowns: Arc<dyn CredentialCooldownRepository>,
}

impl CooldownCachingProviderAccountStore {
    #[must_use]
    pub const fn new(
        authoritative: Arc<dyn ProviderAccountStore>,
        cooldowns: Arc<dyn CredentialCooldownRepository>,
    ) -> Self {
        Self {
            authoritative,
            cooldowns,
        }
    }

    /// 启动时从 PostgreSQL 尽力重建仍有效的 cooldown cache。
    pub async fn hydrate(&self, now: SystemTime) -> u64 {
        let Ok(accounts) = self.authoritative.list_accounts().await else {
            return 0;
        };
        let mut hydrated = 0_u64;
        for account in accounts {
            let Some(cooldown) = projection(
                account.id(),
                account.revision(),
                account.availability(),
                account.cooldown_until(),
                now,
            ) else {
                continue;
            };
            if matches!(
                self.cooldowns.cache_credential_cooldown(&cooldown).await,
                Ok(true)
            ) {
                hydrated = hydrated.saturating_add(1);
            }
        }
        hydrated
    }

    async fn mirror(
        &self,
        account_id: &ProviderAccountId,
        credential_revision: CredentialRevision,
        availability: AccountAvailability,
        cooldown_until: Option<SystemTime>,
    ) {
        let Some(cooldown) = projection(
            account_id,
            credential_revision,
            availability,
            cooldown_until,
            SystemTime::now(),
        ) else {
            let Ok(revision) = Revision::new(credential_revision.get()) else {
                return;
            };
            let _ = self
                .cooldowns
                .invalidate_credential_cooldown(account_id.as_str(), revision)
                .await;
            return;
        };
        let _ = self.cooldowns.cache_credential_cooldown(&cooldown).await;
    }
}

#[async_trait]
impl ProviderAccountStore for CooldownCachingProviderAccountStore {
    async fn create_account(&self, account: NewProviderAccount) -> Result<(), CoreStoreError> {
        self.authoritative.create_account(account).await
    }

    async fn get_account(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, CoreStoreError> {
        self.authoritative.get_account(account).await
    }

    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, CoreStoreError> {
        self.authoritative.list_accounts().await
    }

    async fn list_for_instance(
        &self,
        instance: &ProviderInstanceId,
    ) -> Result<Vec<ProviderAccount>, CoreStoreError> {
        self.authoritative.list_for_instance(instance).await
    }

    async fn load_credential(
        &self,
        account: &ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, CoreStoreError> {
        self.authoritative
            .load_credential(account, expected_revision)
            .await
    }

    async fn compare_and_swap_credential(
        &self,
        update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, CoreStoreError> {
        let account_id = update.account_id().clone();
        let outcome = self
            .authoritative
            .compare_and_swap_credential(update)
            .await?;
        if let CredentialCasOutcome::Updated(credential_revision) = outcome
            && let Ok(revision) = Revision::new(credential_revision.get())
        {
            let _ = self
                .cooldowns
                .invalidate_credential_cooldown(account_id.as_str(), revision)
                .await;
        }
        Ok(outcome)
    }

    async fn get_quota(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<QuotaObservation>, CoreStoreError> {
        self.authoritative.get_quota(account).await
    }

    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, CoreStoreError> {
        self.authoritative.compare_and_swap_quota(observation).await
    }

    async fn apply_state_change(&self, change: AccountStateChange) -> Result<(), CoreStoreError> {
        let account_id = change.account_id.clone();
        let credential_revision = change.expected_revision;
        let availability = change.availability;
        let cooldown_until = change.cooldown_until;
        self.authoritative.apply_state_change(change).await?;
        self.mirror(
            &account_id,
            credential_revision,
            availability,
            cooldown_until,
        )
        .await;
        Ok(())
    }

    async fn update_account(&self, update: ProviderAccountUpdate) -> Result<(), CoreStoreError> {
        self.authoritative.update_account(update).await
    }

    async fn set_enabled(
        &self,
        account: &ProviderAccountId,
        enabled: bool,
    ) -> Result<(), CoreStoreError> {
        self.authoritative.set_enabled(account, enabled).await
    }

    async fn delete_account(&self, account: &ProviderAccountId) -> Result<(), CoreStoreError> {
        self.authoritative.delete_account(account).await?;
        if let Ok(through_revision) = Revision::new(u64::MAX) {
            let _ = self
                .cooldowns
                .invalidate_credential_cooldown(account.as_str(), through_revision)
                .await;
        }
        Ok(())
    }
}

fn projection(
    account_id: &ProviderAccountId,
    credential_revision: CredentialRevision,
    availability: AccountAvailability,
    cooldown_until: Option<SystemTime>,
    now: SystemTime,
) -> Option<CredentialCooldown> {
    let cooldown_until = cooldown_until
        .filter(|_| availability == AccountAvailability::Cooldown)
        .filter(|until| *until > now)
        .map(DateTime::<Utc>::from)?;
    Some(CredentialCooldown {
        provider_account_id: account_id.as_str().to_owned(),
        credential_revision: Revision::new(credential_revision.get()).ok()?,
        cooldown_until,
    })
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "credential cooldown",
        message: message.to_owned(),
    }
}
