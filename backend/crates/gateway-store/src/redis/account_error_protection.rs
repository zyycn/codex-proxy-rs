//! 账号保护的原子反馈、策略栅栏与独立冻结状态

use super::{CredentialCooldown, RedisCredentialCooldownRepository, resource_fingerprint};
use crate::{Revision, StoreResult, redis_unavailable};
use chrono::{DateTime, Utc};
use gateway_admin::model::accounts::AccountFreeze;
use gateway_core::{
    account::{
        AccountErrorProtection, AccountProtectionMatch, CredentialRevision, ProviderAccountId,
    },
    provider_ports::{AccountProtectionGuard, ProviderCooldownKind},
    routing::ConfigRevision,
};
use redis::Script;
use std::collections::BTreeMap;
use std::sync::LazyLock;

const PUBLISH: &str = r#"
local old = redis.call('HGET', KEYS[1], 'fingerprint')
local revision = tonumber(redis.call('HGET', KEYS[1], 'revision') or '0')
if revision > tonumber(ARGV[2]) then return 0 end
if old ~= ARGV[1] or revision ~= tonumber(ARGV[2]) then
  redis.call('HSET', KEYS[1], 'fingerprint', ARGV[1], 'generation', ARGV[3])
end
redis.call('HSET', KEYS[1], 'revision', ARGV[2], 'enabled', ARGV[4])
return 1
"#;

// 原 key 的代次和恢复时间原样搬运，不重新开始冷却
const MIGRATE: &str = r#"
local function migrate_protection(legacy, protection, legacy_index, protection_index, account)
  local kind = redis.call('HGET', legacy, 'kind')
  if kind == 'capacity_freeze' or kind == 'capacity_freeze_probe' then
    if redis.call('EXISTS', protection) == 0 then redis.call('RENAME', legacy, protection) else redis.call('DEL', legacy) end
    redis.call('ZREM', legacy_index, account)
    redis.call('ZADD', protection_index, redis.call('HGET', protection, 'until_ms'), account)
  end
end
"#;

static MIGRATE_SCRIPT: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        &[
            MIGRATE,
            "migrate_protection(KEYS[1], KEYS[2], KEYS[3], KEYS[4], ARGV[1])\nreturn 1",
        ]
        .concat(),
    )
});

static BEGIN: LazyLock<Script> = LazyLock::new(|| {
    Script::new(&[MIGRATE, r#"
migrate_protection(KEYS[5], KEYS[3], KEYS[6], KEYS[4], ARGV[5])
if redis.call('HGET', KEYS[1], 'enabled') ~= '1'
  or redis.call('HGET', KEYS[1], 'fingerprint') ~= ARGV[2]
  or tonumber(redis.call('HGET', KEYS[1], 'revision') or '0') ~= tonumber(ARGV[3]) then return {} end
local clock = redis.call('TIME')
local now = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
local frozen_revision = tonumber(redis.call('HGET', KEYS[3], 'revision') or '0')
if frozen_revision > 0 then
  local until_ms = tonumber(redis.call('HGET', KEYS[3], 'until_ms') or '0')
  local kind = redis.call('HGET', KEYS[3], 'kind')
  if frozen_revision >= tonumber(ARGV[1]) and (until_ms > now or kind == 'error_protection_probe' or kind == 'capacity_freeze_probe') then return {} end
  redis.call('DEL', KEYS[3])
  redis.call('ZREM', KEYS[4], ARGV[5])
end
local current = tonumber(redis.call('HGET', KEYS[2], 'revision') or '0')
if current > tonumber(ARGV[1]) then return {} end
local generation = redis.call('HGET', KEYS[1], 'generation')
if current ~= tonumber(ARGV[1]) or redis.call('HGET', KEYS[2], 'policy') ~= generation then
  redis.call('DEL', KEYS[2])
  redis.call('HSET', KEYS[2], 'revision', ARGV[1], 'policy', generation, 'reset', ARGV[4], 'version', 0)
end
redis.call('EXPIRE', KEYS[2], 691200)
return {generation, redis.call('HGET', KEYS[2], 'reset'), redis.call('HGET', KEYS[2], 'version')}
"#].concat())
});

// 反馈不创建丢失的 guard 状态；Redis 丢失状态或手动恢复后不能重放历史失败
const FAILURE: &str = r#"
if redis.call('HGET', KEYS[1], 'generation') ~= ARGV[2]
  or redis.call('HGET', KEYS[1], 'enabled') ~= '1'
  or redis.call('HGET', KEYS[2], 'revision') ~= ARGV[1]
  or redis.call('HGET', KEYS[2], 'policy') ~= ARGV[2]
  or redis.call('HGET', KEYS[2], 'reset') ~= ARGV[3]
  or redis.call('EXISTS', KEYS[3]) == 1 then return 0 end
local clock = redis.call('TIME')
local now = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
local count = tonumber(redis.call('HGET', KEYS[2], 'count') or '0')
if tonumber(redis.call('HGET', KEYS[2], 'count_until') or '0') <= now then count = 0 end
count = count + 1
redis.call('HSET', KEYS[2], 'count', count, 'count_until', now + tonumber(ARGV[4]) * 1000)
redis.call('HINCRBY', KEYS[2], 'version', 1)
if count < tonumber(ARGV[5]) then return 0 end
local until_ms = now + tonumber(ARGV[6]) * 1000
redis.call('HSET', KEYS[3], 'revision', ARGV[1], 'until_ms', until_ms, 'kind', ARGV[7], 'generation', ARGV[8], 'duration', ARGV[6], 'model', ARGV[9], 'rules', ARGV[10])
redis.call('HSET', KEYS[2], 'reset', ARGV[8], 'count', 0)
redis.call('ZADD', KEYS[4], until_ms, ARGV[11])
if ARGV[7] ~= 'error_protection_probe' then redis.call('PEXPIRE', KEYS[3], tonumber(ARGV[6]) * 1000 + 60000) end
return 1
"#;

const SUCCESS: &str = r#"
if redis.call('HGET', KEYS[1], 'generation') ~= ARGV[2]
  or redis.call('HGET', KEYS[2], 'revision') ~= ARGV[1]
  or redis.call('HGET', KEYS[2], 'policy') ~= ARGV[2]
  or redis.call('HGET', KEYS[2], 'reset') ~= ARGV[3]
  or redis.call('HGET', KEYS[2], 'version') ~= ARGV[4]
  or redis.call('EXISTS', KEYS[3]) == 1 then return 0 end
redis.call('HDEL', KEYS[2], 'count', 'count_until')
redis.call('HINCRBY', KEYS[2], 'version', 1)
return 1
"#;

// 调度和管理详情共用原子状态判断，各自只返回所需数据
const READ_EFFECTIVE: &str = r#"
migrate_protection(KEYS[1], KEYS[3], KEYS[2], KEYS[4], ARGV[1])
local clock = redis.call('TIME')
local now = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
local selected = {}
for i = 1, 3, 2 do
  local revision = redis.call('HGET', KEYS[i], 'revision')
  local until_ms = redis.call('HGET', KEYS[i], 'until_ms')
  local kind = redis.call('HGET', KEYS[i], 'kind') or 'rate_limit'
  if revision and until_ms and (tonumber(until_ms) > now or kind == 'capacity_freeze_probe' or kind == 'error_protection_probe') then
    if #selected == 0 or tonumber(revision) >= tonumber(selected[1]) then selected = {revision, until_ms, kind} end
  else
    redis.call('DEL', KEYS[i]); redis.call('ZREM', KEYS[i+1], ARGV[1])
  end
end
"#;

static READ_COOLDOWN: LazyLock<Script> =
    LazyLock::new(|| Script::new(&[MIGRATE, READ_EFFECTIVE, "return selected"].concat()));

static READ_ACCOUNT_RUNTIME: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        &[
            MIGRATE,
            READ_EFFECTIVE,
            "return {selected, redis.call('HGETALL', KEYS[3])}",
        ]
        .concat(),
    )
});

const CLEAR: &str = r#"
local current = math.max(tonumber(redis.call('HGET', KEYS[1], 'revision') or '0'), tonumber(redis.call('HGET', KEYS[2], 'revision') or '0'))
if current > tonumber(ARGV[1]) then return 0 end
local removed = redis.call('DEL', KEYS[1])
redis.call('HSET', KEYS[2], 'revision', ARGV[1], 'reset', ARGV[2], 'version', 0)
redis.call('HDEL', KEYS[2], 'count', 'count_until')
redis.call('EXPIRE', KEYS[2], 691200)
redis.call('ZREM', KEYS[3], ARGV[3])
return removed
"#;

const ADVANCE_REVISION: &str = r#"
local current = tonumber(redis.call('HGET', KEYS[2], 'revision') or '0')
local frozen = tonumber(redis.call('HGET', KEYS[1], 'revision') or '0')
if current >= tonumber(ARGV[1]) or frozen >= tonumber(ARGV[1]) then return 0 end
redis.call('DEL', KEYS[1]); redis.call('ZREM', KEYS[3], ARGV[3])
redis.call('HSET', KEYS[2], 'revision', ARGV[1], 'reset', ARGV[2], 'version', 0)
redis.call('HDEL', KEYS[2], 'count', 'count_until')
redis.call('EXPIRE', KEYS[2], 691200)
return 1
"#;

const FINISH: &str = r#"
if redis.call('HGET', KEYS[1], 'revision') ~= ARGV[1] or redis.call('HGET', KEYS[1], 'generation') ~= ARGV[2] then return 0 end
if ARGV[3] == '' then
  redis.call('DEL', KEYS[1]); redis.call('ZREM', KEYS[3], ARGV[5])
  redis.call('HSET', KEYS[2], 'reset', ARGV[4], 'version', 0)
  redis.call('HDEL', KEYS[2], 'count', 'count_until')
  redis.call('EXPIRE', KEYS[2], 691200)
else
  local until_ms = math.max(tonumber(redis.call('HGET', KEYS[1], 'until_ms')),tonumber(ARGV[3]))
  redis.call('HSET', KEYS[1], 'until_ms', until_ms, 'generation', ARGV[4])
  redis.call('ZADD', KEYS[3], until_ms, ARGV[5])
end
return 1
"#;

impl RedisCredentialCooldownRepository {
    fn policy_key(&self) -> String {
        format!("{}:account:error-protection-policy", self.namespace)
    }
    pub(super) fn protection_key(&self, account: &str) -> StoreResult<String> {
        Ok(format!(
            "{}:account:{}:error-protection",
            self.namespace,
            resource_fingerprint("credential cooldown", account)?
        ))
    }
    pub(super) fn protection_state_key(&self, account: &str) -> StoreResult<String> {
        Ok(format!(
            "{}:account:{}:error-protection-state",
            self.namespace,
            resource_fingerprint("credential cooldown", account)?
        ))
    }
    pub(super) fn protection_index_key(&self) -> String {
        format!("{}:account:active-protections", self.namespace)
    }

    pub(super) async fn publish_policy(
        &self,
        policy: &AccountErrorProtection,
        revision: ConfigRevision,
    ) -> StoreResult<()> {
        Script::new(PUBLISH)
            .key(self.policy_key())
            .arg(policy.fingerprint())
            .arg(revision.get())
            .arg(uuid::Uuid::new_v4().to_string())
            .arg(u8::from(policy.enabled))
            .invoke_async::<i64>(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("publish account protection", e))?;
        Ok(())
    }
    pub(super) async fn migrate_protection(&self, account: &str) -> StoreResult<()> {
        MIGRATE_SCRIPT
            .key(self.key(account)?)
            .key(self.protection_key(account)?)
            .key(self.active_index_key())
            .key(self.protection_index_key())
            .arg(account)
            .invoke_async::<i64>(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("migrate account protection", e))?;
        Ok(())
    }
    pub(super) async fn begin_account_protection(
        &self,
        account: &ProviderAccountId,
        revision: CredentialRevision,
        fingerprint: &str,
        snapshot_revision: ConfigRevision,
    ) -> StoreResult<Option<AccountProtectionGuard>> {
        let result: Vec<String> = BEGIN
            .key(self.policy_key())
            .key(self.protection_state_key(account.as_str())?)
            .key(self.protection_key(account.as_str())?)
            .key(self.protection_index_key())
            .key(self.key(account.as_str())?)
            .key(self.active_index_key())
            .arg(revision.get())
            .arg(fingerprint)
            .arg(snapshot_revision.get())
            .arg(uuid::Uuid::new_v4().to_string())
            .arg(account.as_str())
            .invoke_async(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("begin account protection", e))?;
        let [generation, reset, version] = result.as_slice() else {
            return Ok(None);
        };
        Ok(Some(AccountProtectionGuard {
            account_id: account.clone(),
            credential_revision: revision,
            policy_generation: generation.clone(),
            reset_generation: reset.clone(),
            evidence_version: version
                .parse()
                .map_err(|_| super::credential_cooldown::invalid("protection version"))?,
        }))
    }
    pub(super) async fn protection_failure(
        &self,
        guard: &AccountProtectionGuard,
        policy: &AccountErrorProtection,
        matched: &[AccountProtectionMatch],
    ) -> StoreResult<bool> {
        if !policy.enabled || matched.is_empty() {
            return Ok(false);
        }
        let rules = serde_json::to_string(matched)
            .map_err(|_| super::credential_cooldown::invalid("protection rules"))?;
        let changed: i64 = Script::new(FAILURE)
            .key(self.policy_key())
            .key(self.protection_state_key(guard.account_id.as_str())?)
            .key(self.protection_key(guard.account_id.as_str())?)
            .key(self.protection_index_key())
            .arg(guard.credential_revision.get())
            .arg(&guard.policy_generation)
            .arg(&guard.reset_generation)
            .arg(policy.window_seconds)
            .arg(policy.threshold)
            .arg(policy.duration_seconds)
            .arg(if policy.probe_enabled {
                "error_protection_probe"
            } else {
                "error_protection"
            })
            .arg(uuid::Uuid::new_v4().to_string())
            .arg(policy.probe_model.as_deref().unwrap_or_default())
            .arg(rules)
            .arg(guard.account_id.as_str())
            .invoke_async(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("record protection failure", e))?;
        Ok(changed == 1)
    }
    pub(super) async fn protection_success(
        &self,
        guard: &AccountProtectionGuard,
    ) -> StoreResult<bool> {
        let changed: i64 = Script::new(SUCCESS)
            .key(self.policy_key())
            .key(self.protection_state_key(guard.account_id.as_str())?)
            .key(self.protection_key(guard.account_id.as_str())?)
            .arg(guard.credential_revision.get())
            .arg(&guard.policy_generation)
            .arg(&guard.reset_generation)
            .arg(guard.evidence_version)
            .invoke_async(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("record protection success", e))?;
        Ok(changed == 1)
    }
    pub(super) async fn read_effective_cooldown(
        &self,
        account: &str,
    ) -> StoreResult<Option<CredentialCooldown>> {
        let result: Vec<String> = READ_COOLDOWN
            .key(self.key(account)?)
            .key(self.active_index_key())
            .key(self.protection_key(account)?)
            .key(self.protection_index_key())
            .arg(account)
            .invoke_async(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("read account cooldown", e))?;
        parse_cooldown(account, &result)
    }

    pub(super) async fn read_account_runtime(
        &self,
        account: &str,
    ) -> StoreResult<(Option<CredentialCooldown>, Option<AccountFreeze>)> {
        let (result, values): (Vec<String>, BTreeMap<String, String>) = READ_ACCOUNT_RUNTIME
            .key(self.key(account)?)
            .key(self.active_index_key())
            .key(self.protection_key(account)?)
            .key(self.protection_index_key())
            .arg(account)
            .invoke_async(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("read account cooldown snapshot", e))?;
        let invalid = || super::credential_cooldown::invalid("invalid account cooldown snapshot");
        let cooldown = parse_cooldown(account, &result)?;
        let freeze = if values.is_empty() {
            None
        } else {
            let revision = values
                .get("revision")
                .ok_or_else(invalid)?
                .parse::<u64>()
                .map_err(|_| invalid())?;
            let until = DateTime::from_timestamp_millis(
                values
                    .get("until_ms")
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?,
            )
            .ok_or_else(invalid)?;
            let kind = ProviderCooldownKind::parse(values.get("kind").ok_or_else(invalid)?)
                .ok_or_else(invalid)?;
            Some(AccountFreeze {
                credential_revision: gateway_admin::model::Revision::new(revision)
                    .map_err(|_| invalid())?,
                until,
                generation: values.get("generation").cloned().unwrap_or_default(),
                requires_probe: kind.requires_probe(),
                duration_seconds: values
                    .get("duration")
                    .map(|v| v.parse())
                    .transpose()
                    .map_err(|_| invalid())?,
                probe_model: values.get("model").filter(|v| !v.is_empty()).cloned(),
                protection_rules: values
                    .get("rules")
                    .map(|v| serde_json::from_str(v))
                    .transpose()
                    .map_err(|_| invalid())?
                    .unwrap_or_default(),
            })
        };
        Ok((cooldown, freeze))
    }

    pub(super) async fn clear_protection(
        &self,
        account: &str,
        revision: Revision,
    ) -> StoreResult<bool> {
        self.migrate_protection(account).await?;
        let changed: i64 = Script::new(CLEAR)
            .key(self.protection_key(account)?)
            .key(self.protection_state_key(account)?)
            .key(self.protection_index_key())
            .arg(revision.get())
            .arg(uuid::Uuid::new_v4().to_string())
            .arg(account)
            .invoke_async(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("clear account protection", e))?;
        Ok(changed == 1)
    }
    pub(super) async fn advance_account_protection_revision(
        &self,
        account: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> StoreResult<bool> {
        self.migrate_protection(account.as_str()).await?;
        let changed: i64 = Script::new(ADVANCE_REVISION)
            .key(self.protection_key(account.as_str())?)
            .key(self.protection_state_key(account.as_str())?)
            .key(self.protection_index_key())
            .arg(revision.get())
            .arg(uuid::Uuid::new_v4().to_string())
            .arg(account.as_str())
            .invoke_async(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("advance protection credential revision", e))?;
        Ok(changed == 1)
    }

    pub(super) async fn finish_protection(
        &self,
        account: &str,
        expected: &AccountFreeze,
        postpone: Option<DateTime<Utc>>,
    ) -> StoreResult<bool> {
        let changed: i64 = Script::new(FINISH)
            .key(self.protection_key(account)?)
            .key(self.protection_state_key(account)?)
            .key(self.protection_index_key())
            .arg(expected.credential_revision.get())
            .arg(&expected.generation)
            .arg(
                postpone
                    .map(|v| v.timestamp_millis().to_string())
                    .unwrap_or_default(),
            )
            .arg(uuid::Uuid::new_v4().to_string())
            .arg(account)
            .invoke_async(&mut self.connection.clone())
            .await
            .map_err(|e| redis_unavailable("finish account protection", e))?;
        Ok(changed == 1)
    }
}

fn parse_cooldown(account: &str, values: &[String]) -> StoreResult<Option<CredentialCooldown>> {
    let [revision, until, kind] = values else {
        return Ok(None);
    };
    let invalid = || super::credential_cooldown::invalid("invalid account cooldown snapshot");
    Ok(Some(CredentialCooldown {
        provider_account_id: account.to_owned(),
        credential_revision: Revision::new(revision.parse().map_err(|_| invalid())?)?,
        cooldown_until: DateTime::from_timestamp_millis(until.parse().map_err(|_| invalid())?)
            .ok_or_else(invalid)?,
        kind: ProviderCooldownKind::parse(kind).ok_or_else(invalid)?,
    }))
}
