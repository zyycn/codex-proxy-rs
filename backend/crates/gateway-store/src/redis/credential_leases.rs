//! Provider、账号与 OAuth refresh 的 Redis lease/fencing。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use gateway_core::engine::credential::{AccountRuntimeSignals, ProviderAccountId};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest,
    ProviderRefreshCapacityRequest, ProviderSchedulingLeaseRequest, ProviderSchedulingState,
    ProviderStoreError, ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use redis::{Script, aio::ConnectionManager};
use uuid::Uuid;

use crate::{Revision, StoreError, StoreResult, redis_unavailable, require_nonempty};

use super::{MAX_REDIS_EXACT_INTEGER, namespace, resource_fingerprint};

const SIGNAL_TTL_MILLIS: u64 = 24 * 60 * 60 * 1_000;
const PROVIDER_ACCOUNT_LEASE_TTL: Duration = Duration::from_secs(10 * 60);
const OAUTH_REFRESH_LEASE_TTL: Duration = Duration::from_secs(5 * 60);
const OAUTH_REFRESH_CAPACITY_RESOURCE: &str = "oauth-refresh-global";

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

const ADVANCE_SCHEDULING_CURSOR_SCRIPT: &str = r#"
local cursor = redis.call('INCR', KEYS[1])
if cursor > tonumber(ARGV[1]) then
  redis.call('SET', KEYS[1], '1', 'PX', ARGV[2])
  cursor = 1
else
  redis.call('PEXPIRE', KEYS[1], ARGV[2])
end
return tostring(cursor - 1)
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialLeaseScope {
    Provider,
    ProviderAccount,
    OAuthRefreshCapacity,
    OAuthRefresh,
    ProviderTask,
}

impl CredentialLeaseScope {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::ProviderAccount => "account",
            Self::OAuthRefreshCapacity => "refresh-capacity",
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

/// 对任意明确资源施加并发数和启动间隔约束的计数 lease。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialBoundedLeaseRequest {
    pub scope: CredentialLeaseScope,
    pub resource_id: String,
    pub owner_id: String,
    pub max_concurrent: u32,
    pub request_interval: Duration,
    pub ttl: Duration,
}

impl CredentialBoundedLeaseRequest {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty("credential bounded lease", "resource_id", &self.resource_id)?;
        require_nonempty("credential bounded lease", "owner_id", &self.owner_id)?;
        if self.max_concurrent == 0 {
            return Err(invalid("max_concurrent must be positive"));
        }
        supported_duration(self.request_interval, true, "request interval")?;
        supported_duration(self.ttl, false, "lease TTL")?;
        Ok(())
    }

    fn lease_request(&self) -> CredentialLeaseRequest {
        CredentialLeaseRequest {
            scope: self.scope,
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

pub enum CredentialBoundedLeaseAcquisition {
    Acquired(CredentialLeaseGuard),
    Busy { retry_after: Option<Duration> },
}

impl fmt::Debug for CredentialBoundedLeaseAcquisition {
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
    async fn try_acquire_bounded_lease(
        &self,
        request: &CredentialBoundedLeaseRequest,
    ) -> StoreResult<CredentialBoundedLeaseAcquisition>;
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

    /// 原子推进跨进程共享的 Provider 调度游标。
    pub async fn advance_scheduling_cursor(&self, provider_kind: &str) -> StoreResult<u64> {
        require_nonempty("provider scheduling cursor", "provider_kind", provider_kind)?;
        let key = self.scheduling_cursor_key(provider_kind)?;
        let mut connection = self.connection.clone();
        let cursor = Script::new(ADVANCE_SCHEDULING_CURSOR_SCRIPT)
            .key(key)
            .arg(MAX_REDIS_EXACT_INTEGER)
            .arg(SIGNAL_TTL_MILLIS)
            .invoke_async::<String>(&mut connection)
            .await
            .map_err(|_| redis_unavailable("advance provider scheduling cursor"))?;
        cursor
            .parse::<u64>()
            .map_err(|_| invalid("Redis returned an invalid scheduling cursor"))
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

    fn scheduling_cursor_key(&self, provider_kind: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("provider scheduling cursor", provider_kind)?;
        Ok(format!(
            "{}:scheduler:provider:{{{fingerprint}}}:cursor",
            self.namespace
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

/// Store-owned 的通用 Provider lease 能力；具体 Provider 不感知 Redis。
pub(crate) struct RedisProviderLeaseCoordinator {
    repository: RedisCredentialLeaseRepository,
    process_id: String,
    sequence: AtomicU64,
}

impl RedisProviderLeaseCoordinator {
    #[must_use]
    pub(crate) fn new(repository: RedisCredentialLeaseRepository) -> Self {
        Self {
            repository,
            process_id: format!("gateway_{}", Uuid::now_v7().simple()),
            sequence: AtomicU64::new(0),
        }
    }

    fn owner_id(&self, operation: &str) -> String {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        format!("{}:{operation}:{sequence}", self.process_id)
    }

    async fn next_scheduling_cursor(
        &self,
        provider_kind: &ProviderKind,
    ) -> Result<u64, ProviderStoreError> {
        self.repository
            .advance_scheduling_cursor(provider_kind.as_str())
            .await
            .map_err(|_| provider_unavailable("advance scheduling cursor"))
    }

    async fn load_signals(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<BTreeMap<ProviderAccountId, AccountRuntimeSignals>, ProviderStoreError> {
        let ids = accounts
            .iter()
            .map(|account| account.as_str().to_owned())
            .collect::<Vec<_>>();
        let signals = self
            .repository
            .credential_runtime_signals(&ids)
            .await
            .map_err(|_| provider_unavailable("load scheduling signals"))?;
        signals
            .into_iter()
            .map(|signal| {
                let account = ProviderAccountId::new(signal.resource_id).map_err(|_| {
                    ProviderStoreError::new(
                        ProviderStoreErrorKind::InvalidData,
                        "decode scheduling signals",
                    )
                })?;
                Ok((
                    account,
                    AccountRuntimeSignals {
                        in_flight: signal.in_flight,
                        last_started_at: signal.last_started_at.map(Into::into),
                        quota_reset_at: None,
                        quota_remaining_rank: None,
                    },
                ))
            })
            .collect()
    }

    async fn acquire_scheduling(
        &self,
        request: &ProviderSchedulingLeaseRequest,
    ) -> Result<ProviderLeaseAcquisition, ProviderStoreError> {
        let ttl = request
            .deadline()
            .duration_since(SystemTime::now())
            .ok()
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| remaining.min(PROVIDER_ACCOUNT_LEASE_TTL))
            .ok_or_else(|| {
                ProviderStoreError::new(
                    ProviderStoreErrorKind::Unavailable,
                    "acquire expired scheduling lease",
                )
            })?;
        let acquisition = self
            .repository
            .try_acquire_bounded_lease(&CredentialBoundedLeaseRequest {
                scope: CredentialLeaseScope::ProviderAccount,
                resource_id: request.account_id().as_str().to_owned(),
                owner_id: self.owner_id("request"),
                max_concurrent: request.max_concurrent().get(),
                request_interval: request.request_interval(),
                ttl,
            })
            .await
            .map_err(|_| provider_unavailable("acquire scheduling lease"))?;
        Ok(match acquisition {
            CredentialBoundedLeaseAcquisition::Acquired(guard) => {
                ProviderLeaseAcquisition::Acquired(Box::new(guard))
            }
            CredentialBoundedLeaseAcquisition::Busy { retry_after } => {
                ProviderLeaseAcquisition::Busy { retry_after }
            }
        })
    }

    async fn acquire_refresh_capacity(
        &self,
        request: ProviderRefreshCapacityRequest,
    ) -> Result<ProviderLeaseAcquisition, ProviderStoreError> {
        let acquisition = self
            .repository
            .try_acquire_bounded_lease(&CredentialBoundedLeaseRequest {
                scope: CredentialLeaseScope::OAuthRefreshCapacity,
                resource_id: OAUTH_REFRESH_CAPACITY_RESOURCE.to_owned(),
                owner_id: self.owner_id("refresh-capacity"),
                max_concurrent: request.max_concurrent().get(),
                request_interval: Duration::ZERO,
                ttl: OAUTH_REFRESH_LEASE_TTL,
            })
            .await
            .map_err(|_| provider_unavailable("acquire refresh capacity"))?;
        Ok(match acquisition {
            CredentialBoundedLeaseAcquisition::Acquired(guard) => {
                ProviderLeaseAcquisition::Acquired(Box::new(guard))
            }
            CredentialBoundedLeaseAcquisition::Busy { retry_after } => {
                ProviderLeaseAcquisition::Busy { retry_after }
            }
        })
    }
}

impl ProviderLeasePort for RedisProviderLeaseCoordinator {
    fn load_state<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        accounts: &'a [ProviderAccountId],
    ) -> futures::future::BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>> {
        Box::pin(async move {
            let signals = self.load_signals(accounts).await?;
            let round_robin_cursor = self.next_scheduling_cursor(provider_kind).await?;
            Ok(ProviderSchedulingState::new(
                signals,
                None,
                round_robin_cursor,
            ))
        })
    }

    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> futures::future::BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async move {
            match request {
                ProviderLeaseRequest::Scheduling(request) => {
                    self.acquire_scheduling(&request).await
                }
                ProviderLeaseRequest::RefreshCapacity(request) => {
                    self.acquire_refresh_capacity(request).await
                }
                ProviderLeaseRequest::Refresh(request) => {
                    let lease = CredentialLeaseRequest {
                        scope: CredentialLeaseScope::OAuthRefresh,
                        resource_id: request.account_id().as_str().to_owned(),
                        owner_id: self.owner_id("refresh"),
                        ttl: OAUTH_REFRESH_LEASE_TTL,
                    };
                    self.repository
                        .try_acquire_guard(lease)
                        .await
                        .map(|guard| match guard {
                            Some(guard) => ProviderLeaseAcquisition::Acquired(Box::new(guard)),
                            None => ProviderLeaseAcquisition::Busy { retry_after: None },
                        })
                        .map_err(|_| provider_unavailable("acquire refresh lease"))
                }
            }
        })
    }
}

fn provider_unavailable(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Unavailable, operation)
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

    async fn try_acquire_bounded_lease(
        &self,
        request: &CredentialBoundedLeaseRequest,
    ) -> StoreResult<CredentialBoundedLeaseAcquisition> {
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
            Some(grant) => Ok(CredentialBoundedLeaseAcquisition::Acquired(
                CredentialLeaseGuard {
                    repository: self.clone(),
                    request: lease_request,
                    grant: Some(grant),
                },
            )),
            None => Ok(CredentialBoundedLeaseAcquisition::Busy {
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
