//! Provider instance、账号与 OAuth refresh 的 Redis lease/fencing。

use std::{fmt, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use redis::{Script, aio::ConnectionManager};
use uuid::Uuid;

use crate::{Revision, StoreError, StoreResult, redis_unavailable, require_nonempty};

use super::{MAX_REDIS_EXACT_INTEGER, namespace, resource_fingerprint};

const SIGNAL_TTL_MILLIS: u64 = 24 * 60 * 60 * 1_000;

const ACQUIRE_SCRIPT: &str = r#"
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)

local in_flight = redis.call('ZCARD', KEYS[1])
local max_concurrent = tonumber(ARGV[4])
local interval_ms = tonumber(ARGV[5])
local retry_ms = 0

if in_flight >= max_concurrent then
  local earliest = redis.call('ZRANGE', KEYS[1], 0, 0, 'WITHSCORES')
  if #earliest == 2 then
    retry_ms = math.max(retry_ms, math.ceil(tonumber(earliest[2]) - now_ms))
  end
end

local last_started = tonumber(redis.call('GET', KEYS[3]) or '0')
if last_started > 0 and now_ms - last_started < interval_ms then
  retry_ms = math.max(retry_ms, interval_ms - (now_ms - last_started))
end

if retry_ms > 0 then
  return {0, '0', '0', tostring(math.max(1, retry_ms))}
end

local fence = redis.call('INCR', KEYS[2])
if fence > 9007199254740991 then
  return redis.error_reply('credential lease fencing token exceeds exact Lua integer range')
end
local expires_at = now_ms + tonumber(ARGV[3])
if expires_at > 9007199254740991 then
  return redis.error_reply('credential lease expiry exceeds exact Lua integer range')
end
local member = ARGV[1] .. '|' .. ARGV[2] .. '|' .. tostring(fence)
redis.call('ZADD', KEYS[1], expires_at, member)

local active_ttl = redis.call('PTTL', KEYS[1])
if active_ttl < tonumber(ARGV[3]) then
  redis.call('PEXPIRE', KEYS[1], ARGV[3])
end
redis.call('SET', KEYS[3], tostring(now_ms), 'PX', ARGV[6])
local fence_ttl = redis.call('PTTL', KEYS[2])
if fence_ttl < tonumber(ARGV[6]) then
  redis.call('PEXPIRE', KEYS[2], ARGV[6])
end
return {1, tostring(fence), tostring(expires_at), '0'}
"#;

const RENEW_SCRIPT: &str = r#"
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
if redis.call('ZSCORE', KEYS[1], ARGV[1]) == false then
  return {0, '0'}
end
local expires_at = now_ms + tonumber(ARGV[2])
if expires_at > 9007199254740991 then
  return redis.error_reply('credential lease expiry exceeds exact Lua integer range')
end
redis.call('ZADD', KEYS[1], expires_at, ARGV[1])
local active_ttl = redis.call('PTTL', KEYS[1])
if active_ttl < tonumber(ARGV[2]) then
  redis.call('PEXPIRE', KEYS[1], ARGV[2])
end
return {1, tostring(expires_at)}
"#;

const RELEASE_SCRIPT: &str = r#"
local removed = redis.call('ZREM', KEYS[1], ARGV[1])
if redis.call('ZCARD', KEYS[1]) == 0 then
  redis.call('DEL', KEYS[1])
end
return removed
"#;

const SIGNAL_SCRIPT: &str = r#"
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
local in_flight = redis.call('ZCARD', KEYS[1])
local last_started = redis.call('GET', KEYS[2]) or '0'
return {tostring(in_flight), tostring(last_started)}
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialLeaseScope {
    ProviderInstance,
    ProviderAccount,
    OAuthRefresh,
    ProviderTask,
}

impl CredentialLeaseScope {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderInstance => "instance",
            Self::ProviderAccount => "account",
            Self::OAuthRefresh => "refresh",
            Self::ProviderTask => "task",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialLeaseRequest {
    pub scope: CredentialLeaseScope,
    pub resource_id: String,
    pub owner_id: String,
    pub ttl: Duration,
}

impl CredentialLeaseRequest {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty("credential lease", "resource_id", &self.resource_id)?;
        require_nonempty("credential lease", "owner_id", &self.owner_id)?;
        supported_duration(self.ttl, false, "lease TTL")?;
        Ok(())
    }
}

/// Provider account 的计数 lease 请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSchedulingLeaseRequest {
    pub resource_id: String,
    pub owner_id: String,
    pub max_concurrent: u32,
    pub request_interval: Duration,
    pub ttl: Duration,
}

impl CredentialSchedulingLeaseRequest {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(
            "credential scheduling lease",
            "resource_id",
            &self.resource_id,
        )?;
        require_nonempty("credential scheduling lease", "owner_id", &self.owner_id)?;
        if self.max_concurrent == 0 {
            return Err(invalid("max_concurrent must be positive"));
        }
        supported_duration(self.request_interval, true, "request interval")?;
        supported_duration(self.ttl, false, "lease TTL")?;
        Ok(())
    }

    fn lease_request(&self) -> CredentialLeaseRequest {
        CredentialLeaseRequest {
            scope: CredentialLeaseScope::ProviderAccount,
            resource_id: self.resource_id.clone(),
            owner_id: self.owner_id.clone(),
            ttl: self.ttl,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialLeaseGrant {
    pub lease_id: String,
    pub fencing_token: Revision,
    pub expires_at: DateTime<Utc>,
}

/// Redis 可丢失的账号调度信号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRuntimeSignal {
    pub resource_id: String,
    pub in_flight: u32,
    pub last_started_at: Option<DateTime<Utc>>,
}

pub enum CredentialSchedulingLeaseAcquisition {
    Acquired(CredentialLeaseGuard),
    Busy { retry_after: Option<Duration> },
}

impl fmt::Debug for CredentialSchedulingLeaseAcquisition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acquired(_) => formatter.write_str("Acquired([LEASE_GUARD])"),
            Self::Busy { retry_after } => formatter
                .debug_struct("Busy")
                .field("retry_after", retry_after)
                .finish(),
        }
    }
}

/// Drop 时在当前 Tokio runtime 上尽力释放；进程崩溃由 Redis TTL 回收。
pub struct CredentialLeaseGuard {
    repository: RedisCredentialLeaseRepository,
    request: CredentialLeaseRequest,
    grant: Option<CredentialLeaseGrant>,
}

impl CredentialLeaseGuard {
    #[must_use]
    pub fn grant(&self) -> Option<&CredentialLeaseGrant> {
        self.grant.as_ref()
    }

    pub async fn release(mut self) -> StoreResult<bool> {
        let Some(grant) = self.grant.take() else {
            return Ok(false);
        };
        self.repository
            .release_credential_lease(&self.request, &grant)
            .await
    }
}

impl fmt::Debug for CredentialLeaseGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialLeaseGuard")
            .field("scope", &self.request.scope)
            .field("resource_id", &"[FINGERPRINTED]")
            .field("owner_id", &"[FINGERPRINTED]")
            .field("grant", &self.grant)
            .finish()
    }
}

impl Drop for CredentialLeaseGuard {
    fn drop(&mut self) {
        let Some(grant) = self.grant.take() else {
            return;
        };
        let repository = self.repository.clone();
        let request = self.request.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn(async move {
                let _ = repository.release_credential_lease(&request, &grant).await;
            }));
        }
    }
}

#[async_trait]
pub trait CredentialLeaseRepository: Send + Sync {
    async fn acquire_credential_lease(
        &self,
        request: &CredentialLeaseRequest,
    ) -> StoreResult<Option<CredentialLeaseGrant>>;
    async fn renew_credential_lease(
        &self,
        request: &CredentialLeaseRequest,
        grant: &CredentialLeaseGrant,
    ) -> StoreResult<Option<CredentialLeaseGrant>>;
    async fn release_credential_lease(
        &self,
        request: &CredentialLeaseRequest,
        grant: &CredentialLeaseGrant,
    ) -> StoreResult<bool>;
    async fn credential_runtime_signals(
        &self,
        resource_ids: &[String],
    ) -> StoreResult<Vec<CredentialRuntimeSignal>>;
    async fn try_acquire_scheduling_lease(
        &self,
        request: &CredentialSchedulingLeaseRequest,
    ) -> StoreResult<CredentialSchedulingLeaseAcquisition>;
}

#[derive(Clone)]
pub struct RedisCredentialLeaseRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisCredentialLeaseRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    /// 获取带 Drop 释放语义的通用 lease guard，供 Provider refresh/task 组合器使用。
    pub async fn try_acquire_guard(
        &self,
        request: CredentialLeaseRequest,
    ) -> StoreResult<Option<CredentialLeaseGuard>> {
        let grant = self.acquire_credential_lease(&request).await?;
        Ok(grant.map(|grant| CredentialLeaseGuard {
            repository: self.clone(),
            request,
            grant: Some(grant),
        }))
    }

    fn keys(&self, request: &CredentialLeaseRequest) -> StoreResult<[String; 3]> {
        let fingerprint = resource_fingerprint("credential lease", &request.resource_id)?;
        let tag = format!("{{{fingerprint}}}");
        let prefix = format!("{}:lease:{}:{tag}", self.namespace, request.scope.as_str());
        Ok([
            format!("{prefix}:active"),
            format!("{prefix}:fence"),
            format!("{prefix}:last-started"),
        ])
    }

    fn lease_member(
        request: &CredentialLeaseRequest,
        grant: &CredentialLeaseGrant,
    ) -> StoreResult<String> {
        let owner = resource_fingerprint("credential lease owner", &request.owner_id)?;
        Ok(format!(
            "{owner}|{}|{}",
            grant.lease_id,
            grant.fencing_token.get()
        ))
    }

    async fn acquire_with_limits(
        &self,
        request: &CredentialLeaseRequest,
        max_concurrent: u32,
        request_interval: Duration,
    ) -> StoreResult<LeaseAttempt> {
        request.validate()?;
        if max_concurrent == 0 {
            return Err(invalid("max_concurrent must be positive"));
        }
        let keys = self.keys(request)?;
        let lease_id = Uuid::now_v7().to_string();
        let owner = resource_fingerprint("credential lease owner", &request.owner_id)?;
        let ttl_millis = duration_millis(request.ttl)?;
        let interval_millis = duration_millis(request_interval)?;
        let signal_ttl = SIGNAL_TTL_MILLIS.max(ttl_millis).max(interval_millis);
        let mut connection = self.connection.clone();
        let (acquired, fence, expires_at, retry_after): (i64, String, String, String) =
            Script::new(ACQUIRE_SCRIPT)
                .key(&keys[0])
                .key(&keys[1])
                .key(&keys[2])
                .arg(owner)
                .arg(&lease_id)
                .arg(ttl_millis)
                .arg(max_concurrent)
                .arg(interval_millis)
                .arg(signal_ttl)
                .invoke_async(&mut connection)
                .await
                .map_err(|_| redis_unavailable("acquire credential lease"))?;
        if acquired == 0 {
            return Ok(LeaseAttempt {
                grant: None,
                retry_after: Some(duration(&retry_after)?),
            });
        }
        Ok(LeaseAttempt {
            grant: Some(grant(lease_id, &fence, &expires_at)?),
            retry_after: None,
        })
    }

    async fn load_signal(&self, resource_id: String) -> StoreResult<CredentialRuntimeSignal> {
        let request = CredentialLeaseRequest {
            scope: CredentialLeaseScope::ProviderAccount,
            resource_id: resource_id.clone(),
            owner_id: "signal-reader".to_owned(),
            ttl: Duration::from_secs(1),
        };
        let keys = self.keys(&request)?;
        let mut connection = self.connection.clone();
        let (in_flight, last_started): (String, String) = Script::new(SIGNAL_SCRIPT)
            .key(&keys[0])
            .key(&keys[2])
            .invoke_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("load credential runtime signal"))?;
        let in_flight = in_flight
            .parse::<u32>()
            .map_err(|_| invalid("Redis returned an invalid in-flight count"))?;
        let last_started_at = if last_started == "0" {
            None
        } else {
            Some(timestamp(&last_started)?)
        };
        Ok(CredentialRuntimeSignal {
            resource_id,
            in_flight,
            last_started_at,
        })
    }
}

#[async_trait]
impl CredentialLeaseRepository for RedisCredentialLeaseRepository {
    async fn acquire_credential_lease(
        &self,
        request: &CredentialLeaseRequest,
    ) -> StoreResult<Option<CredentialLeaseGrant>> {
        self.acquire_with_limits(request, 1, Duration::ZERO)
            .await
            .map(|attempt| attempt.grant)
    }

    async fn renew_credential_lease(
        &self,
        request: &CredentialLeaseRequest,
        grant: &CredentialLeaseGrant,
    ) -> StoreResult<Option<CredentialLeaseGrant>> {
        request.validate()?;
        let keys = self.keys(request)?;
        let member = Self::lease_member(request, grant)?;
        let mut connection = self.connection.clone();
        let (renewed, expires_at): (i64, String) = Script::new(RENEW_SCRIPT)
            .key(&keys[0])
            .arg(member)
            .arg(duration_millis(request.ttl)?)
            .invoke_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("renew credential lease"))?;
        if renewed == 0 {
            return Ok(None);
        }
        Ok(Some(CredentialLeaseGrant {
            lease_id: grant.lease_id.clone(),
            fencing_token: grant.fencing_token,
            expires_at: timestamp(&expires_at)?,
        }))
    }

    async fn release_credential_lease(
        &self,
        request: &CredentialLeaseRequest,
        grant: &CredentialLeaseGrant,
    ) -> StoreResult<bool> {
        request.validate()?;
        let keys = self.keys(request)?;
        let member = Self::lease_member(request, grant)?;
        let mut connection = self.connection.clone();
        let released = Script::new(RELEASE_SCRIPT)
            .key(&keys[0])
            .arg(member)
            .invoke_async::<i64>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("release credential lease"))?;
        Ok(released == 1)
    }

    async fn credential_runtime_signals(
        &self,
        resource_ids: &[String],
    ) -> StoreResult<Vec<CredentialRuntimeSignal>> {
        for resource_id in resource_ids {
            require_nonempty("credential runtime signal", "resource_id", resource_id)?;
        }
        let signals = join_all(
            resource_ids
                .iter()
                .cloned()
                .map(|resource_id| self.load_signal(resource_id)),
        )
        .await;
        signals.into_iter().collect()
    }

    async fn try_acquire_scheduling_lease(
        &self,
        request: &CredentialSchedulingLeaseRequest,
    ) -> StoreResult<CredentialSchedulingLeaseAcquisition> {
        request.validate()?;
        let lease_request = request.lease_request();
        let attempt = self
            .acquire_with_limits(
                &lease_request,
                request.max_concurrent,
                request.request_interval,
            )
            .await?;
        match attempt.grant {
            Some(grant) => Ok(CredentialSchedulingLeaseAcquisition::Acquired(
                CredentialLeaseGuard {
                    repository: self.clone(),
                    request: lease_request,
                    grant: Some(grant),
                },
            )),
            None => Ok(CredentialSchedulingLeaseAcquisition::Busy {
                retry_after: attempt.retry_after,
            }),
        }
    }
}

struct LeaseAttempt {
    grant: Option<CredentialLeaseGrant>,
    retry_after: Option<Duration>,
}

fn grant(lease_id: String, fence: &str, expires_at: &str) -> StoreResult<CredentialLeaseGrant> {
    let fence = fence
        .parse::<u64>()
        .map_err(|_| invalid("Redis returned an invalid fencing token"))?;
    Ok(CredentialLeaseGrant {
        lease_id,
        fencing_token: Revision::new(fence)?,
        expires_at: timestamp(expires_at)?,
    })
}

fn timestamp(value: &str) -> StoreResult<DateTime<Utc>> {
    let milliseconds = value
        .parse::<i64>()
        .map_err(|_| invalid("Redis returned an invalid timestamp"))?;
    DateTime::from_timestamp_millis(milliseconds)
        .ok_or_else(|| invalid("Redis returned an out-of-range timestamp"))
}

fn duration(value: &str) -> StoreResult<Duration> {
    value
        .parse::<u64>()
        .map(Duration::from_millis)
        .map_err(|_| invalid("Redis returned an invalid retry interval"))
}

fn supported_duration(value: Duration, allow_zero: bool, field: &'static str) -> StoreResult<()> {
    let milliseconds = value.as_millis();
    if (!allow_zero && milliseconds == 0) || milliseconds > u128::from(MAX_REDIS_EXACT_INTEGER) {
        return Err(invalid(&format!("{field} is outside the supported range")));
    }
    Ok(())
}

fn duration_millis(value: Duration) -> StoreResult<u64> {
    supported_duration(value, true, "duration")?;
    u64::try_from(value.as_millis()).map_err(|_| invalid("duration is too large"))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "credential lease",
        message: message.to_owned(),
    }
}
