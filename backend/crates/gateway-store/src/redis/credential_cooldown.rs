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

use crate::{Revision, StoreError, StoreResult, redis_unavailable, require_nonempty};

use super::{namespace, resource_fingerprint};

pub(super) const COOLDOWN_READ_BATCH_SIZE: usize = 128;

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
    pub(super) connection: ConnectionManager,
    pub(super) namespace: String,
}

impl RedisCredentialCooldownRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    pub(super) fn key(&self, provider_account_id: &str) -> StoreResult<String> {
        let fingerprint = resource_fingerprint("credential cooldown", provider_account_id)?;
        Ok(format!("{}:account:{fingerprint}:cooldown", self.namespace))
    }

    pub(super) fn active_index_key(&self) -> String {
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
        .map_err(|source| redis_unavailable("cache credential cooldown", source))?;
        Ok(written == 1)
    }

    async fn read_at_key(
        &self,
        key: String,
        index_member: Option<&str>,
    ) -> StoreResult<Option<(Revision, DateTime<Utc>)>> {
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
        parse_scoped_cooldown(
            result.map_err(|source| redis_unavailable("read credential cooldown", source))?,
        )
    }

    /// 保留每条命令的错误，脚本失效时只补发尚未执行的 NOSCRIPT 项
    pub(super) async fn read_script_batch(
        &self,
        script: &Script,
        commands: Vec<StoreResult<redis::Cmd>>,
    ) -> Vec<StoreResult<redis::Value>> {
        let mut pipeline = redis::pipe();
        pipeline.ignore_errors();
        for command in commands.iter().filter_map(|command| command.as_ref().ok()) {
            pipeline.add_command(command.clone());
        }
        let mut connection = self.connection.clone();
        let values = pipeline
            .query_async::<Vec<redis::RedisResult<redis::Value>>>(&mut connection)
            .await;
        let values = match values {
            Ok(values) => {
                let mut retry = redis::pipe();
                retry.ignore_errors();
                let mut retry_indices = Vec::new();
                for (index, (command, value)) in pipeline.cmd_iter().zip(&values).enumerate() {
                    if value.as_ref().is_err_and(|error| {
                        error.kind() == redis::ErrorKind::Server(redis::ServerErrorKind::NoScript)
                    }) {
                        retry.add_command(command.clone());
                        retry_indices.push(index);
                    }
                }
                let mut values = values
                    .into_iter()
                    .map(|value| {
                        value.map_err(|source| redis_unavailable("read cooldown batch", source))
                    })
                    .collect::<Vec<_>>();
                if !retry_indices.is_empty() {
                    let retried = async {
                        script.load_async(&mut connection).await?;
                        retry
                            .query_async::<Vec<redis::RedisResult<redis::Value>>>(&mut connection)
                            .await
                    }
                    .await;
                    match retried {
                        Ok(retried) => {
                            for (index, value) in retry_indices.into_iter().zip(retried) {
                                values[index] = value.map_err(|source| {
                                    redis_unavailable("retry cooldown batch", source)
                                });
                            }
                        }
                        Err(source) => {
                            let error = redis_unavailable("retry cooldown batch", source);
                            for index in retry_indices {
                                values[index] = Err(error.clone());
                            }
                        }
                    }
                }
                values
            }
            Err(source) => {
                vec![Err(redis_unavailable("read cooldown batch", source)); pipeline.len()]
            }
        };
        let mut values = values.into_iter();
        commands
            .into_iter()
            .map(|command| {
                command.and_then(|_| {
                    values
                        .next()
                        .unwrap_or_else(|| Err(invalid("missing cooldown batch response")))
                })
            })
            .collect()
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
        .map_err(|source| redis_unavailable("invalidate credential cooldown", source))?;
        Ok(removed == 1)
    }

    async fn all_cooldown_accounts(&self) -> StoreResult<Vec<String>> {
        let (mut accounts, protected): (Vec<String>, Vec<String>) = redis::pipe()
            .cmd("ZRANGE")
            .arg(self.active_index_key())
            .arg(0)
            .arg(-1)
            .cmd("ZRANGE")
            .arg(self.protection_index_key())
            .arg(0)
            .arg(-1)
            .query_async(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("list account cooldowns", e))?;
        accounts.extend(protected);
        accounts.sort_unstable();
        accounts.dedup();
        Ok(accounts)
    }

    pub(crate) async fn active_cooldowns(&self) -> StoreResult<AccountRuntimeSnapshot> {
        self.read_cooldown_accounts(self.all_cooldown_accounts().await?)
            .await
    }

    pub(crate) async fn account_cooldowns(
        &self,
        accounts: &[String],
    ) -> StoreResult<AccountRuntimeSnapshot> {
        if accounts.is_empty() {
            return Ok(AccountRuntimeSnapshot::default());
        }
        // 索引与冷却事实由 Lua 原子维护，只读取范围内实际有状态的账号
        let (cooldowns, protections): (Vec<Option<f64>>, Vec<Option<f64>>) = redis::pipe()
            .cmd("ZMSCORE")
            .arg(self.active_index_key())
            .arg(accounts)
            .cmd("ZMSCORE")
            .arg(self.protection_index_key())
            .arg(accounts)
            .query_async(&mut self.connection.clone())
            .await
            .map_err(|error| redis_unavailable("read scoped account cooldown index", error))?;
        let active = accounts
            .iter()
            .zip(cooldowns)
            .zip(protections)
            .filter(|((_, cooldown), protection)| cooldown.is_some() || protection.is_some())
            .map(|((account, _), _)| account.clone())
            .collect();
        self.read_cooldown_accounts(active).await
    }

    async fn read_cooldown_accounts(
        &self,
        accounts: Vec<String>,
    ) -> StoreResult<AccountRuntimeSnapshot> {
        use futures::{StreamExt as _, TryStreamExt as _};
        let reads = futures::stream::iter(accounts)
            .map(|account| async move {
                self.read_account_runtime(&account)
                    .await
                    .map(|runtime| (account, runtime))
            })
            .buffer_unordered(32)
            .try_collect::<Vec<_>>()
            .await?;
        let mut snapshot = AccountRuntimeSnapshot::default();
        for (account, (cooldown, freeze)) in reads {
            if let Some(cooldown) = cooldown {
                snapshot.cooldown.insert(
                    account.clone(),
                    gateway_core::account::AccountCooldown {
                        until: cooldown.cooldown_until.into(),
                        kind: cooldown.kind,
                    },
                );
            }
            if let Some(freeze) = freeze {
                snapshot
                    .protection_rules
                    .insert(account, freeze.protection_rules);
            }
        }
        Ok(snapshot)
    }

    pub(crate) async fn active_freezes(
        &self,
    ) -> StoreResult<BTreeMap<String, gateway_admin::model::accounts::AccountFreeze>> {
        use futures::{StreamExt as _, TryStreamExt as _};
        let reads = futures::stream::iter(self.all_cooldown_accounts().await?)
            .map(|account| async move {
                self.read_account_runtime(&account)
                    .await
                    .map(|(_, freeze)| (account, freeze))
            })
            .buffer_unordered(32)
            .try_collect::<Vec<_>>()
            .await?;
        Ok(reads
            .into_iter()
            .filter_map(|(account, freeze)| freeze.map(|value| (account, value)))
            .collect())
    }

    pub(crate) async fn finish_freeze(
        &self,
        account_id: &str,
        expected: &gateway_admin::model::accounts::AccountFreeze,
        postpone_until: Option<DateTime<Utc>>,
    ) -> StoreResult<bool> {
        self.finish_protection(account_id, expected, postpone_until)
            .await
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
        self.migrate_protection(&cooldown.provider_account_id)
            .await?;
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
        self.read_effective_cooldown(provider_account_id).await
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
        let protection_cleared = self
            .clear_protection(provider_account_id, through_revision)
            .await?;
        let cooldown_cleared = self
            .invalidate_at_key(
                self.key(provider_account_id)?,
                through_revision,
                Some(provider_account_id),
            )
            .await?;
        Ok(protection_cleared || cooldown_cleared)
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
            self.protection_key(provider_account_id)?,
            self.protection_state_key(provider_account_id)?,
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
                .map_err(|source| redis_unavailable("scan account cooldown keys", source))?;
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
            redis.call('ZREM', ARGV[2], ARGV[1])
            return removed + redis.call('ZREM', KEYS[1], ARGV[1])
        "#,
        )
        .key(self.active_index_key())
        .key(keys)
        .arg(provider_account_id)
        .arg(self.protection_index_key())
        .invoke_async(&mut connection)
        .await
        .map_err(|source| redis_unavailable("delete account cooldowns and index", source))?;
        Ok(removed > 0)
    }
}

impl ProviderCooldownPort for RedisCredentialCooldownRepository {
    fn advance_protection_revision<'a>(
        &'a self,
        account: &'a ProviderAccountId,
        revision: CredentialRevision,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            self.advance_account_protection_revision(account, revision)
                .await
                .map_err(|e| {
                    crate::provider_unavailable("advance protection credential revision", e)
                })
        })
    }

    fn publish_protection_policy<'a>(
        &'a self,
        policy: &'a gateway_core::account::AccountErrorProtection,
        revision: gateway_core::routing::ConfigRevision,
    ) -> futures::future::BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            self.publish_policy(policy, revision)
                .await
                .map_err(|e| crate::provider_unavailable("publish account protection", e))
        })
    }
    fn begin_protection<'a>(
        &'a self,
        account: &'a ProviderAccountId,
        revision: CredentialRevision,
        fingerprint: &'a str,
        snapshot_revision: gateway_core::routing::ConfigRevision,
    ) -> futures::future::BoxFuture<
        'a,
        Result<Option<gateway_core::provider_ports::AccountProtectionGuard>, ProviderStoreError>,
    > {
        Box::pin(async move {
            self.begin_account_protection(account, revision, fingerprint, snapshot_revision)
                .await
                .map_err(|e| crate::provider_unavailable("begin account protection", e))
        })
    }
    fn record_protection_failure<'a>(
        &'a self,
        guard: &'a gateway_core::provider_ports::AccountProtectionGuard,
        policy: &'a gateway_core::account::AccountErrorProtection,
        matched: &'a [gateway_core::account::AccountProtectionMatch],
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            self.protection_failure(guard, policy, matched)
                .await
                .map_err(|e| crate::provider_unavailable("record protection failure", e))
        })
    }
    fn record_protection_success<'a>(
        &'a self,
        guard: &'a gateway_core::provider_ports::AccountProtectionGuard,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            self.protection_success(guard)
                .await
                .map_err(|e| crate::provider_unavailable("record protection success", e))
        })
    }

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
                .map_err(|source| crate::provider_unavailable("cache credential cooldown", source))
        })
    }

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async move {
            CredentialCooldownRepository::read_credential_cooldown(self, account_id.as_str())
                .await
                .map_err(|source| crate::provider_unavailable("read credential cooldown", source))?
                .map(provider_cooldown)
                .transpose()
        })
    }

    fn read_many<'a>(
        &'a self,
        account_ids: &'a [ProviderAccountId],
    ) -> futures::future::BoxFuture<'a, Vec<Result<Option<ProviderCooldown>, ProviderStoreError>>>
    {
        Box::pin(async move {
            self.read_effective_cooldowns(account_ids)
                .await
                .into_iter()
                .map(|result| {
                    result
                        .map_err(|source| {
                            crate::provider_unavailable("read credential cooldown", source)
                        })?
                        .map(provider_cooldown)
                        .transpose()
                })
                .collect()
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
            .map_err(|source| crate::provider_unavailable("clear credential cooldown", source))
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
            .map_err(|source| {
                crate::provider_unavailable("cache scoped credential cooldown", source)
            })
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
            .map_err(|source| {
                crate::provider_unavailable("read scoped credential cooldown", source)
            })?
            .map(|(revision, until)| {
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

    fn read_scoped_many<'a>(
        &'a self,
        account_ids: &'a [ProviderAccountId],
        scope: &'a ProviderCooldownScope,
    ) -> futures::future::BoxFuture<
        'a,
        Vec<Result<Option<ProviderScopedCooldown>, ProviderStoreError>>,
    > {
        Box::pin(async move {
            let script = Script::new(READ_SCRIPT);
            let mut results = Vec::with_capacity(account_ids.len());
            for batch in account_ids.chunks(COOLDOWN_READ_BATCH_SIZE) {
                let commands = batch
                    .iter()
                    .map(|account| {
                        let mut command = redis::cmd("EVALSHA");
                        command
                            .arg(script.get_hash())
                            .arg(1)
                            .arg(self.scoped_key(account.as_str(), scope)?)
                            .arg("");
                        Ok(command)
                    })
                    .collect();
                let values = self.read_script_batch(&script, commands).await;
                results.extend(batch.iter().zip(values).map(|(account, value)| {
                    let value = value
                        .and_then(|value| {
                            redis::from_redis_value(value).map_err(|source| {
                                redis_unavailable("decode scoped cooldown", source)
                            })
                        })
                        .and_then(parse_scoped_cooldown)
                        .map_err(|source| {
                            crate::provider_unavailable("read scoped credential cooldown", source)
                        })?;
                    value
                        .map(|(revision, until)| {
                            Ok(ProviderScopedCooldown::new(
                                account.clone(),
                                CredentialRevision::new(revision.get()).map_err(|_| {
                                    provider_invalid("decode scoped credential cooldown")
                                })?,
                                scope.clone(),
                                until.into(),
                            ))
                        })
                        .transpose()
                }));
            }
            results
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
            .map_err(|source| {
                crate::provider_unavailable("clear scoped credential cooldown", source)
            })
        })
    }

    fn clear_all<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            self.delete_account_cooldowns(account_id.as_str())
                .await
                .map_err(|source| {
                    crate::provider_unavailable("clear all credential cooldowns", source)
                })
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
                .map_err(|source| {
                    crate::provider_unavailable("clear cooldown after success", source)
                })?;
            Ok(())
        })
    }
}

pub(super) fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        source: None,
        entity: "credential cooldown",
        message: message.to_owned(),
    }
}

fn parse_scoped_cooldown(
    (present, revision, until_ms, _, _): (i64, String, String, String, String),
) -> StoreResult<Option<(Revision, DateTime<Utc>)>> {
    if present == 0 {
        return Ok(None);
    }
    let revision = revision
        .parse::<u64>()
        .map_err(|source| invalid("cached cooldown revision is invalid").with_source(source))?;
    let until_ms = until_ms
        .parse::<i64>()
        .map_err(|source| invalid("cached cooldown expiry is invalid").with_source(source))?;
    let cooldown_until = DateTime::from_timestamp_millis(until_ms)
        .ok_or_else(|| invalid("cached cooldown expiry is invalid"))?;
    Ok(Some((Revision::new(revision)?, cooldown_until)))
}

fn provider_cooldown(record: CredentialCooldown) -> Result<ProviderCooldown, ProviderStoreError> {
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
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}
