//! OAuth 授权临时状态的 Redis 原子持久化。

use std::time::Duration;

use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingFlowPort, OAuthPendingPutOutcome,
    OAuthPendingTakeOutcome, ProviderStoreError, ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use redis::{Script, aio::ConnectionManager};
use sha2::{Digest as _, Sha256};

use super::{MAX_REDIS_EXACT_INTEGER, namespace};
use crate::{StoreError, StoreResult};

const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

const CREATE_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 1 then
  return 0
end
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
local expires_at_ms = now_ms + tonumber(ARGV[3])
local expires_at_epoch_seconds = math.floor(expires_at_ms / 1000)
redis.call('HSET', KEYS[1],
  'owner_fingerprint', ARGV[1],
  'expires_at_epoch_seconds', expires_at_epoch_seconds,
  'provider_payload', ARGV[2])
redis.call('PEXPIREAT', KEYS[1], expires_at_ms)
return 1
"#;

const TAKE_SCRIPT: &str = r#"
local function equal_bytes(left, right)
  local different = math.abs(string.len(left) - string.len(right))
  local length = math.max(string.len(left), string.len(right))
  for index = 1, length do
    local left_byte = string.byte(left, index) or 0
    local right_byte = string.byte(right, index) or 0
    if left_byte ~= right_byte then
      different = different + 1
    end
  end
  return different == 0
end
local owner = redis.call('HGET', KEYS[1], 'owner_fingerprint')
if owner == false then
  return {0, ''}
end
if not equal_bytes(owner, ARGV[1]) then
  return {-1, ''}
end
local payload = redis.call('HGET', KEYS[1], 'provider_payload')
if payload == false then
  return redis.error_reply('OAuth pending flow payload is missing')
end
redis.call('DEL', KEYS[1])
return {1, payload}
"#;

/// OAuth pending flow 的 Redis 原子持久化能力。
#[derive(Clone)]
pub struct RedisOAuthPendingFlowRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisOAuthPendingFlowRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: format!("{}:oauth-pending:v1", namespace(key_namespace)?),
        })
    }

    fn key(&self, provider_kind: &ProviderKind, flow: &OAuthPendingBinding) -> String {
        let fingerprint = provider_scoped_fingerprint(provider_kind, flow);
        format!(
            "{}:{}:{fingerprint}",
            self.namespace,
            provider_kind.as_str()
        )
    }
}

impl OAuthPendingFlowPort for RedisOAuthPendingFlowRepository {
    fn put_if_absent(
        &self,
        flow: NewOAuthPendingFlow,
    ) -> futures::future::BoxFuture<'_, Result<OAuthPendingPutOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = self.key(flow.provider_kind(), flow.flow());
            let owner = provider_scoped_fingerprint(flow.provider_kind(), flow.owner());
            let payload = serde_json::to_vec(&serde_json::Value::Object(
                flow.payload().expose_to_provider().clone(),
            ))
            .map_err(|_| provider_invalid("encode OAuth pending payload"))?;
            if payload.len() > MAX_PAYLOAD_BYTES {
                return Err(provider_invalid("validate OAuth pending payload"));
            }
            let ttl_millis =
                ttl_millis(flow.ttl()).map_err(|_| provider_invalid("encode OAuth pending TTL"))?;
            let mut connection = self.connection.clone();
            let stored = Script::new(CREATE_SCRIPT)
                .key(key)
                .arg(owner)
                .arg(payload)
                .arg(ttl_millis)
                .invoke_async::<i64>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("create OAuth pending flow"))?;
            Ok(if stored == 1 {
                OAuthPendingPutOutcome::Stored
            } else {
                OAuthPendingPutOutcome::AlreadyExists
            })
        })
    }

    fn take_if_owner<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a OAuthPendingBinding,
        owner: &'a OAuthPendingBinding,
    ) -> futures::future::BoxFuture<'a, Result<OAuthPendingTakeOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = self.key(provider_kind, flow);
            let owner = provider_scoped_fingerprint(provider_kind, owner);
            let mut connection = self.connection.clone();
            let (status, payload): (i64, Vec<u8>) = Script::new(TAKE_SCRIPT)
                .key(key)
                .arg(owner)
                .invoke_async(&mut connection)
                .await
                .map_err(|_| provider_unavailable("take OAuth pending flow"))?;
            match status {
                0 => Ok(OAuthPendingTakeOutcome::NotFound),
                -1 => Ok(OAuthPendingTakeOutcome::OwnerMismatch),
                1 => {
                    let value: serde_json::Value = serde_json::from_slice(&payload)
                        .map_err(|_| provider_invalid("decode OAuth pending payload"))?;
                    let serde_json::Value::Object(fields) = value else {
                        return Err(provider_invalid("decode OAuth pending payload"));
                    };
                    Ok(OAuthPendingTakeOutcome::Taken(
                        gateway_core::engine::credential::OpaqueProviderData::new(fields),
                    ))
                }
                _ => Err(provider_invalid("decode OAuth pending outcome")),
            }
        })
    }
}

fn provider_scoped_fingerprint(
    provider_kind: &ProviderKind,
    binding: &OAuthPendingBinding,
) -> String {
    let mut digest = Sha256::new();
    digest.update(provider_kind.as_str().as_bytes());
    digest.update([0]);
    digest.update(binding.expose_to_store().as_bytes());
    hex::encode(digest.finalize())
}

fn ttl_millis(ttl: Duration) -> StoreResult<u64> {
    u64::try_from(ttl.as_millis())
        .ok()
        .filter(|value| *value > 0 && *value <= MAX_REDIS_EXACT_INTEGER)
        .ok_or_else(|| StoreError::InvalidData {
            entity: "OAuth pending flow",
            message: "TTL is outside the supported range".to_owned(),
        })
}

fn provider_unavailable(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Unavailable, operation)
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}
