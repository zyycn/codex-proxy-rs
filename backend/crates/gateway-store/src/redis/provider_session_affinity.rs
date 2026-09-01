//! Provider 会话亲和性的可丢失 Redis 映射。

use std::time::Duration;

use gateway_core::account::ProviderAccountId;
use gateway_core::provider_ports::{
    ProviderSessionAffinityKey, ProviderSessionAffinityPort, ProviderStoreError,
    ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use redis::aio::ConnectionManager;

use crate::StoreResult;

use super::{namespace, resource_fingerprint};

const MAX_SESSION_AFFINITY_TTL: Duration = Duration::from_secs(24 * 60 * 60);

const CLAIM_OR_LOAD_SCRIPT: &str = r#"
local current = redis.call('GET', KEYS[1])
if current then
  return current
end
redis.call('PSETEX', KEYS[1], tonumber(ARGV[2]), ARGV[1])
return ARGV[1]
"#;

const COMPARE_AND_BIND_SCRIPT: &str = r#"
local current = redis.call('GET', KEYS[1])
if not current or current == ARGV[1] then
  redis.call('PSETEX', KEYS[1], tonumber(ARGV[3]), ARGV[2])
  return ARGV[2]
end
return current
"#;

#[derive(Clone)]
pub struct RedisProviderSessionAffinityRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisProviderSessionAffinityRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    fn key(
        &self,
        provider_kind: &ProviderKind,
        affinity_key: &ProviderSessionAffinityKey,
    ) -> Result<String, ProviderStoreError> {
        let scope = format!(
            "{}\0{}",
            provider_kind.as_str(),
            affinity_key.expose_to_store()
        );
        let fingerprint = resource_fingerprint("provider session affinity", &scope)
            .map_err(|_| provider_invalid("encode provider session affinity key"))?;
        Ok(format!(
            "{}:scheduler:affinity:{{{fingerprint}}}",
            self.namespace
        ))
    }
}

impl ProviderSessionAffinityPort for RedisProviderSessionAffinityRepository {
    fn load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderAccountId>, ProviderStoreError>> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let account_id = redis::cmd("GET")
                .arg(self.key(provider_kind, key)?)
                .query_async::<Option<String>>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("load provider session affinity"))?;
            account_id
                .map(|account_id| {
                    ProviderAccountId::new(account_id)
                        .map_err(|_| provider_invalid("decode provider session affinity"))
                })
                .transpose()
        })
    }

    fn bind<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        account_id: &'a ProviderAccountId,
        ttl: Duration,
    ) -> futures::future::BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            let ttl_millis = session_affinity_ttl_millis(ttl)?;
            let mut connection = self.connection.clone();
            redis::cmd("PSETEX")
                .arg(self.key(provider_kind, key)?)
                .arg(ttl_millis)
                .arg(account_id.as_str())
                .query_async::<()>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("bind provider session affinity"))
        })
    }

    fn claim_or_load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        candidate_account_id: &'a ProviderAccountId,
        ttl: Duration,
    ) -> futures::future::BoxFuture<'a, Result<ProviderAccountId, ProviderStoreError>> {
        Box::pin(async move {
            let ttl_millis = session_affinity_ttl_millis(ttl)?;
            let mut connection = self.connection.clone();
            let effective_account_id = redis::Script::new(CLAIM_OR_LOAD_SCRIPT)
                .key(self.key(provider_kind, key)?)
                .arg(candidate_account_id.as_str())
                .arg(ttl_millis)
                .invoke_async::<String>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("claim provider session affinity"))?;
            decode_account_id(effective_account_id)
        })
    }

    fn compare_and_bind<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        expected_account_id: &'a ProviderAccountId,
        replacement_account_id: &'a ProviderAccountId,
        ttl: Duration,
    ) -> futures::future::BoxFuture<'a, Result<ProviderAccountId, ProviderStoreError>> {
        Box::pin(async move {
            let ttl_millis = session_affinity_ttl_millis(ttl)?;
            let mut connection = self.connection.clone();
            let effective_account_id = redis::Script::new(COMPARE_AND_BIND_SCRIPT)
                .key(self.key(provider_kind, key)?)
                .arg(expected_account_id.as_str())
                .arg(replacement_account_id.as_str())
                .arg(ttl_millis)
                .invoke_async::<String>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("compare provider session affinity"))?;
            decode_account_id(effective_account_id)
        })
    }

    fn clear<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            redis::cmd("DEL")
                .arg(self.key(provider_kind, key)?)
                .query_async::<u64>(&mut connection)
                .await
                .map(|removed| removed > 0)
                .map_err(|_| provider_unavailable("clear provider session affinity"))
        })
    }
}

fn session_affinity_ttl_millis(ttl: Duration) -> Result<u64, ProviderStoreError> {
    if ttl.is_zero() || ttl > MAX_SESSION_AFFINITY_TTL {
        return Err(provider_invalid("validate provider session affinity TTL"));
    }
    u64::try_from(ttl.as_millis())
        .map_err(|_| provider_invalid("validate provider session affinity TTL"))
}

fn decode_account_id(account_id: String) -> Result<ProviderAccountId, ProviderStoreError> {
    ProviderAccountId::new(account_id)
        .map_err(|_| provider_invalid("decode provider session affinity"))
}

fn provider_unavailable(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Unavailable, operation)
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}
