//! 请求调度用的可丢失 Provider 级 cooldown Redis 存储。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::{
    engine::credential::{CredentialRevision, ProviderAccountId},
    provider_ports::{
        ProviderCooldown, ProviderCooldownPort, ProviderCooldownScope, ProviderScopedCooldown,
        ProviderStoreError, ProviderStoreErrorKind,
    },
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

    fn scoped_key(
        &self,
        provider_account_id: &str,
        scope: &ProviderCooldownScope,
    ) -> StoreResult<String> {
        let account_fingerprint = resource_fingerprint("credential cooldown", provider_account_id)?;
        let scope_fingerprint = resource_fingerprint("credential cooldown scope", scope.value())?;
        Ok(format!(
            "{}:account:{account_fingerprint}:cooldown:{}:{scope_fingerprint}",
            self.namespace,
            scope.kind(),
        ))
    }

    async fn cache_at_key(
        &self,
        key: String,
        credential_revision: Revision,
        cooldown_until: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let until_ms = cooldown_until.timestamp_millis();
        if until_ms <= 0 {
            return Err(invalid("cooldown expiry must be positive"));
        }
        let mut connection = self.connection.clone();
        let written = Script::new(WRITE_SCRIPT)
            .key(key)
            .arg(credential_revision.get())
            .arg(until_ms)
            .invoke_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("cache credential cooldown"))?;
        Ok(written == 1)
    }

    async fn read_at_key(&self, key: String) -> StoreResult<Option<(Revision, DateTime<Utc>)>> {
        let mut connection = self.connection.clone();
        let (present, revision, until_ms): (i64, String, String) = Script::new(READ_SCRIPT)
            .key(key)
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
        Ok(Some((Revision::new(revision)?, cooldown_until)))
    }

    async fn invalidate_at_key(
        &self,
        key: String,
        through_revision: Revision,
    ) -> StoreResult<bool> {
        let mut connection = self.connection.clone();
        let removed = Script::new(INVALIDATE_SCRIPT)
            .key(key)
            .arg(through_revision.get())
            .invoke_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("invalidate credential cooldown"))?;
        Ok(removed == 1)
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
        self.cache_at_key(
            self.key(&cooldown.provider_account_id)?,
            cooldown.credential_revision,
            cooldown.cooldown_until,
        )
        .await
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
        self.read_at_key(self.key(provider_account_id)?)
            .await
            .map(|value| {
                value.map(|(credential_revision, cooldown_until)| CredentialCooldown {
                    provider_account_id: provider_account_id.to_owned(),
                    credential_revision,
                    cooldown_until,
                })
            })
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
        self.invalidate_at_key(self.key(provider_account_id)?, through_revision)
            .await
    }
}

impl ProviderCooldownPort for RedisCredentialCooldownRepository {
    fn put_if_later(
        &self,
        cooldown: ProviderCooldown,
    ) -> futures::future::BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let record = CredentialCooldown {
                provider_account_id: cooldown.account_id().as_str().to_owned(),
                credential_revision: Revision::new(cooldown.credential_revision().get())
                    .map_err(|_| provider_invalid("encode credential cooldown"))?,
                cooldown_until: cooldown.until().into(),
            };
            CredentialCooldownRepository::cache_credential_cooldown(self, &record)
                .await
                .map_err(|_| provider_unavailable("cache credential cooldown"))
        })
    }

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async move {
            CredentialCooldownRepository::read_credential_cooldown(self, account_id.as_str())
                .await
                .map_err(|_| provider_unavailable("read credential cooldown"))?
                .map(|record| {
                    let account_id = ProviderAccountId::new(record.provider_account_id)
                        .map_err(|_| provider_invalid("decode credential cooldown"))?;
                    let revision = CredentialRevision::new(record.credential_revision.get())
                        .map_err(|_| provider_invalid("decode credential cooldown"))?;
                    Ok(ProviderCooldown::new(
                        account_id,
                        revision,
                        record.cooldown_until.into(),
                    ))
                })
                .transpose()
        })
    }

    fn clear<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        through_revision: CredentialRevision,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let revision = Revision::new(through_revision.get())
                .map_err(|_| provider_invalid("encode credential cooldown revision"))?;
            CredentialCooldownRepository::invalidate_credential_cooldown(
                self,
                account_id.as_str(),
                revision,
            )
            .await
            .map_err(|_| provider_unavailable("clear credential cooldown"))
        })
    }

    fn put_scoped_if_later(
        &self,
        cooldown: ProviderScopedCooldown,
    ) -> futures::future::BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let revision = Revision::new(cooldown.credential_revision().get())
                .map_err(|_| provider_invalid("encode scoped credential cooldown"))?;
            self.cache_at_key(
                self.scoped_key(cooldown.account_id().as_str(), cooldown.scope())
                    .map_err(|_| provider_invalid("encode scoped credential cooldown"))?,
                revision,
                cooldown.until().into(),
            )
            .await
            .map_err(|_| provider_unavailable("cache scoped credential cooldown"))
        })
    }

    fn read_scoped<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        scope: &'a ProviderCooldownScope,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>>
    {
        Box::pin(async move {
            self.read_at_key(
                self.scoped_key(account_id.as_str(), scope)
                    .map_err(|_| provider_invalid("encode scoped credential cooldown"))?,
            )
            .await
            .map_err(|_| provider_unavailable("read scoped credential cooldown"))?
            .map(|(revision, until)| {
                Ok(ProviderScopedCooldown::new(
                    account_id.clone(),
                    CredentialRevision::new(revision.get())
                        .map_err(|_| provider_invalid("decode scoped credential cooldown"))?,
                    scope.clone(),
                    until.into(),
                ))
            })
            .transpose()
        })
    }

    fn clear_scoped<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        scope: &'a ProviderCooldownScope,
        through_revision: CredentialRevision,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let revision = Revision::new(through_revision.get())
                .map_err(|_| provider_invalid("encode scoped cooldown revision"))?;
            self.invalidate_at_key(
                self.scoped_key(account_id.as_str(), scope)
                    .map_err(|_| provider_invalid("encode scoped credential cooldown"))?,
                revision,
            )
            .await
            .map_err(|_| provider_unavailable("clear scoped credential cooldown"))
        })
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "credential cooldown",
        message: message.to_owned(),
    }
}

fn provider_unavailable(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Unavailable, operation)
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}
