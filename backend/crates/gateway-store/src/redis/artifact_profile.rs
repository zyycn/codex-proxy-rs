//! Provider 官方制品画像的单键 Redis cache。

use std::time::{Duration, SystemTime};

use gateway_core::engine::credential::OpaqueProviderData;
use gateway_core::provider_ports::{
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderStoreError,
    ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use redis::{Script, aio::ConnectionManager};

use crate::StoreResult;

use super::{MAX_REDIS_EXACT_INTEGER, namespace};

const MAX_PROFILE_BYTES: usize = 16 * 1024;

const REPLACE_SCRIPT: &str = r#"
local current_sequence = redis.call('HGET', KEYS[1], 'artifact_sequence')
if current_sequence then
  local current_number = tonumber(current_sequence)
  local candidate_number = tonumber(ARGV[1])
  if current_number > candidate_number then return 0 end
  if current_number == candidate_number then
    local current_profile = redis.call('HGET', KEYS[1], 'profile')
    if current_profile ~= ARGV[3] then return -1 end
  end
end
redis.call('HSET', KEYS[1],
  'artifact_sequence', ARGV[1],
  'verified_at_ms', ARGV[2],
  'profile', ARGV[3])
redis.call('PEXPIRE', KEYS[1], ARGV[4])
return 1
"#;

/// 每个 Provider 只占一个覆盖写 key；TTL 负责进程长期停机后的最终清理。
#[derive(Clone)]
pub struct RedisProviderArtifactProfileRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisProviderArtifactProfileRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    fn key(&self, provider_kind: &ProviderKind) -> String {
        format!(
            "{}:provider:{}:artifact-profile:v1",
            self.namespace,
            provider_kind.as_str()
        )
    }
}

impl ProviderArtifactProfileCachePort for RedisProviderArtifactProfileRepository {
    fn replace_if_newer(
        &self,
        profile: ProviderArtifactProfile,
        ttl: Duration,
    ) -> futures::future::BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let artifact_sequence = profile.artifact_sequence();
            if artifact_sequence == 0 || artifact_sequence > MAX_REDIS_EXACT_INTEGER {
                return Err(provider_invalid("validate artifact profile sequence"));
            }
            let ttl_ms = exact_positive_millis(ttl, "validate artifact profile TTL")?;
            let verified_at_ms = profile
                .verified_at()
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .and_then(|duration| u64::try_from(duration.as_millis()).ok())
                .filter(|millis| *millis <= MAX_REDIS_EXACT_INTEGER)
                .ok_or_else(|| provider_invalid("validate artifact profile timestamp"))?;
            let payload = serde_json::to_vec(&serde_json::Value::Object(
                profile.profile().expose_to_provider().clone(),
            ))
            .map_err(|_| provider_invalid("encode artifact profile"))?;
            if payload.len() > MAX_PROFILE_BYTES {
                return Err(provider_invalid("validate artifact profile size"));
            }

            let mut connection = self.connection.clone();
            let outcome = Script::new(REPLACE_SCRIPT)
                .key(self.key(profile.provider_kind()))
                .arg(artifact_sequence)
                .arg(verified_at_ms)
                .arg(payload)
                .arg(ttl_ms)
                .invoke_async::<i64>(&mut connection)
                .await
                .map_err(|_| provider_unavailable("replace artifact profile"))?;
            match outcome {
                1 => Ok(true),
                0 => Ok(false),
                -1 => Err(ProviderStoreError::new(
                    ProviderStoreErrorKind::Conflict,
                    "replace artifact profile",
                )),
                _ => Err(provider_invalid("decode artifact profile write outcome")),
            }
        })
    }

    fn read<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderArtifactProfile>, ProviderStoreError>>
    {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let values: (Option<u64>, Option<u64>, Option<Vec<u8>>) = redis::cmd("HMGET")
                .arg(self.key(provider_kind))
                .arg("artifact_sequence")
                .arg("verified_at_ms")
                .arg("profile")
                .query_async(&mut connection)
                .await
                .map_err(|_| provider_unavailable("read artifact profile"))?;
            let (artifact_sequence, verified_at_ms, payload) = values;
            if artifact_sequence.is_none() && verified_at_ms.is_none() && payload.is_none() {
                return Ok(None);
            }
            let (Some(artifact_sequence), Some(verified_at_ms), Some(payload)) =
                (artifact_sequence, verified_at_ms, payload)
            else {
                return Err(provider_invalid("decode artifact profile"));
            };
            if artifact_sequence == 0 || artifact_sequence > MAX_REDIS_EXACT_INTEGER {
                return Err(provider_invalid("decode artifact profile sequence"));
            }
            if payload.len() > MAX_PROFILE_BYTES {
                return Err(provider_invalid("decode artifact profile size"));
            }
            let serde_json::Value::Object(fields) = serde_json::from_slice(&payload)
                .map_err(|_| provider_invalid("decode artifact profile"))?
            else {
                return Err(provider_invalid("decode artifact profile"));
            };
            let verified_at = SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_millis(verified_at_ms))
                .ok_or_else(|| provider_invalid("decode artifact profile timestamp"))?;
            Ok(Some(ProviderArtifactProfile::new(
                provider_kind.clone(),
                artifact_sequence,
                verified_at,
                OpaqueProviderData::new(fields),
            )))
        })
    }
}

fn exact_positive_millis(
    duration: Duration,
    operation: &'static str,
) -> Result<u64, ProviderStoreError> {
    u64::try_from(duration.as_millis())
        .ok()
        .filter(|millis| *millis > 0 && *millis <= MAX_REDIS_EXACT_INTEGER)
        .ok_or_else(|| provider_invalid(operation))
}

fn provider_unavailable(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Unavailable, operation)
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}
