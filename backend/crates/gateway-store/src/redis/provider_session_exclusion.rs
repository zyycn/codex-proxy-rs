//! Provider 会话级账号排除的可丢失 Redis 状态。
//!
//! Redis 只保存 Provider 派生的不可逆会话键对应的账号 ID 集合及 revision；
//! 不读取或保存任何 Provider 协议正文。

use std::collections::BTreeSet;
use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::provider_ports::{
    ProviderSessionAffinityKey, ProviderSessionExclusionPort, ProviderSessionExclusions,
    ProviderStoreError, ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::StoreResult;

use super::{namespace, resource_fingerprint};

const MAX_SESSION_EXCLUSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

const RECORD_FAILURE_SCRIPT: &str = r#"
local raw = redis.call('GET', KEYS[1])
local state = { failedAccountIds = {}, revision = '' }
if raw then
  local ok, decoded = pcall(cjson.decode, raw)
  if ok and type(decoded) == 'table' and type(decoded.failedAccountIds) == 'table' then
    state = decoded
  end
end

local account_id = ARGV[1]
local known = false
for _, failed_account_id in ipairs(state.failedAccountIds) do
  if failed_account_id == account_id then
    known = true
    break
  end
end
if not known then
  table.insert(state.failedAccountIds, account_id)
end

state.revision = ARGV[2]
local encoded = cjson.encode(state)
redis.call('SET', KEYS[1], encoded, 'EX', tonumber(ARGV[3]))
return encoded
"#;

const CLEAR_SCRIPT: &str = r#"
local raw = redis.call('GET', KEYS[1])
if not raw then
  return 0
end
local ok, state = pcall(cjson.decode, raw)
if not ok or type(state) ~= 'table' then
  return 0
end
if tostring(state.revision or '') ~= ARGV[1] then
  return 0
end
return redis.call('DEL', KEYS[1])
"#;

#[derive(Clone)]
pub struct RedisProviderSessionExclusionRepository {
    connection: ConnectionManager,
    namespace: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionExclusionWire {
    failed_account_ids: Vec<String>,
    #[serde(default)]
    revision: String,
}

impl RedisProviderSessionExclusionRepository {
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
        let fingerprint = resource_fingerprint("provider session exclusion", &scope)
            .map_err(|_| provider_invalid("encode provider session exclusion key"))?;
        Ok(format!(
            "{}:scheduler:session-exclusion:{{{fingerprint}}}",
            self.namespace
        ))
    }
}

impl ProviderSessionExclusionPort for RedisProviderSessionExclusionRepository {
    fn load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderSessionExclusions>, ProviderStoreError>> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let payload = redis::cmd("GET")
                .arg(self.key(provider_kind, key)?)
                .query_async::<Option<String>>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("load provider session exclusion"))?;
            payload.map(decode_state).transpose()
        })
    }

    fn record_failure<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        account_id: &'a ProviderAccountId,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<ProviderSessionExclusions, ProviderStoreError>> {
        Box::pin(async move {
            if ttl.is_zero() || ttl > MAX_SESSION_EXCLUSION_TTL {
                return Err(provider_invalid("validate provider session exclusion TTL"));
            }
            let ttl_seconds = ttl.as_secs().max(1);
            let mut connection = self.connection.clone();
            let payload = redis::Script::new(RECORD_FAILURE_SCRIPT)
                .key(self.key(provider_kind, key)?)
                .arg(account_id.as_str())
                .arg(Uuid::new_v4().to_string())
                .arg(ttl_seconds)
                .invoke_async::<String>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("record provider session exclusion"))?;
            decode_state(payload)
        })
    }

    fn clear<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        expected_revision: &'a str,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let removed = redis::Script::new(CLEAR_SCRIPT)
                .key(self.key(provider_kind, key)?)
                .arg(expected_revision)
                .invoke_async::<u64>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("clear provider session exclusion"))?;
            Ok(removed > 0)
        })
    }
}

fn decode_state(payload: impl AsRef<str>) -> Result<ProviderSessionExclusions, ProviderStoreError> {
    let state: SessionExclusionWire = serde_json::from_str(payload.as_ref())
        .map_err(|_| provider_invalid("decode provider session exclusion"))?;
    let excluded_accounts = state
        .failed_account_ids
        .into_iter()
        .map(|account_id| {
            ProviderAccountId::new(account_id)
                .map_err(|_| provider_invalid("decode provider session exclusion"))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(ProviderSessionExclusions::new(
        excluded_accounts,
        state.revision,
    ))
}

fn provider_unavailable(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Unavailable, operation)
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}
