//! 请求调度用的可丢失 Provider 级 cooldown Redis 存储

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::model::accounts::AccountRuntimeSnapshot;
use gateway_core::{
    account::{CredentialRevision, ProviderAccountId},
    provider_ports::{
        ProviderCooldown, ProviderCooldownKind, ProviderCooldownPort, ProviderCooldownScope,
        ProviderScopedCooldown, ProviderStoreError, ProviderStoreErrorKind,
    },
};
use redis::{Script, aio::ConnectionManager};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::{Revision, StoreError, StoreResult, redis_unavailable, require_nonempty};

use super::{namespace, resource_fingerprint};

const WRITE_SCRIPT: &str = r#"
local current = tonumber(redis.call('HGET', KEYS[1], 'revision') or '0')
local incoming = tonumber(ARGV[1])
local incoming_until = tonumber(ARGV[2])
if current > incoming then return 0 end
local current_until = tonumber(redis.call('HGET', KEYS[1], 'until_ms') or '0')
local current_kind = redis.call('HGET', KEYS[1], 'kind') or 'rate_limit'
if current == incoming then
  if current_kind ~= 'rate_limit' and ARGV[4] == 'rate_limit' then return 0 end
  if current_kind == ARGV[4] and current_until >= incoming_until then return 0 end
  incoming_until = math.max(current_until, incoming_until)
end
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
if incoming_until <= now_ms and ARGV[4] ~= 'capacity_freeze_probe' then return 0 end
redis.call('HSET', KEYS[1], 'revision', ARGV[1], 'until_ms', incoming_until, 'kind', ARGV[4], 'generation', ARGV[5])
if ARGV[4] == 'capacity_freeze_probe' then
  redis.call('PERSIST', KEYS[1])
else
  redis.call('PEXPIRE', KEYS[1], incoming_until - now_ms + 60000)
end
if #KEYS > 1 then redis.call('ZADD', KEYS[2], incoming_until, ARGV[3]) end
return 1
"#;

const READ_SCRIPT: &str = r#"
local revision = redis.call('HGET', KEYS[1], 'revision')
local until_ms = redis.call('HGET', KEYS[1], 'until_ms')
local kind = redis.call('HGET', KEYS[1], 'kind') or 'rate_limit'
local clock = redis.call('TIME')
local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
if revision == false or until_ms == false or (kind ~= 'capacity_freeze_probe' and tonumber(until_ms) <= now_ms) then
  redis.call('DEL', KEYS[1])
  if #KEYS > 1 then redis.call('ZREM', KEYS[2], ARGV[1]) end
  return {0, '0', '0', 'rate_limit', ''}
end
return {1, revision, until_ms, kind, redis.call('HGET', KEYS[1], 'generation') or ''}
"#;

const INVALIDATE_SCRIPT: &str = r#"
local current = tonumber(redis.call('HGET', KEYS[1], 'revision') or '0')
if current > tonumber(ARGV[1]) then return 0 end
redis.call('DEL', KEYS[1])
if #KEYS > 1 then redis.call('ZREM', KEYS[2], ARGV[2]) end
return 1
"#;

// 普通推理成功只能清除临时限流和未形成冻结的证据，判断与删除必须原子执行
const SUCCESS_SCRIPT: &str = r#"
local current = tonumber(redis.call('HGET', KEYS[1], 'revision') or '0')
local kind = redis.call('HGET', KEYS[1], 'kind') or 'rate_limit'
if current > tonumber(ARGV[1]) or kind ~= 'rate_limit' then return 0 end
redis.call('DEL', KEYS[1], KEYS[3], KEYS[4])
redis.call('ZREM', KEYS[2], ARGV[2])
return 1
"#;

// 探测结果只能修改读到的这一代冻结；删除后重建同 revision 的冻结也不匹配
const FINISH_FREEZE_SCRIPT: &str = r#"
if redis.call('HGET', KEYS[1], 'generation') ~= ARGV[2]
  or redis.call('HGET', KEYS[1], 'revision') ~= ARGV[1] then return 0 end
local kind = redis.call('HGET', KEYS[1], 'kind')
if kind ~= 'capacity_freeze' and kind ~= 'capacity_freeze_probe' then return 0 end
if ARGV[4] == '' then
  redis.call('DEL', KEYS[1], KEYS[3], KEYS[4])
  redis.call('ZREM', KEYS[2], ARGV[3])
else
  local until_ms = math.max(tonumber(redis.call('HGET', KEYS[1], 'until_ms')), tonumber(ARGV[4]))
  redis.call('HSET', KEYS[1], 'until_ms', until_ms, 'generation', ARGV[5])
  redis.call('ZADD', KEYS[2], until_ms, ARGV[3])
  if kind == 'capacity_freeze_probe' then
    redis.call('PERSIST', KEYS[1])
  else
    local clock = redis.call('TIME')
    local now_ms = (tonumber(clock[1]) * 1000) + math.floor(tonumber(clock[2]) / 1000)
    redis.call('PEXPIRE', KEYS[1], math.max(1, until_ms - now_ms + 60000))
  end
end
return 1
"#;

// 每次容量失败都顺延窗口 TTL（与刷新退避计数同语义），并把本次观测到的
// 在途并发并入峰值证据；峰值与计数共享同一窗口生命周期
const RECORD_CAPACITY_FAILURE_SCRIPT: &str = r#"
local count = redis.call('INCR', KEYS[1])
local ttl_ms = tonumber(ARGV[1])
redis.call('PEXPIRE', KEYS[1], ttl_ms)
local in_flight = tonumber(ARGV[2])
if in_flight > 0 then
  local peak = tonumber(redis.call('GET', KEYS[2]) or '0')
  if in_flight > peak then
    redis.call('SET', KEYS[2], in_flight, 'PX', ttl_ms)
  else
    redis.call('PEXPIRE', KEYS[2], ttl_ms)
  end
end
return count
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialCooldown {
    pub provider_account_id: String,
    pub credential_revision: Revision,
    pub cooldown_until: DateTime<Utc>,
    pub kind: ProviderCooldownKind,
}

#[async_trait]
pub trait CredentialCooldownRepository: Send + Sync {
    async fn cache_credential_cooldown(&self, cooldown: &CredentialCooldown) -> StoreResult<bool>;
    async fn read_credential_cooldown(
        &self,
        provider_account_id: &str,
    ) -> StoreResult<Option<CredentialCooldown>>;
    async fn invalidate_credential_cooldown(
        &self,
        provider_account_id: &str,
        through_revision: Revision,
    ) -> StoreResult<bool>;
    /// 删除账号时清除该账号全部 account/model scope cooldown key
    async fn delete_account_cooldowns(&self, provider_account_id: &str) -> StoreResult<bool>;
}

#[derive(Clone)]
pub struct RedisCredentialCooldownRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisCredentialCooldownRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    fn key(&self, provider_account_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("credential cooldown", provider_account_id)?;
        Ok(format!("{}:account:{fingerprint}:cooldown", self.namespace))
    }

    fn active_index_key(&self) -> String {
        format!("{}:account:active-cooldowns", self.namespace)
    }

    fn capacity_failures_key(&self, provider_account_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("credential cooldown", provider_account_id)?;
        Ok(format!(
            "{}:account:{fingerprint}:capacity-failures",
            self.namespace
        ))
    }

    fn capacity_peak_key(&self, provider_account_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("credential cooldown", provider_account_id)?;
        Ok(format!(
            "{}:account:{fingerprint}:capacity-peak-inflight",
            self.namespace
        ))
    }

    fn scoped_key(
        &self,
        provider_account_id: &str,
        scope: &ProviderCooldownScope,
    ) -> StoreResult<String> {
        let account_fingerprint = resource_fingerprint("credential cooldown", provider_account_id)?;
        let scope_fingerprint = resource_fingerprint("credential cooldown scope", scope.value())?;
        Ok(format!(
            "{}:account:{account_fingerprint}:cooldown:{}:{scope_fingerprint}",
            self.namespace,
            scope.kind(),
        ))
    }

    async fn cache_at_key(
        &self,
        key: String,
        credential_revision: Revision,
        cooldown_until: DateTime<Utc>,
        kind: ProviderCooldownKind,
        index_member: Option<&str>,
    ) -> StoreResult<bool> {
        let until_ms = cooldown_until.timestamp_millis();
        if until_ms <= 0 {
            return Err(invalid("cooldown expiry must be positive"));
        }
        let mut connection = self.connection.clone();
        let written = if let Some(index_member) = index_member {
            Script::new(WRITE_SCRIPT)
                .key(key)
                .key(self.active_index_key())
                .arg(credential_revision.get())
                .arg(until_ms)
                .arg(index_member)
                .arg(kind.as_str())
                .arg(uuid::Uuid::new_v4().to_string())
                .invoke_async::<i64>(&mut connection)
                .await
        } else {
            Script::new(WRITE_SCRIPT)
                .key(key)
                .arg(credential_revision.get())
                .arg(until_ms)
                .arg("")
                .arg(kind.as_str())
                .arg(uuid::Uuid::new_v4().to_string())
                .invoke_async::<i64>(&mut connection)
                .await
        }
        .map_err(|_| redis_unavailable("cache credential cooldown"))?;
        Ok(written == 1)
    }

    async fn read_at_key(
        &self,
        key: String,
        index_member: Option<&str>,
    ) -> StoreResult<Option<(Revision, DateTime<Utc>, ProviderCooldownKind, String)>> {
        let mut connection = self.connection.clone();
        let result = if let Some(index_member) = index_member {
            Script::new(READ_SCRIPT)
                .key(key)
                .key(self.active_index_key())
                .arg(index_member)
                .invoke_async(&mut connection)
                .await
        } else {
            Script::new(READ_SCRIPT)
                .key(key)
                .arg("")
                .invoke_async(&mut connection)
                .await
        };
        let (present, revision, until_ms, kind, generation): (i64, String, String, String, String) =
            result.map_err(|_| redis_unavailable("read credential cooldown"))?;
        if present == 0 {
            return Ok(None);
        }
        let revision = revision
            .parse::<u64>()
            .map_err(|_| invalid("cached cooldown revision is invalid"))?;
        let until_ms = until_ms
            .parse::<i64>()
            .map_err(|_| invalid("cached cooldown expiry is invalid"))?;
        let cooldown_until = DateTime::from_timestamp_millis(until_ms)
            .ok_or_else(|| invalid("cached cooldown expiry is invalid"))?;
        let kind = ProviderCooldownKind::parse(&kind).unwrap_or(ProviderCooldownKind::RateLimit);
        Ok(Some((
            Revision::new(revision)?,
            cooldown_until,
            kind,
            generation,
        )))
    }

    async fn invalidate_at_key(
        &self,
        key: String,
        through_revision: Revision,
        index_member: Option<&str>,
    ) -> StoreResult<bool> {
        let mut connection = self.connection.clone();
        let removed = if let Some(index_member) = index_member {
            Script::new(INVALIDATE_SCRIPT)
                .key(key)
                .key(self.active_index_key())
                .arg(through_revision.get())
                .arg(index_member)
                .invoke_async::<i64>(&mut connection)
                .await
        } else {
            Script::new(INVALIDATE_SCRIPT)
                .key(key)
                .arg(through_revision.get())
                .arg("")
                .invoke_async::<i64>(&mut connection)
                .await
        }
        .map_err(|_| redis_unavailable("invalidate credential cooldown"))?;
        Ok(removed == 1)
    }

    async fn indexed_accounts(&self) -> StoreResult<Vec<String>> {
        let mut connection = self.connection.clone();
        redis::cmd("ZRANGE")
            .arg(self.active_index_key())
            .arg(0)
            .arg(-1)
            .query_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("list indexed cooldowns"))
    }

    pub(crate) async fn active_cooldowns(&self) -> StoreResult<AccountRuntimeSnapshot> {
        let mut cooldown = BTreeMap::new();
        for account_id in self.indexed_accounts().await? {
            if let Some((_, until, kind, _)) = self
                .read_at_key(self.key(&account_id)?, Some(&account_id))
                .await?
            {
                cooldown.insert(
                    account_id,
                    gateway_core::account::AccountCooldown {
                        until: until.into(),
                        kind,
                    },
                );
            }
        }
        Ok(AccountRuntimeSnapshot {
            cooldown,
            in_flight: None,
        })
    }

    /// 包含已到探测时间但尚未确认恢复的冻结；到期不能从 worker 工作集中移除
    pub(crate) async fn active_freezes(
        &self,
    ) -> StoreResult<BTreeMap<String, gateway_admin::model::accounts::AccountFreeze>> {
        let mut freezes = BTreeMap::new();
        for account_id in self.indexed_accounts().await? {
            if let Some((revision, until, kind, generation)) = self
                .read_at_key(self.key(&account_id)?, Some(&account_id))
                .await?
                && kind.is_capacity_freeze()
            {
                freezes.insert(
                    account_id,
                    gateway_admin::model::accounts::AccountFreeze {
                        credential_revision: gateway_admin::model::Revision::new(revision.get())
                            .map_err(|_| invalid("freeze revision"))?,
                        until,
                        generation,
                        requires_probe: kind.requires_probe(),
                    },
                );
            }
        }
        Ok(freezes)
    }

    pub(crate) async fn finish_freeze(
        &self,
        account_id: &str,
        expected: &gateway_admin::model::accounts::AccountFreeze,
        postpone_until: Option<DateTime<Utc>>,
    ) -> StoreResult<bool> {
        let mut connection = self.connection.clone();
        let changed: i64 = Script::new(FINISH_FREEZE_SCRIPT)
            .key(self.key(account_id)?)
            .key(self.active_index_key())
            .key(self.capacity_failures_key(account_id)?)
            .key(self.capacity_peak_key(account_id)?)
            .arg(expected.credential_revision.get())
            .arg(&expected.generation)
            .arg(account_id)
            .arg(
                postpone_until
                    .map(|until| until.timestamp_millis().to_string())
                    .unwrap_or_default(),
            )
            .arg(uuid::Uuid::new_v4().to_string())
            .invoke_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("finish capacity freeze"))?;
        Ok(changed == 1)
    }

    /// 读取窗口内观测到的在途并发峰值；key 随窗口 TTL 过期，无需额外清理
    pub(crate) async fn read_capacity_peak(
        &self,
        provider_account_id: &str,
    ) -> StoreResult<Option<u32>> {
        let mut connection = self.connection.clone();
        let peak: Option<i64> = redis::cmd("GET")
            .arg(self.capacity_peak_key(provider_account_id)?)
            .query_async(&mut connection)
            .await
            .map_err(|_| redis_unavailable("read capacity peak in-flight"))?;
        peak.map(u32::try_from)
            .transpose()
            .map_err(|_| invalid("capacity peak in-flight is invalid"))
    }
}

#[async_trait]
impl CredentialCooldownRepository for RedisCredentialCooldownRepository {
    async fn cache_credential_cooldown(&self, cooldown: &CredentialCooldown) -> StoreResult<bool> {
        require_nonempty(
            "credential cooldown",
            "provider_account_id",
            &cooldown.provider_account_id,
        )?;
        self.cache_at_key(
            self.key(&cooldown.provider_account_id)?,
            cooldown.credential_revision,
            cooldown.cooldown_until,
            cooldown.kind,
            Some(&cooldown.provider_account_id),
        )
        .await
    }

    async fn read_credential_cooldown(
        &self,
        provider_account_id: &str,
    ) -> StoreResult<Option<CredentialCooldown>> {
        require_nonempty(
            "credential cooldown",
            "provider_account_id",
            provider_account_id,
        )?;
        self.read_at_key(self.key(provider_account_id)?, Some(provider_account_id))
            .await
            .map(|value| {
                value.map(
                    |(credential_revision, cooldown_until, kind, _)| CredentialCooldown {
                        provider_account_id: provider_account_id.to_owned(),
                        credential_revision,
                        cooldown_until,
                        kind,
                    },
                )
            })
    }

    async fn invalidate_credential_cooldown(
        &self,
        provider_account_id: &str,
        through_revision: Revision,
    ) -> StoreResult<bool> {
        require_nonempty(
            "credential cooldown",
            "provider_account_id",
            provider_account_id,
        )?;
        self.invalidate_at_key(
            self.key(provider_account_id)?,
            through_revision,
            Some(provider_account_id),
        )
        .await
    }

    async fn delete_account_cooldowns(&self, provider_account_id: &str) -> StoreResult<bool> {
        require_nonempty(
            "credential cooldown",
            "provider_account_id",
            provider_account_id,
        )?;
        // 账号删除：清除该账号的 account key 与全部 model-scoped key
        // 用 SCAN 精确匹配命名空间内该账号前缀，避免 KEYS 阻塞
        let mut connection = self.connection.clone();
        let mut keys = vec![
            self.key(provider_account_id)?,
            self.capacity_failures_key(provider_account_id)?,
            self.capacity_peak_key(provider_account_id)?,
        ];
        let pattern = format!(
            "{}:account:{}:cooldown:*",
            self.namespace,
            resource_fingerprint("credential cooldown", provider_account_id)?
        );
        let mut cursor = 0_i64;
        loop {
            let (next, found): (i64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut connection)
                .await
                .map_err(|_| redis_unavailable("scan account cooldown keys"))?;
            keys.extend(found);
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        // 删除与索引移除同属一个原子边界；否则新冻结可能在两步之间写入后丢失索引
        let removed: i64 = Script::new(
            r#"
            local removed = 0
            for i = 2, #KEYS do removed = removed + redis.call('DEL', KEYS[i]) end
            return removed + redis.call('ZREM', KEYS[1], ARGV[1])
        "#,
        )
        .key(self.active_index_key())
        .key(keys)
        .arg(provider_account_id)
        .invoke_async(&mut connection)
        .await
        .map_err(|_| redis_unavailable("delete account cooldowns and index"))?;
        Ok(removed > 0)
    }
}

impl ProviderCooldownPort for RedisCredentialCooldownRepository {
    fn put_if_later(
        &self,
        cooldown: ProviderCooldown,
    ) -> futures::future::BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let record = CredentialCooldown {
                provider_account_id: cooldown.account_id().as_str().to_owned(),
                credential_revision: Revision::new(cooldown.credential_revision().get())
                    .map_err(|_| provider_invalid("encode credential cooldown"))?,
                cooldown_until: cooldown.until().into(),
                kind: cooldown.kind(),
            };
            CredentialCooldownRepository::cache_credential_cooldown(self, &record)
                .await
                .map_err(|_| provider_unavailable("cache credential cooldown"))
        })
    }

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async move {
            CredentialCooldownRepository::read_credential_cooldown(self, account_id.as_str())
                .await
                .map_err(|_| provider_unavailable("read credential cooldown"))?
                .map(|record| {
                    let account_id = ProviderAccountId::new(record.provider_account_id)
                        .map_err(|_| provider_invalid("decode credential cooldown"))?;
                    let revision = CredentialRevision::new(record.credential_revision.get())
                        .map_err(|_| provider_invalid("decode credential cooldown"))?;
                    Ok(ProviderCooldown::new_with_kind(
                        account_id,
                        revision,
                        record.cooldown_until.into(),
                        record.kind,
                    ))
                })
                .transpose()
        })
    }

    fn clear<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        through_revision: CredentialRevision,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let revision = Revision::new(through_revision.get())
                .map_err(|_| provider_invalid("encode credential cooldown revision"))?;
            CredentialCooldownRepository::invalidate_credential_cooldown(
                self,
                account_id.as_str(),
                revision,
            )
            .await
            .map_err(|_| provider_unavailable("clear credential cooldown"))
        })
    }

    fn put_scoped_if_later(
        &self,
        cooldown: ProviderScopedCooldown,
    ) -> futures::future::BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let revision = Revision::new(cooldown.credential_revision().get())
                .map_err(|_| provider_invalid("encode scoped credential cooldown"))?;
            self.cache_at_key(
                self.scoped_key(cooldown.account_id().as_str(), cooldown.scope())
                    .map_err(|_| provider_invalid("encode scoped credential cooldown"))?,
                revision,
                cooldown.until().into(),
                ProviderCooldownKind::RateLimit,
                None,
            )
            .await
            .map_err(|_| provider_unavailable("cache scoped credential cooldown"))
        })
    }

    fn read_scoped<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        scope: &'a ProviderCooldownScope,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>>
    {
        Box::pin(async move {
            self.read_at_key(
                self.scoped_key(account_id.as_str(), scope)
                    .map_err(|_| provider_invalid("encode scoped credential cooldown"))?,
                None,
            )
            .await
            .map_err(|_| provider_unavailable("read scoped credential cooldown"))?
            .map(|(revision, until, _, _)| {
                Ok(ProviderScopedCooldown::new(
                    account_id.clone(),
                    CredentialRevision::new(revision.get())
                        .map_err(|_| provider_invalid("decode scoped credential cooldown"))?,
                    scope.clone(),
                    until.into(),
                ))
            })
            .transpose()
        })
    }

    fn clear_scoped<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        scope: &'a ProviderCooldownScope,
        through_revision: CredentialRevision,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let revision = Revision::new(through_revision.get())
                .map_err(|_| provider_invalid("encode scoped cooldown revision"))?;
            self.invalidate_at_key(
                self.scoped_key(account_id.as_str(), scope)
                    .map_err(|_| provider_invalid("encode scoped credential cooldown"))?,
                revision,
                None,
            )
            .await
            .map_err(|_| provider_unavailable("clear scoped credential cooldown"))
        })
    }

    fn clear_all<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            self.delete_account_cooldowns(account_id.as_str())
                .await
                .map_err(|_| provider_unavailable("clear all credential cooldowns"))
        })
    }

    fn record_capacity_failure<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        window: Duration,
        in_flight: u32,
    ) -> futures::future::BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let window_ms = u64::try_from(window.as_millis())
                .map_err(|_| provider_invalid("encode capacity failure window"))?;
            let count: i64 = Script::new(RECORD_CAPACITY_FAILURE_SCRIPT)
                .key(
                    self.capacity_failures_key(account_id.as_str())
                        .map_err(|_| provider_invalid("encode capacity failure key"))?,
                )
                .key(
                    self.capacity_peak_key(account_id.as_str())
                        .map_err(|_| provider_invalid("encode capacity peak key"))?,
                )
                .arg(i64::try_from(window_ms).unwrap_or(i64::MAX))
                .arg(i64::from(in_flight))
                .invoke_async(&mut connection)
                .await
                .map_err(|_| provider_unavailable("record capacity failure"))?;
            u32::try_from(count).map_err(|_| provider_invalid("decode capacity failure count"))
        })
    }

    fn clear_after_success<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        through_revision: CredentialRevision,
    ) -> futures::future::BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let _: i64 = Script::new(SUCCESS_SCRIPT)
                .key(
                    self.key(account_id.as_str())
                        .map_err(|_| provider_invalid("cooldown key"))?,
                )
                .key(self.active_index_key())
                .key(
                    self.capacity_failures_key(account_id.as_str())
                        .map_err(|_| provider_invalid("capacity count key"))?,
                )
                .key(
                    self.capacity_peak_key(account_id.as_str())
                        .map_err(|_| provider_invalid("capacity peak key"))?,
                )
                .arg(through_revision.get())
                .arg(account_id.as_str())
                .invoke_async(&mut connection)
                .await
                .map_err(|_| provider_unavailable("clear cooldown after success"))?;
            Ok(())
        })
    }

    fn capacity_peak_in_flight<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<Option<u32>, ProviderStoreError>> {
        Box::pin(async move {
            self.read_capacity_peak(account_id.as_str())
                .await
                .map_err(|_| provider_unavailable("read capacity peak in-flight"))
        })
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "credential cooldown",
        message: message.to_owned(),
    }
}

fn provider_unavailable(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Unavailable, operation)
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}
