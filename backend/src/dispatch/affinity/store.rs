//! Redis 会话亲和存储。

use chrono::{DateTime, Duration, Utc};
use redis::AsyncCommands;
use thiserror::Error;

use crate::infra::redis::RedisConnection;

use super::types::SessionAffinityEntry;

/// 单会话保留的最近响应元数据数量。
pub const MAX_CONVERSATION_AFFINITIES: usize = 128;
/// 单账号保留的最近响应索引数量。
pub const MAX_ACCOUNT_AFFINITIES: usize = 16_384;
/// Redis 全局保留的最近响应元数据数量。
pub const MAX_GLOBAL_AFFINITIES: usize = 65_536;
/// Redis 全局响应元数据 JSON 字节预算。
pub const MAX_GLOBAL_AFFINITY_BYTES: u64 = 128 * 1024 * 1024;

const MAX_AFFINITY_METADATA_BYTES: usize = 64 * 1024;
const GLOBAL_STALE_CLEANUP_BATCH: usize = 256;
const AFFINITY_VERSION: &str = "affinity:v3";

const UPSERT_SCRIPT: &str = r#"
local response_id = ARGV[1]
local metadata_json = ARGV[2]
local score = tonumber(ARGV[3])
local expires_at = tonumber(ARGV[4])
local ttl_seconds = tonumber(ARGV[5])
local cutoff_ms = tonumber(ARGV[6])
local response_prefix = ARGV[7]
local conversation_prefix = ARGV[8]
local account_prefix = ARGV[9]
local max_conversation = tonumber(ARGV[10])
local max_account = tonumber(ARGV[11])
local max_global = tonumber(ARGV[12])
local max_global_bytes = tonumber(ARGV[13])
local stale_cleanup_batch = tonumber(ARGV[14])

local current = cjson.decode(metadata_json)
local conversation_id = current.conversationId
local account_id = current.accountId

local function decrement_bytes(size)
  if size <= 0 then
    return
  end
  local remaining = redis.call('DECRBY', KEYS[5], size)
  if remaining <= 0 then
    redis.call('DEL', KEYS[5])
  end
end

local function read_info(id)
  local raw = redis.call('HGET', KEYS[6], id)
  if not raw then
    return nil
  end
  local ok, info = pcall(cjson.decode, raw)
  if not ok then
    redis.call('HDEL', KEYS[6], id)
    return nil
  end
  return info
end

local function remove_response(id)
  local response_key = response_prefix .. id
  local info = read_info(id)
  local metadata = nil
  local raw = redis.call('GET', response_key)
  if raw then
    local ok, decoded = pcall(cjson.decode, raw)
    if ok then
      metadata = decoded
    end
  end

  local stored_conversation_id = info and info.conversationId or metadata and metadata.conversationId
  local stored_account_id = info and info.accountId or metadata and metadata.accountId
  local metadata_bytes = info and tonumber(info.metadataBytes or 0)
    or redis.call('STRLEN', response_key)

  redis.call('UNLINK', response_key)
  redis.call('ZREM', KEYS[4], id)
  redis.call('HDEL', KEYS[6], id)
  redis.call('ZREM', KEYS[2], id)
  redis.call('ZREM', KEYS[3], id)
  decrement_bytes(metadata_bytes)
  if stored_conversation_id then
    redis.call('ZREM', conversation_prefix .. stored_conversation_id, id)
  end
  if stored_account_id then
    redis.call('ZREM', account_prefix .. stored_account_id, id)
  end
end

local function trim_index(key, max_members)
  local excess = redis.call('ZCARD', key) - max_members
  if excess <= 0 then
    return
  end
  local ids = redis.call('ZRANGE', key, 0, excess - 1)
  for _, id in ipairs(ids) do
    remove_response(id)
  end
end

local previous_info = read_info(response_id)
if previous_info then
  if previous_info.conversationId and previous_info.conversationId ~= conversation_id then
    redis.call('ZREM', conversation_prefix .. previous_info.conversationId, response_id)
  end
  if previous_info.accountId and previous_info.accountId ~= account_id then
    redis.call('ZREM', account_prefix .. previous_info.accountId, response_id)
  end
end
local previous_bytes = previous_info and tonumber(previous_info.metadataBytes or 0)
  or redis.call('STRLEN', KEYS[1])
local metadata_bytes = string.len(metadata_json)

redis.call('SET', KEYS[1], metadata_json, 'EXAT', expires_at)
redis.call('ZADD', KEYS[2], score, response_id)
redis.call('EXPIRE', KEYS[2], ttl_seconds)
redis.call('ZADD', KEYS[3], score, response_id)
redis.call('EXPIRE', KEYS[3], ttl_seconds)
redis.call('ZADD', KEYS[4], score, response_id)
redis.call('EXPIRE', KEYS[4], ttl_seconds)
redis.call('INCRBY', KEYS[5], metadata_bytes - previous_bytes)
redis.call('EXPIRE', KEYS[5], ttl_seconds)
redis.call('HSET', KEYS[6], response_id, cjson.encode({
  conversationId = conversation_id,
  accountId = account_id,
  metadataBytes = metadata_bytes,
}))
redis.call('EXPIRE', KEYS[6], ttl_seconds)

local stale_conversation_ids = redis.call('ZRANGEBYSCORE', KEYS[2], '-inf', cutoff_ms)
for _, id in ipairs(stale_conversation_ids) do
  remove_response(id)
end
local stale_account_ids = redis.call(
  'ZRANGEBYSCORE', KEYS[3], '-inf', cutoff_ms, 'LIMIT', 0, stale_cleanup_batch
)
for _, id in ipairs(stale_account_ids) do
  remove_response(id)
end
local stale_global_ids = redis.call(
  'ZRANGEBYSCORE', KEYS[4], '-inf', cutoff_ms, 'LIMIT', 0, stale_cleanup_batch
)
for _, id in ipairs(stale_global_ids) do
  remove_response(id)
end

trim_index(KEYS[2], max_conversation)
trim_index(KEYS[3], max_account)
trim_index(KEYS[4], max_global)

while tonumber(redis.call('GET', KEYS[5]) or '0') > max_global_bytes do
  local oldest = redis.call('ZRANGE', KEYS[4], 0, 0)
  if #oldest == 0 then
    redis.call('DEL', KEYS[5])
    break
  end
  remove_response(oldest[1])
end

return 1
"#;

const FORGET_SCRIPT: &str = r#"
local response_id = ARGV[1]
local response_prefix = ARGV[2]
local conversation_prefix = ARGV[3]
local account_prefix = ARGV[4]
local response_key = response_prefix .. response_id
local info_json = redis.call('HGET', KEYS[3], response_id)
local metadata_json = redis.call('GET', response_key)
if not info_json and not metadata_json then
  return 0
end

local info = nil
if info_json then
  local ok, decoded = pcall(cjson.decode, info_json)
  if ok then
    info = decoded
  end
end
local metadata = nil
if metadata_json then
  local ok, decoded = pcall(cjson.decode, metadata_json)
  if ok then
    metadata = decoded
  end
end
local conversation_id = info and info.conversationId or metadata and metadata.conversationId
local account_id = info and info.accountId or metadata and metadata.accountId
local metadata_bytes = info and tonumber(info.metadataBytes or 0)
  or redis.call('STRLEN', response_key)

redis.call('UNLINK', response_key)
redis.call('ZREM', KEYS[1], response_id)
redis.call('HDEL', KEYS[3], response_id)
if metadata_bytes > 0 then
  local remaining = redis.call('DECRBY', KEYS[2], metadata_bytes)
  if remaining <= 0 then
    redis.call('DEL', KEYS[2])
  end
end
if conversation_id then
  redis.call('ZREM', conversation_prefix .. conversation_id, response_id)
end
if account_id then
  redis.call('ZREM', account_prefix .. account_id, response_id)
end
return 1
"#;

const FORGET_ACCOUNT_SCRIPT: &str = r#"
local account_id = ARGV[1]
local response_prefix = ARGV[2]
local conversation_prefix = ARGV[3]
local account_prefix = ARGV[4]
local account_key = account_prefix .. account_id
local response_ids = redis.call('ZRANGE', account_key, 0, -1)

local function decrement_bytes(size)
  if size <= 0 then
    return
  end
  local remaining = redis.call('DECRBY', KEYS[3], size)
  if remaining <= 0 then
    redis.call('DEL', KEYS[3])
  end
end

for _, response_id in ipairs(response_ids) do
  local response_key = response_prefix .. response_id
  local info_json = redis.call('HGET', KEYS[4], response_id)
  local metadata_json = redis.call('GET', response_key)
  local info = nil
  if info_json then
    local ok, decoded = pcall(cjson.decode, info_json)
    if ok then
      info = decoded
    end
  end
  local metadata = nil
  if metadata_json then
    local ok, decoded = pcall(cjson.decode, metadata_json)
    if ok then
      metadata = decoded
    end
  end
  local stored_account_id = info and info.accountId or metadata and metadata.accountId
  if stored_account_id == account_id then
    local conversation_id = info and info.conversationId or metadata and metadata.conversationId
    local metadata_bytes = info and tonumber(info.metadataBytes or 0)
      or redis.call('STRLEN', response_key)
    redis.call('UNLINK', response_key)
    redis.call('ZREM', KEYS[2], response_id)
    redis.call('HDEL', KEYS[4], response_id)
    decrement_bytes(metadata_bytes)
    if conversation_id then
      redis.call('ZREM', conversation_prefix .. conversation_id, response_id)
    end
  end
end

redis.call('DEL', account_key)
return #response_ids
"#;

/// Redis 会话亲和性存储。
#[derive(Clone)]
pub struct RedisSessionAffinityStore {
    redis: RedisConnection,
}

/// Redis 会话亲和性存储错误。
#[derive(Debug, Error)]
pub enum RedisSessionAffinityStoreError {
    #[error("Redis session affinity operation failed: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("invalid Redis session affinity value: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session affinity metadata is too large: {bytes} bytes exceeds {max} bytes")]
    MetadataTooLarge { bytes: usize, max: usize },
}

pub type RedisSessionAffinityStoreResult<T> = Result<T, RedisSessionAffinityStoreError>;

impl RedisSessionAffinityStore {
    pub fn new(redis: RedisConnection) -> Self {
        Self { redis }
    }

    pub async fn upsert(
        &self,
        response_id: &str,
        entry: &SessionAffinityEntry,
        ttl: Duration,
    ) -> RedisSessionAffinityStoreResult<()> {
        let expires_at = entry
            .created_at
            .checked_add_signed(ttl)
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        if expires_at <= Utc::now() {
            return Ok(());
        }

        let metadata_json = serde_json::to_string(entry)?;
        if metadata_json.len() > MAX_AFFINITY_METADATA_BYTES {
            return Err(RedisSessionAffinityStoreError::MetadataTooLarge {
                bytes: metadata_json.len(),
                max: MAX_AFFINITY_METADATA_BYTES,
            });
        }

        let ttl_seconds = ttl.num_seconds().max(1);
        let cutoff_ms = Utc::now().timestamp_millis() - ttl.num_milliseconds();
        let mut connection = self.redis.manager();
        let _: i64 = redis::Script::new(UPSERT_SCRIPT)
            .key(self.response_key(response_id))
            .key(self.conversation_key(&entry.conversation_id))
            .key(self.account_key(&entry.account_id))
            .key(self.global_index_key())
            .key(self.global_bytes_key())
            .key(self.global_info_key())
            .arg(response_id)
            .arg(metadata_json)
            .arg(entry.created_at.timestamp_millis())
            .arg(expires_at.timestamp())
            .arg(ttl_seconds)
            .arg(cutoff_ms)
            .arg(self.response_key_prefix())
            .arg(self.conversation_key_prefix())
            .arg(self.account_key_prefix())
            .arg(MAX_CONVERSATION_AFFINITIES)
            .arg(MAX_ACCOUNT_AFFINITIES)
            .arg(MAX_GLOBAL_AFFINITIES)
            .arg(MAX_GLOBAL_AFFINITY_BYTES)
            .arg(GLOBAL_STALE_CLEANUP_BATCH)
            .invoke_async(&mut connection)
            .await?;
        Ok(())
    }

    pub async fn get(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> RedisSessionAffinityStoreResult<Option<SessionAffinityEntry>> {
        let mut connection = self.redis.manager();
        let value: Option<String> = connection.get(self.response_key(response_id)).await?;
        let Some(value) = value else {
            return Ok(None);
        };
        let entry: SessionAffinityEntry = serde_json::from_str(&value)?;
        if entry
            .created_at
            .checked_add_signed(ttl)
            .is_none_or(|expires_at| expires_at <= now)
        {
            self.forget(response_id).await?;
            return Ok(None);
        }
        Ok(Some(entry))
    }

    pub async fn latest_by_conversation(
        &self,
        conversation_id: &str,
        max_age: Option<Duration>,
        variant_hash: Option<&str>,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> RedisSessionAffinityStoreResult<Option<(String, SessionAffinityEntry)>> {
        let conversation_key = self.conversation_key(conversation_id);
        let mut connection = self.redis.manager();
        let response_ids: Vec<String> = redis::cmd("ZREVRANGE")
            .arg(&conversation_key)
            .arg(0)
            .arg(MAX_CONVERSATION_AFFINITIES.saturating_sub(1))
            .query_async(&mut connection)
            .await?;
        if response_ids.is_empty() {
            return Ok(None);
        }

        let response_keys = response_ids
            .iter()
            .map(|response_id| self.response_key(response_id))
            .collect::<Vec<_>>();
        let values: Vec<Option<String>> = redis::cmd("MGET")
            .arg(response_keys)
            .query_async(&mut connection)
            .await?;
        let mut stale_ids = Vec::new();
        let mut result = None;
        for (response_id, value) in response_ids.into_iter().zip(values) {
            let Some(value) = value else {
                stale_ids.push(response_id);
                continue;
            };
            let entry: SessionAffinityEntry = serde_json::from_str(&value)?;
            if entry.conversation_id != conversation_id
                || entry
                    .created_at
                    .checked_add_signed(ttl)
                    .is_none_or(|expires_at| expires_at <= now)
            {
                stale_ids.push(response_id);
                continue;
            }
            if variant_hash.is_some_and(|expected| entry.variant_hash.as_deref() != Some(expected))
                || max_age
                    .is_some_and(|max_age| now.signed_duration_since(entry.created_at) > max_age)
            {
                continue;
            }
            result = Some((response_id, entry));
            break;
        }
        if !stale_ids.is_empty() {
            let _: usize = connection.zrem(conversation_key, stale_ids).await?;
        }
        Ok(result)
    }

    pub async fn forget(&self, response_id: &str) -> RedisSessionAffinityStoreResult<bool> {
        let mut connection = self.redis.manager();
        let forgotten: i64 = redis::Script::new(FORGET_SCRIPT)
            .key(self.global_index_key())
            .key(self.global_bytes_key())
            .key(self.global_info_key())
            .arg(response_id)
            .arg(self.response_key_prefix())
            .arg(self.conversation_key_prefix())
            .arg(self.account_key_prefix())
            .invoke_async(&mut connection)
            .await?;
        Ok(forgotten > 0)
    }

    pub async fn forget_account(&self, account_id: &str) -> RedisSessionAffinityStoreResult<u64> {
        let mut connection = self.redis.manager();
        let forgotten: u64 = redis::Script::new(FORGET_ACCOUNT_SCRIPT)
            .key(self.account_key(account_id))
            .key(self.global_index_key())
            .key(self.global_bytes_key())
            .key(self.global_info_key())
            .arg(account_id)
            .arg(self.response_key_prefix())
            .arg(self.conversation_key_prefix())
            .arg(self.account_key_prefix())
            .invoke_async(&mut connection)
            .await?;
        Ok(forgotten)
    }

    fn response_key(&self, response_id: &str) -> String {
        format!("{}{response_id}", self.response_key_prefix())
    }

    fn conversation_key(&self, conversation_id: &str) -> String {
        format!("{}{conversation_id}", self.conversation_key_prefix())
    }

    fn account_key(&self, account_id: &str) -> String {
        format!("{}{account_id}", self.account_key_prefix())
    }

    fn global_index_key(&self) -> String {
        self.redis.key(&format!("{AFFINITY_VERSION}:global:index"))
    }

    fn global_bytes_key(&self) -> String {
        self.redis.key(&format!("{AFFINITY_VERSION}:global:bytes"))
    }

    fn global_info_key(&self) -> String {
        self.redis.key(&format!("{AFFINITY_VERSION}:global:info"))
    }

    fn response_key_prefix(&self) -> String {
        self.redis.key(&format!("{AFFINITY_VERSION}:resp:"))
    }

    fn conversation_key_prefix(&self) -> String {
        self.redis.key(&format!("{AFFINITY_VERSION}:conv:"))
    }

    fn account_key_prefix(&self) -> String {
        self.redis.key(&format!("{AFFINITY_VERSION}:account:"))
    }
}
