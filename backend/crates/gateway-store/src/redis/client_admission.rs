//! Client API Key 的 Redis RPM/并发原子准入与热状态恢复。

use std::{collections::HashSet, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::engine::admission::{
    ClientAdmissionDecision as CoreAdmissionDecision, ClientAdmissionError as CoreAdmissionError,
    ClientAdmissionPort, ClientAdmissionRecovery as CoreAdmissionRecovery,
    ClientAdmissionRejection as CoreAdmissionRejection,
    ClientAdmissionRequest as CoreAdmissionRequest,
    ClientAdmissionRestoreResult as CoreAdmissionRestoreResult,
};
use redis::{Script, aio::ConnectionManager};

use crate::{StoreError, StoreResult, redis_unavailable, require_nonempty};

use super::{MAX_REDIS_EXACT_INTEGER, namespace, resource_fingerprint};

const ADMIT_SCRIPT: &str = r#"
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
local cutoff = now_ms - 60000
local lease_ttl_ms = tonumber(ARGV[2])
if now_ms + lease_ttl_ms > tonumber(ARGV[5]) then return 3 end
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', cutoff)

if tonumber(ARGV[3]) > 0 and redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[3]) then
  return 2
end
if tonumber(ARGV[4]) > 0 and redis.call('ZCARD', KEYS[2]) >= tonumber(ARGV[4]) then
  return 1
end

redis.call('ZADD', KEYS[1], now_ms + lease_ttl_ms, ARGV[1])
redis.call('ZADD', KEYS[2], now_ms, ARGV[1])

local function extend_ttl(key, ttl)
  local current = redis.call('PTTL', key)
  if current < ttl then redis.call('PEXPIRE', key, ttl) end
end

local active_tail = redis.call('ZRANGE', KEYS[1], -1, -1, 'WITHSCORES')
local active_ttl = 120000
if #active_tail == 2 then
  active_ttl = math.max(active_ttl, tonumber(active_tail[2]) - now_ms + 60000)
end
extend_ttl(KEYS[1], active_ttl)
extend_ttl(KEYS[2], 120000)
return 0
"#;

const RESTORE_SCRIPT: &str = r#"
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
local cutoff = now_ms - 60000
local recent_count = tonumber(ARGV[1])
local cursor = 2

for _ = 1, recent_count do
  local started_at_ms = tonumber(ARGV[cursor + 1])
  if started_at_ms > now_ms then return {-1, 0, 0} end
  cursor = cursor + 2
end

local running_count = tonumber(ARGV[cursor])
local running_cursor = cursor + 1

redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', cutoff)

local restored_recent = 0
cursor = 2
for _ = 1, recent_count do
  local request_id = ARGV[cursor]
  local started_at_ms = tonumber(ARGV[cursor + 1])
  if started_at_ms > cutoff then
    local inserted = redis.call('ZADD', KEYS[2], 'NX', started_at_ms, request_id)
    if inserted == 1 then
      restored_recent = restored_recent + 1
    end
  end
  cursor = cursor + 2
end

local restored_running = 0
for _ = 1, running_count do
  local request_id = ARGV[running_cursor]
  local expires_at_ms = tonumber(ARGV[running_cursor + 1])
  if expires_at_ms > now_ms then
    restored_running = restored_running
      + redis.call('ZADD', KEYS[1], 'NX', expires_at_ms, request_id)
  end
  running_cursor = running_cursor + 2
end

local function extend_ttl(key, ttl)
  if redis.call('ZCARD', key) == 0 then return end
  local current = redis.call('PTTL', key)
  if current < ttl then redis.call('PEXPIRE', key, ttl) end
end

local active_tail = redis.call('ZRANGE', KEYS[1], -1, -1, 'WITHSCORES')
local active_ttl = 120000
if #active_tail == 2 then
  active_ttl = math.max(active_ttl, tonumber(active_tail[2]) - now_ms + 60000)
end
extend_ttl(KEYS[1], active_ttl)
extend_ttl(KEYS[2], 120000)
return {0, restored_recent, restored_running}
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAdmissionLimits {
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRequest {
    pub model_request_id: String,
    pub client_api_key_ref: String,
    pub lease_ttl: Duration,
    pub limits: ClientAdmissionLimits,
}

impl ClientAdmissionRequest {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(
            "client admission",
            "model_request_id",
            &self.model_request_id,
        )?;
        require_nonempty(
            "client admission",
            "client_api_key_ref",
            &self.client_api_key_ref,
        )?;
        if self.lease_ttl.is_zero() {
            return Err(invalid("lease TTL must be positive"));
        }
        redis_duration_millis(self.lease_ttl)?;
        redis_integer(self.limits.max_concurrency, "maximum concurrency")?;
        redis_integer(self.limits.requests_per_minute, "requests per minute")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRecentRequest {
    pub model_request_id: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRunningRequest {
    pub model_request_id: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRestore {
    pub client_api_key_ref: String,
    pub recent_requests: Vec<ClientAdmissionRecentRequest>,
    pub running_requests: Vec<ClientAdmissionRunningRequest>,
}

impl ClientAdmissionRestore {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(
            "client admission",
            "client_api_key_ref",
            &self.client_api_key_ref,
        )?;
        redis_len(self.recent_requests.len(), "recent request count")?;
        redis_len(self.running_requests.len(), "running request count")?;

        let mut recent_ids = HashSet::with_capacity(self.recent_requests.len());
        for request in &self.recent_requests {
            validate_recovery_request_id(&request.model_request_id)?;
            redis_timestamp_millis(request.started_at, "request start time")?;
            if !recent_ids.insert(request.model_request_id.as_str()) {
                return Err(invalid("recent request IDs must be unique"));
            }
        }

        let mut running_ids = HashSet::with_capacity(self.running_requests.len());
        for request in &self.running_requests {
            validate_recovery_request_id(&request.model_request_id)?;
            redis_timestamp_millis(request.expires_at, "request expiry time")?;
            if !running_ids.insert(request.model_request_id.as_str()) {
                return Err(invalid("running request IDs must be unique"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAdmissionRestoreResult {
    pub restored_recent_requests: u64,
    pub restored_running_requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAdmissionRejection {
    RateLimited,
    ConcurrencyLimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAdmissionDecision {
    Granted,
    Rejected(ClientAdmissionRejection),
}

#[async_trait]
pub trait ClientAdmissionRepository: Send + Sync {
    async fn admit_client_request(
        &self,
        request: &ClientAdmissionRequest,
    ) -> StoreResult<ClientAdmissionDecision>;
    async fn release_client_request(
        &self,
        client_api_key_ref: &str,
        model_request_id: &str,
    ) -> StoreResult<bool>;
    async fn restore_client_admission(
        &self,
        recovery: &ClientAdmissionRestore,
    ) -> StoreResult<ClientAdmissionRestoreResult>;
    async fn clear_client_admission(&self, client_api_key_ref: &str) -> StoreResult<()>;
}

#[derive(Clone)]
pub struct RedisClientAdmissionRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisClientAdmissionRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    fn keys(&self, client_api_key_ref: &str) -> StoreResult<[String; 2]> {
        let fingerprint = resource_fingerprint("client admission", client_api_key_ref)?;
        let tag = format!("{{{fingerprint}}}");
        Ok([
            format!("{}:client:{tag}:active", self.namespace),
            format!("{}:client:{tag}:requests", self.namespace),
        ])
    }
}

#[async_trait]
impl ClientAdmissionRepository for RedisClientAdmissionRepository {
    async fn admit_client_request(
        &self,
        request: &ClientAdmissionRequest,
    ) -> StoreResult<ClientAdmissionDecision> {
        request.validate()?;
        let keys = self.keys(&request.client_api_key_ref)?;
        let lease_ttl_ms = u64::try_from(request.lease_ttl.as_millis())
            .map_err(|_| invalid("lease TTL is too large"))?;
        let mut connection = self.connection.clone();
        let code = Script::new(ADMIT_SCRIPT)
            .key(&keys[0])
            .key(&keys[1])
            .arg(&request.model_request_id)
            .arg(lease_ttl_ms)
            .arg(request.limits.max_concurrency)
            .arg(request.limits.requests_per_minute)
            .arg(MAX_REDIS_EXACT_INTEGER)
            .invoke_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("admit client request"))?;
        match code {
            0 => Ok(ClientAdmissionDecision::Granted),
            1 => Ok(ClientAdmissionDecision::Rejected(
                ClientAdmissionRejection::RateLimited,
            )),
            2 => Ok(ClientAdmissionDecision::Rejected(
                ClientAdmissionRejection::ConcurrencyLimited,
            )),
            3 => Err(invalid("lease expiry is outside the supported range")),
            _ => Err(invalid("Redis returned an unknown admission decision")),
        }
    }

    async fn release_client_request(
        &self,
        client_api_key_ref: &str,
        model_request_id: &str,
    ) -> StoreResult<bool> {
        require_nonempty("client admission", "model_request_id", model_request_id)?;
        let keys = self.keys(client_api_key_ref)?;
        let mut connection = self.connection.clone();
        let removed = redis::cmd("ZREM")
            .arg(&keys[0])
            .arg(model_request_id)
            .query_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("release client request"))?;
        Ok(removed == 1)
    }

    async fn restore_client_admission(
        &self,
        recovery: &ClientAdmissionRestore,
    ) -> StoreResult<ClientAdmissionRestoreResult> {
        recovery.validate()?;
        let keys = self.keys(&recovery.client_api_key_ref)?;
        let script = Script::new(RESTORE_SCRIPT);
        let mut invocation = script.prepare_invoke();
        invocation.key(&keys[0]).key(&keys[1]).arg(redis_len(
            recovery.recent_requests.len(),
            "recent request count",
        )?);
        for request in &recovery.recent_requests {
            invocation
                .arg(&request.model_request_id)
                .arg(redis_timestamp_millis(
                    request.started_at,
                    "request start time",
                )?);
        }
        invocation.arg(redis_len(
            recovery.running_requests.len(),
            "running request count",
        )?);
        for request in &recovery.running_requests {
            invocation
                .arg(&request.model_request_id)
                .arg(redis_timestamp_millis(
                    request.expires_at,
                    "request expiry time",
                )?);
        }

        let mut connection = self.connection.clone();
        let (code, restored_recent_requests, restored_running_requests) = invocation
            .invoke_async::<(i64, u64, u64)>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("restore client admission"))?;
        if code == -1 {
            return Err(invalid("request start time is after Redis server time"));
        }
        if code != 0 {
            return Err(invalid("Redis returned an unknown recovery decision"));
        }
        Ok(ClientAdmissionRestoreResult {
            restored_recent_requests,
            restored_running_requests,
        })
    }

    async fn clear_client_admission(&self, client_api_key_ref: &str) -> StoreResult<()> {
        let keys = self.keys(client_api_key_ref)?;
        let mut connection = self.connection.clone();
        redis::cmd("DEL")
            .arg(&keys)
            .query_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("clear client admission"))?;
        Ok(())
    }
}

impl ClientAdmissionPort for RedisClientAdmissionRepository {
    fn admit(
        &self,
        request: CoreAdmissionRequest,
    ) -> futures::future::BoxFuture<'_, Result<CoreAdmissionDecision, CoreAdmissionError>> {
        Box::pin(async move {
            self.admit_client_request(&ClientAdmissionRequest {
                model_request_id: request.model_request_id.as_str().to_owned(),
                client_api_key_ref: request.client_api_key_id.as_str().to_owned(),
                lease_ttl: request.lease_ttl,
                limits: ClientAdmissionLimits {
                    max_concurrency: request.limits.max_concurrency,
                    requests_per_minute: request.limits.requests_per_minute,
                },
            })
            .await
            .map(|decision| match decision {
                ClientAdmissionDecision::Granted => CoreAdmissionDecision::Granted,
                ClientAdmissionDecision::Rejected(ClientAdmissionRejection::RateLimited) => {
                    CoreAdmissionDecision::Rejected(CoreAdmissionRejection::RateLimited)
                }
                ClientAdmissionDecision::Rejected(ClientAdmissionRejection::ConcurrencyLimited) => {
                    CoreAdmissionDecision::Rejected(CoreAdmissionRejection::ConcurrencyLimited)
                }
            })
            .map_err(|_| CoreAdmissionError)
        })
    }

    fn release<'a>(
        &'a self,
        client_api_key_id: &'a gateway_core::policy::ClientApiKeyId,
        model_request_id: &'a gateway_core::engine::ModelRequestId,
    ) -> futures::future::BoxFuture<'a, Result<bool, CoreAdmissionError>> {
        Box::pin(async move {
            self.release_client_request(client_api_key_id.as_str(), model_request_id.as_str())
                .await
                .map_err(|_| CoreAdmissionError)
        })
    }

    fn restore(
        &self,
        recovery: CoreAdmissionRecovery,
    ) -> futures::future::BoxFuture<'_, Result<CoreAdmissionRestoreResult, CoreAdmissionError>>
    {
        Box::pin(async move {
            self.restore_client_admission(&ClientAdmissionRestore {
                client_api_key_ref: recovery.client_api_key_id.as_str().to_owned(),
                recent_requests: recovery
                    .recent_requests
                    .into_iter()
                    .map(|request| ClientAdmissionRecentRequest {
                        model_request_id: request.model_request_id.as_str().to_owned(),
                        started_at: DateTime::<Utc>::from(request.started_at),
                    })
                    .collect(),
                running_requests: recovery
                    .running_requests
                    .into_iter()
                    .map(|request| ClientAdmissionRunningRequest {
                        model_request_id: request.model_request_id.as_str().to_owned(),
                        expires_at: DateTime::<Utc>::from(request.expires_at),
                    })
                    .collect(),
            })
            .await
            .map(|restored| CoreAdmissionRestoreResult {
                restored_recent_requests: restored.restored_recent_requests,
                restored_running_requests: restored.restored_running_requests,
            })
            .map_err(|_| CoreAdmissionError)
        })
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "client admission",
        message: message.to_owned(),
    }
}

fn validate_recovery_request_id(model_request_id: &str) -> StoreResult<()> {
    require_nonempty("client admission", "model_request_id", model_request_id)
}

fn redis_duration_millis(duration: Duration) -> StoreResult<u64> {
    let milliseconds =
        u64::try_from(duration.as_millis()).map_err(|_| invalid("lease TTL is too large"))?;
    redis_integer(milliseconds, "lease TTL")
}

fn redis_timestamp_millis(timestamp: DateTime<Utc>, field: &str) -> StoreResult<u64> {
    let milliseconds = u64::try_from(timestamp.timestamp_millis()).map_err(|_| invalid(field))?;
    redis_integer(milliseconds, field)
}

fn redis_len(length: usize, field: &str) -> StoreResult<u64> {
    let value = u64::try_from(length).map_err(|_| invalid(field))?;
    redis_integer(value, field)
}

fn redis_integer(value: u64, field: &str) -> StoreResult<u64> {
    if value > MAX_REDIS_EXACT_INTEGER {
        return Err(invalid(&format!(
            "{field} is outside Redis' exact integer range"
        )));
    }
    Ok(value)
}
