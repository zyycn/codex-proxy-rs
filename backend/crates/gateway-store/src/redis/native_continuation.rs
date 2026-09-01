//! Responses continuation 的 Redis 会话亲和记录。
//!
//! 该存储只保存可丢失的 response → Provider/account pin；PostgreSQL 审计记录
//! 不是 continuation 的可用性前提。键只含 response ID 的不可逆指纹；Client Key
//! 所有权写入 payload 并在读取时严格校验，使跨 Key 复用能够 fail closed。
//! Provider 私有状态作为不透明 JSON 保存，Store 不解释其内容。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use gateway_core::account::ProviderAccountId;
use gateway_core::engine::continuation::{
    NativeContinuationPin, NativeContinuationPort, NativeContinuationScope,
    NativeContinuationStoreError, PreviousResponseId,
};
use gateway_core::operation::ProviderSessionState;
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::ProviderKind;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};

use crate::StoreResult;

use super::{namespace, resource_fingerprint};

const CONTINUATION_TTL: Duration = Duration::from_secs(4 * 60 * 60);
const MAX_CONTINUATIONS: usize = 65_536;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_INDEX_CLEANUP_PER_RECORD: usize = 64;

const RECORD_SCRIPT: &str = r#"
local entry_key = KEYS[1]
local index_key = KEYS[2]
local entry_id = ARGV[1]
local payload = ARGV[2]
local score = tonumber(ARGV[3])
local ttl_seconds = tonumber(ARGV[4])
local cutoff = tonumber(ARGV[5])
local max_entries = tonumber(ARGV[6])
local entry_prefix = ARGV[7]
local cleanup_limit = tonumber(ARGV[8])

redis.call('SET', entry_key, payload, 'EX', ttl_seconds)
redis.call('ZADD', index_key, score, entry_id)
redis.call('EXPIRE', index_key, ttl_seconds)

local stale = redis.call('ZRANGEBYSCORE', index_key, '-inf', cutoff, 'LIMIT', 0, cleanup_limit)
for _, stale_id in ipairs(stale) do
  redis.call('UNLINK', entry_prefix .. '{' .. stale_id .. '}')
  redis.call('ZREM', index_key, stale_id)
end

local count = redis.call('ZCARD', index_key)
local excess = count - max_entries
if excess > cleanup_limit then
  excess = cleanup_limit
end
if excess > 0 then
  local oldest = redis.call('ZRANGE', index_key, 0, excess - 1)
  for _, old_id in ipairs(oldest) do
    redis.call('UNLINK', entry_prefix .. '{' .. old_id .. '}')
    redis.call('ZREM', index_key, old_id)
  end
end

return 1
"#;

/// 基于 Redis 的 best-effort 原生响应 continuation 亲和。
#[derive(Clone)]
pub struct RedisNativeContinuationRepository {
    connection: ConnectionManager,
    namespace: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContinuationWire {
    client_api_key_id: String,
    upstream_response_id: String,
    provider: String,
    account: String,
    scope: String,
    #[serde(default)]
    session_state: Option<ProviderSessionState>,
}

impl RedisNativeContinuationRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: format!("{}:response-affinity:v2", namespace(key_namespace)?),
        })
    }

    fn entry_fingerprint(&self, response_id: &str) -> Result<String, NativeContinuationStoreError> {
        resource_fingerprint("native continuation", response_id)
            .map_err(|error| NativeContinuationStoreError::invalid_data(error.to_string()))
    }

    fn entry_key(&self, fingerprint: &str) -> String {
        format!("{}{{{fingerprint}}}", self.entry_prefix())
    }

    fn entry_prefix(&self) -> String {
        format!("{}:entry:", self.namespace)
    }

    fn index_key(&self) -> String {
        format!("{}:global:index", self.namespace)
    }
}

impl NativeContinuationPort for RedisNativeContinuationRepository {
    fn resolve<'a>(
        &'a self,
        client_api_key_id: &'a ClientApiKeyId,
        previous_response_id: &'a PreviousResponseId,
    ) -> BoxFuture<'a, Result<Option<NativeContinuationPin>, NativeContinuationStoreError>> {
        Box::pin(async move {
            if previous_response_id.as_str().is_empty() {
                return Ok(None);
            }
            let fingerprint = self.entry_fingerprint(previous_response_id.as_str())?;
            let mut connection = self.connection.clone();
            let payload = redis::cmd("GET")
                .arg(self.entry_key(&fingerprint))
                .query_async::<Option<String>>(&mut connection)
                .await
                .map_err(|error| {
                    NativeContinuationStoreError::unavailable(format!("redis GET failed: {error}"))
                })?;
            payload
                .map(|payload| decode_pin(client_api_key_id, previous_response_id, &payload))
                .transpose()
        })
    }

    fn record<'a>(
        &'a self,
        pin: NativeContinuationPin,
    ) -> BoxFuture<'a, Result<(), NativeContinuationStoreError>> {
        Box::pin(async move {
            if pin.previous_response_id().as_str().is_empty() {
                return Ok(());
            }
            let fingerprint = self.entry_fingerprint(pin.previous_response_id().as_str())?;
            let payload = encode_pin(&pin)?;
            if payload.len() > MAX_RECORD_BYTES {
                return Err(NativeContinuationStoreError::invalid_data(format!(
                    "pin payload is too large ({} bytes, limit {MAX_RECORD_BYTES})",
                    payload.len()
                )));
            }
            let now_millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| {
                    NativeContinuationStoreError::unavailable("system clock predates Unix epoch")
                })?
                .as_millis();
            let now_millis = u64::try_from(now_millis).map_err(|_| {
                NativeContinuationStoreError::unavailable("system time is outside supported range")
            })?;
            let ttl_millis = u64::try_from(CONTINUATION_TTL.as_millis()).map_err(|_| {
                NativeContinuationStoreError::invalid_data(
                    "continuation TTL is outside supported range",
                )
            })?;
            let cutoff = now_millis.saturating_sub(ttl_millis);
            let ttl_seconds = CONTINUATION_TTL.as_secs();
            let mut connection = self.connection.clone();
            redis::Script::new(RECORD_SCRIPT)
                .key(self.entry_key(&fingerprint))
                .key(self.index_key())
                .arg(fingerprint)
                .arg(payload)
                .arg(now_millis)
                .arg(ttl_seconds)
                .arg(cutoff)
                .arg(MAX_CONTINUATIONS)
                .arg(self.entry_prefix())
                .arg(MAX_INDEX_CLEANUP_PER_RECORD)
                .invoke_async::<i64>(&mut connection)
                .await
                .map_err(|error| {
                    NativeContinuationStoreError::unavailable(format!(
                        "redis affinity script failed: {error}"
                    ))
                })?;
            Ok(())
        })
    }
}

fn encode_pin(pin: &NativeContinuationPin) -> Result<String, NativeContinuationStoreError> {
    if pin
        .session_state()
        .is_some_and(|state| state.provider() != pin.provider().as_str())
    {
        return Err(NativeContinuationStoreError::invalid_data(
            "session state provider does not match pin provider",
        ));
    }
    serde_json::to_string(&ContinuationWire {
        client_api_key_id: pin.client_api_key_id().as_str().to_owned(),
        upstream_response_id: pin.upstream_response_id().as_str().to_owned(),
        provider: pin.provider().as_str().to_owned(),
        account: pin.account().as_str().to_owned(),
        scope: scope_name(pin.scope()).to_owned(),
        session_state: pin.session_state().cloned(),
    })
    .map_err(|error| {
        NativeContinuationStoreError::invalid_data(format!("pin serialization failed: {error}"))
    })
}

fn decode_pin(
    expected_client_api_key_id: &ClientApiKeyId,
    previous_response_id: &PreviousResponseId,
    payload: &str,
) -> Result<NativeContinuationPin, NativeContinuationStoreError> {
    let wire: ContinuationWire = serde_json::from_str(payload)
        .map_err(|error| NativeContinuationStoreError::invalid_data(format!("{error}")))?;
    let provider = ProviderKind::new(wire.provider).map_err(|_| {
        NativeContinuationStoreError::invalid_data("pin payload contains an invalid provider")
    })?;
    let account = ProviderAccountId::new(wire.account).map_err(|_| {
        NativeContinuationStoreError::invalid_data("pin payload contains an invalid account id")
    })?;
    let client_api_key_id = ClientApiKeyId::new(wire.client_api_key_id).map_err(|_| {
        NativeContinuationStoreError::invalid_data(
            "pin payload contains an invalid client API key id",
        )
    })?;
    if client_api_key_id != *expected_client_api_key_id {
        return Err(NativeContinuationStoreError::ownership_mismatch());
    }
    let upstream_response_id = PreviousResponseId::new(wire.upstream_response_id);
    let scope = parse_scope(&wire.scope)?;
    let mut pin = NativeContinuationPin::new(
        previous_response_id.clone(),
        upstream_response_id,
        client_api_key_id,
        provider.clone(),
        account,
    )
    .with_scope(scope);
    if let Some(state) = wire.session_state {
        if state.provider() != provider.as_str() {
            return Err(NativeContinuationStoreError::invalid_data(
                "pin payload session state provider does not match pin provider",
            ));
        }
        pin = pin.with_session_state(state);
    }
    Ok(pin)
}

const fn scope_name(scope: NativeContinuationScope) -> &'static str {
    match scope {
        NativeContinuationScope::Persisted => "persisted",
        NativeContinuationScope::ConnectionLocal => "connection_local",
    }
}

fn parse_scope(scope: &str) -> Result<NativeContinuationScope, NativeContinuationStoreError> {
    match scope {
        "persisted" => Ok(NativeContinuationScope::Persisted),
        "connection_local" => Ok(NativeContinuationScope::ConnectionLocal),
        _ => Err(NativeContinuationStoreError::invalid_data(
            "pin payload contains an unsupported continuation scope",
        )),
    }
}
