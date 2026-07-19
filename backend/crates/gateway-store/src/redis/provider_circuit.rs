//! Provider instance 的可重建 Redis circuit。

use std::num::NonZeroU32;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redis::{Script, aio::ConnectionManager};

use crate::{StoreError, StoreResult, redis_unavailable, require_nonempty};

use super::{namespace, resource_fingerprint};

const FAILURE_SCRIPT: &str = r#"
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
local failures = redis.call('HINCRBY', KEYS[1], 'failures', 1)
local open_until = tonumber(redis.call('HGET', KEYS[1], 'open_until_ms') or '0')
if failures >= tonumber(ARGV[1]) then
  open_until = now_ms + tonumber(ARGV[2])
  redis.call('HSET', KEYS[1], 'open_until_ms', tostring(open_until))
end
redis.call('HSET', KEYS[1], 'observed_at_ms', tostring(now_ms))
redis.call('PEXPIRE', KEYS[1], math.max(86400000, tonumber(ARGV[2]) * 2))
return {tostring(failures), tostring(open_until)}
"#;

const DECISION_SCRIPT: &str = r#"
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
local open_until = tonumber(redis.call('HGET', KEYS[1], 'open_until_ms') or '0')
if open_until > now_ms then return {0, tostring(open_until)} end
return {1, '0'}
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCircuitPolicy {
    pub failure_threshold: NonZeroU32,
    pub open_duration: Duration,
}

impl Default for ProviderCircuitPolicy {
    fn default() -> Self {
        Self {
            failure_threshold: NonZeroU32::new(3).unwrap_or(NonZeroU32::MIN),
            open_duration: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCircuitDecision {
    Allow,
    BlockedUntil(DateTime<Utc>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCircuitObservation {
    pub failure_count: u32,
    pub open_until: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait ProviderCircuitRepository: Send + Sync {
    async fn provider_circuit_decision(
        &self,
        provider_instance_id: &str,
    ) -> StoreResult<ProviderCircuitDecision>;
    async fn observe_provider_failure(
        &self,
        provider_instance_id: &str,
    ) -> StoreResult<ProviderCircuitObservation>;
    async fn observe_provider_success(&self, provider_instance_id: &str) -> StoreResult<()>;
}

#[derive(Clone)]
pub struct RedisProviderCircuitRepository {
    connection: ConnectionManager,
    namespace: String,
    policy: ProviderCircuitPolicy,
}

impl RedisProviderCircuitRepository {
    pub fn new(
        connection: ConnectionManager,
        key_namespace: &str,
        policy: ProviderCircuitPolicy,
    ) -> StoreResult<Self> {
        if policy.open_duration.is_zero() {
            return Err(invalid("open duration must be positive"));
        }
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
            policy,
        })
    }

    fn key(&self, provider_instance_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("provider circuit", provider_instance_id)?;
        Ok(format!("{}:instance:{fingerprint}:circuit", self.namespace))
    }
}

#[async_trait]
impl ProviderCircuitRepository for RedisProviderCircuitRepository {
    async fn provider_circuit_decision(
        &self,
        provider_instance_id: &str,
    ) -> StoreResult<ProviderCircuitDecision> {
        require_nonempty(
            "provider circuit",
            "provider_instance_id",
            provider_instance_id,
        )?;
        let mut connection = self.connection.clone();
        let (allow, until): (i64, String) = Script::new(DECISION_SCRIPT)
            .key(self.key(provider_instance_id)?)
            .invoke_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("read provider circuit"))?;
        if allow == 1 {
            Ok(ProviderCircuitDecision::Allow)
        } else {
            Ok(ProviderCircuitDecision::BlockedUntil(timestamp(&until)?))
        }
    }

    async fn observe_provider_failure(
        &self,
        provider_instance_id: &str,
    ) -> StoreResult<ProviderCircuitObservation> {
        require_nonempty(
            "provider circuit",
            "provider_instance_id",
            provider_instance_id,
        )?;
        let duration_ms = u64::try_from(self.policy.open_duration.as_millis())
            .map_err(|_| invalid("open duration is too large"))?;
        let mut connection = self.connection.clone();
        let (failures, until): (String, String) = Script::new(FAILURE_SCRIPT)
            .key(self.key(provider_instance_id)?)
            .arg(self.policy.failure_threshold.get())
            .arg(duration_ms)
            .invoke_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("observe provider failure"))?;
        let failure_count = failures
            .parse()
            .map_err(|_| invalid("Redis returned an invalid failure count"))?;
        let open_until = if until == "0" {
            None
        } else {
            Some(timestamp(&until)?)
        };
        Ok(ProviderCircuitObservation {
            failure_count,
            open_until,
        })
    }

    async fn observe_provider_success(&self, provider_instance_id: &str) -> StoreResult<()> {
        require_nonempty(
            "provider circuit",
            "provider_instance_id",
            provider_instance_id,
        )?;
        let mut connection = self.connection.clone();
        redis::cmd("DEL")
            .arg(self.key(provider_instance_id)?)
            .query_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("reset provider circuit"))?;
        Ok(())
    }
}

fn timestamp(value: &str) -> StoreResult<DateTime<Utc>> {
    let value = value
        .parse::<i64>()
        .map_err(|_| invalid("Redis returned an invalid circuit timestamp"))?;
    DateTime::from_timestamp_millis(value)
        .ok_or_else(|| invalid("Redis returned an out-of-range circuit timestamp"))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "provider circuit",
        message: message.to_owned(),
    }
}
