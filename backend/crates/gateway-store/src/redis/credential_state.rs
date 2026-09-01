//! Provider credential state 的可重建 Redis cache fence。

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redis::{Script, aio::ConnectionManager};

use gateway_core::account::{
    CredentialRevision, CredentialState, OpaqueProviderData, ProviderAccountId,
};
use gateway_core::provider_ports::{
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderCredentialState,
    ProviderCredentialStatePort, ProviderStoreError, ProviderStoreErrorKind,
};

use crate::{Revision, StoreError, StoreResult, redis_unavailable, require_nonempty};

use super::{namespace, resource_fingerprint};

const WRITE_SCRIPT: &str = r#"
local current = tonumber(redis.call('HGET', KEYS[1], 'revision') or '0')
if current >= tonumber(ARGV[1]) then return 0 end
redis.call('HSET', KEYS[1],
  'revision', ARGV[1],
  'enabled', ARGV[2],
  'credential_state', ARGV[3],
  'observed_at_ms', ARGV[4])
redis.call('PEXPIRE', KEYS[1], ARGV[5])
return 1
"#;

const RECORD_REFRESH_BACKOFF_SCRIPT: &str = r#"
local n = redis.call('INCR', KEYS[1])
redis.call('PEXPIRE', KEYS[1], ARGV[1])
return n
"#;

const MAX_PROVIDER_CATALOG_BYTES: usize = 1024 * 1024;
const CREDENTIAL_STATE_TTL_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStateCache {
    pub provider_account_id: String,
    pub revision: Revision,
    pub enabled: bool,
    pub credential_state: String,
    pub observed_at: DateTime<Utc>,
}

impl CredentialStateCache {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(
            "credential state cache",
            "provider_account_id",
            &self.provider_account_id,
        )?;
        if CredentialState::parse(&self.credential_state).is_none() {
            return Err(invalid("credential_state is not registered"));
        }
        Ok(())
    }
}

#[async_trait]
pub trait CredentialStateRepository: Send + Sync {
    async fn cache_credential_state(
        &self,
        state: &CredentialStateCache,
        ttl_seconds: u64,
    ) -> StoreResult<bool>;
    async fn read_credential_state(
        &self,
        provider_account_id: &str,
    ) -> StoreResult<Option<CredentialStateCache>>;
    async fn clear_credential_state(&self, provider_account_id: &str) -> StoreResult<bool>;
}

/// Provider 目录 cache 的隔离键。具体作用域由对应 Provider 决定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisProviderCatalogCacheKey {
    pub provider_kind: String,
    pub catalog_scope: String,
}

impl RedisProviderCatalogCacheKey {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(
            "provider catalog cache",
            "provider_kind",
            &self.provider_kind,
        )?;
        if !self.provider_kind.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        }) {
            return Err(catalog_invalid("provider_kind must be a stable slug"));
        }
        require_nonempty(
            "provider catalog cache",
            "catalog_scope",
            &self.catalog_scope,
        )
    }
}

#[async_trait]
pub trait ProviderCatalogCacheRepository: Send + Sync {
    async fn replace_provider_catalog(
        &self,
        key: &RedisProviderCatalogCacheKey,
        catalog: &OpaqueProviderData,
        ttl_seconds: u64,
    ) -> StoreResult<()>;

    async fn get_provider_catalog(
        &self,
        key: &RedisProviderCatalogCacheKey,
    ) -> StoreResult<Option<OpaqueProviderData>>;
}

#[derive(Clone)]
pub struct RedisCredentialStateRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisCredentialStateRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    fn key(&self, provider_account_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("credential state cache", provider_account_id)?;
        Ok(format!("{}:account:{fingerprint}:state", self.namespace))
    }

    fn backoff_key(&self, provider_account_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("credential refresh backoff", provider_account_id)?;
        Ok(format!(
            "{}:account:{fingerprint}:refresh-backoff",
            self.namespace
        ))
    }

    fn catalog_key(&self, key: &RedisProviderCatalogCacheKey) -> StoreResult<String> {
        key.validate()?;
        let fingerprint = resource_fingerprint("provider catalog cache", &key.catalog_scope)?;
        Ok(format!(
            "{}:provider:{}:catalog:{{{fingerprint}}}",
            self.namespace, key.provider_kind,
        ))
    }
}

#[async_trait]
impl CredentialStateRepository for RedisCredentialStateRepository {
    async fn cache_credential_state(
        &self,
        state: &CredentialStateCache,
        ttl_seconds: u64,
    ) -> StoreResult<bool> {
        state.validate()?;
        let ttl_ms = ttl_seconds
            .checked_mul(1_000)
            .filter(|ttl| *ttl > 0)
            .ok_or_else(|| invalid("TTL must be positive"))?;
        let mut connection = self.connection.clone();
        let written = Script::new(WRITE_SCRIPT)
            .key(self.key(&state.provider_account_id)?)
            .arg(state.revision.get())
            .arg(u8::from(state.enabled))
            .arg(&state.credential_state)
            .arg(state.observed_at.timestamp_millis())
            .arg(ttl_ms)
            .invoke_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("cache credential state"))?;
        Ok(written == 1)
    }

    async fn read_credential_state(
        &self,
        provider_account_id: &str,
    ) -> StoreResult<Option<CredentialStateCache>> {
        require_nonempty(
            "credential state cache",
            "provider_account_id",
            provider_account_id,
        )?;
        let mut connection = self.connection.clone();
        let values: (Option<u64>, Option<u8>, Option<String>, Option<i64>) = redis::cmd("HMGET")
            .arg(self.key(provider_account_id)?)
            .arg("revision")
            .arg("enabled")
            .arg("credential_state")
            .arg("observed_at_ms")
            .query_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("read credential state"))?;
        let (Some(revision), Some(enabled), Some(credential_state), Some(observed_at_ms)) = values
        else {
            return Ok(None);
        };
        let state = CredentialStateCache {
            provider_account_id: provider_account_id.to_owned(),
            revision: Revision::new(revision)?,
            enabled: enabled == 1,
            credential_state,
            observed_at: DateTime::from_timestamp_millis(observed_at_ms)
                .ok_or_else(|| invalid("observed_at is invalid"))?,
        };
        state.validate()?;
        Ok(Some(state))
    }

    async fn clear_credential_state(&self, provider_account_id: &str) -> StoreResult<bool> {
        require_nonempty(
            "credential state cache",
            "provider_account_id",
            provider_account_id,
        )?;
        let mut connection = self.connection.clone();
        let removed = redis::cmd("DEL")
            .arg(self.key(provider_account_id)?)
            .query_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("clear credential state"))?;
        Ok(removed == 1)
    }
}

#[async_trait]
impl ProviderCatalogCacheRepository for RedisCredentialStateRepository {
    async fn replace_provider_catalog(
        &self,
        key: &RedisProviderCatalogCacheKey,
        catalog: &OpaqueProviderData,
        ttl_seconds: u64,
    ) -> StoreResult<()> {
        if ttl_seconds == 0 {
            return Err(catalog_invalid("TTL must be positive"));
        }
        let ttl_ms = ttl_seconds
            .checked_mul(1_000)
            .ok_or_else(|| catalog_invalid("TTL is too large"))?;
        let payload = serde_json::to_vec(&serde_json::Value::Object(
            catalog.expose_to_provider().clone(),
        ))
        .map_err(|error| catalog_invalid(&error.to_string()))?;
        if payload.len() > MAX_PROVIDER_CATALOG_BYTES {
            return Err(catalog_invalid("catalog exceeds the cache size limit"));
        }
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg(self.catalog_key(key)?)
            .arg(payload)
            .arg("PX")
            .arg(ttl_ms)
            .query_async::<()>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("replace provider catalog cache"))?;
        Ok(())
    }

    async fn get_provider_catalog(
        &self,
        key: &RedisProviderCatalogCacheKey,
    ) -> StoreResult<Option<OpaqueProviderData>> {
        let mut connection = self.connection.clone();
        let payload = redis::cmd("GET")
            .arg(self.catalog_key(key)?)
            .query_async::<Option<Vec<u8>>>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("read provider catalog cache"))?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_slice(&payload)
            .map_err(|error| catalog_invalid(&error.to_string()))?;
        let serde_json::Value::Object(fields) = value else {
            return Err(catalog_invalid("cached catalog must be a JSON object"));
        };
        Ok(Some(OpaqueProviderData::new(fields)))
    }
}

impl ProviderCredentialStatePort for RedisCredentialStateRepository {
    fn replace(
        &self,
        state: ProviderCredentialState,
    ) -> futures::future::BoxFuture<'_, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            let observed_at: DateTime<Utc> = state.observed_at().into();
            let cached = CredentialStateCache {
                provider_account_id: state.account_id().as_str().to_owned(),
                revision: Revision::new(state.credential_revision().get())
                    .map_err(|_| provider_invalid("encode credential state"))?,
                enabled: state.enabled(),
                credential_state: state.credential_state().as_str().to_owned(),
                observed_at,
            };
            CredentialStateRepository::cache_credential_state(
                self,
                &cached,
                CREDENTIAL_STATE_TTL_SECONDS,
            )
            .await
            .map(|_| ())
            .map_err(|_| provider_unavailable("replace credential state"))
        })
    }

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderCredentialState>, ProviderStoreError>>
    {
        Box::pin(async move {
            let cached =
                CredentialStateRepository::read_credential_state(self, account_id.as_str())
                    .await
                    .map_err(|_| provider_unavailable("read credential state"))?;
            cached
                .map(|state| {
                    let account_id = ProviderAccountId::new(state.provider_account_id)
                        .map_err(|_| provider_invalid("decode credential state"))?;
                    let revision = CredentialRevision::new(state.revision.get())
                        .map_err(|_| provider_invalid("decode credential state"))?;
                    let credential_state = CredentialState::parse(&state.credential_state)
                        .ok_or_else(|| provider_invalid("decode credential state"))?;
                    Ok(ProviderCredentialState::new(
                        account_id,
                        revision,
                        state.enabled,
                        credential_state,
                        state.observed_at.into(),
                    ))
                })
                .transpose()
        })
    }

    fn clear<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            CredentialStateRepository::clear_credential_state(self, account_id.as_str())
                .await
                .map_err(|_| provider_unavailable("clear credential state"))
        })
    }

    fn record_refresh_backoff<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        window: Duration,
    ) -> futures::future::BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async move {
            let window_millis = u64::try_from(window.as_millis())
                .ok()
                .filter(|millis| *millis > 0)
                .ok_or_else(|| provider_invalid("record refresh backoff"))?;
            let key = self
                .backoff_key(account_id.as_str())
                .map_err(|_| provider_unavailable("record refresh backoff"))?;
            let mut connection = self.connection.clone();
            let count = Script::new(RECORD_REFRESH_BACKOFF_SCRIPT)
                .key(key)
                .arg(window_millis)
                .invoke_async::<i64>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("record refresh backoff"))?;
            Ok(u32::try_from(count).unwrap_or(u32::MAX))
        })
    }

    fn clear_refresh_backoff<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            let key = self
                .backoff_key(account_id.as_str())
                .map_err(|_| provider_unavailable("clear refresh backoff"))?;
            let mut connection = self.connection.clone();
            redis::cmd("DEL")
                .arg(key)
                .query_async::<i64>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("clear refresh backoff"))?;
            Ok(())
        })
    }
}

impl ProviderCatalogCachePort for RedisCredentialStateRepository {
    fn replace<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
        catalog: &'a OpaqueProviderData,
        ttl: std::time::Duration,
    ) -> futures::future::BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            let ttl_seconds = ttl.as_secs();
            if ttl_seconds == 0 || ttl.subsec_nanos() != 0 {
                return Err(provider_invalid("validate catalog cache TTL"));
            }
            let key = RedisProviderCatalogCacheKey {
                provider_kind: key.provider_kind().as_str().to_owned(),
                catalog_scope: key.scope().as_str().to_owned(),
            };
            ProviderCatalogCacheRepository::replace_provider_catalog(
                self,
                &key,
                catalog,
                ttl_seconds,
            )
            .await
            .map_err(|_| provider_unavailable("replace catalog cache"))
        })
    }

    fn read<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
    ) -> futures::future::BoxFuture<'a, Result<Option<OpaqueProviderData>, ProviderStoreError>>
    {
        Box::pin(async move {
            let key = RedisProviderCatalogCacheKey {
                provider_kind: key.provider_kind().as_str().to_owned(),
                catalog_scope: key.scope().as_str().to_owned(),
            };
            ProviderCatalogCacheRepository::get_provider_catalog(self, &key)
                .await
                .map_err(|_| provider_unavailable("read catalog cache"))
        })
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "credential state cache",
        message: message.to_owned(),
    }
}

fn catalog_invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "provider catalog cache",
        message: message.to_owned(),
    }
}

fn provider_unavailable(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Unavailable, operation)
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}
