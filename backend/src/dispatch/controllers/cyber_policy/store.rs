use chrono::Duration;
use redis::AsyncCommands;
use thiserror::Error;

use crate::infra::redis::RedisConnection;

use super::types::SessionState;

const STATE_VERSION: &str = "cyber-policy:v2";

const RECORD_FAILURE_SCRIPT: &str = r#"
local raw = redis.call('GET', KEYS[1])
local state = { failedAccountIds = {}, revision = '' }
if raw then
  local ok, decoded = pcall(cjson.decode, raw)
  if ok and type(decoded) == 'table' and type(decoded.failedAccountIds) == 'table' then
    state = decoded
  end
end

local account_id = ARGV[1]
local known = false
for _, failed_account_id in ipairs(state.failedAccountIds) do
  if failed_account_id == account_id then
    known = true
    break
  end
end

if not known then
  table.insert(state.failedAccountIds, account_id)
end

state.revision = ARGV[3]

local encoded = cjson.encode(state)
redis.call('SET', KEYS[1], encoded, 'EX', tonumber(ARGV[2]))
return encoded
"#;

const CLEAR_STATE_SCRIPT: &str = r#"
local raw = redis.call('GET', KEYS[1])
if not raw then
  return 0
end
local ok, state = pcall(cjson.decode, raw)
if not ok or type(state) ~= 'table' then
  return 0
end
if tostring(state.revision or '') ~= ARGV[1] then
  return 0
end
return redis.call('DEL', KEYS[1])
"#;

#[derive(Clone)]
pub struct Store {
    redis: RedisConnection,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("Redis cyber policy operation failed: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("invalid Redis cyber policy value: {0}")]
    Json(#[from] serde_json::Error),
}

impl Store {
    pub fn new(redis: RedisConnection) -> Self {
        Self { redis }
    }

    pub async fn load(&self, session_key: &str) -> Result<Option<SessionState>, StoreError> {
        let mut connection = self.redis.manager();
        let value: Option<String> = connection.get(self.key(session_key)).await?;
        value
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(Into::into)
    }

    pub async fn record_failure(
        &self,
        session_key: &str,
        account_id: &str,
        ttl: Duration,
    ) -> Result<SessionState, StoreError> {
        let mut connection = self.redis.manager();
        let revision = uuid::Uuid::new_v4().to_string();
        let value: String = redis::Script::new(RECORD_FAILURE_SCRIPT)
            .key(self.key(session_key))
            .arg(account_id)
            .arg(ttl.num_seconds().max(1))
            .arg(revision)
            .invoke_async(&mut connection)
            .await?;
        Ok(serde_json::from_str(&value)?)
    }

    pub async fn clear(
        &self,
        session_key: &str,
        expected_revision: &str,
    ) -> Result<bool, StoreError> {
        let mut connection = self.redis.manager();
        let removed: usize = redis::Script::new(CLEAR_STATE_SCRIPT)
            .key(self.key(session_key))
            .arg(expected_revision)
            .invoke_async(&mut connection)
            .await?;
        Ok(removed > 0)
    }

    fn key(&self, session_key: &str) -> String {
        self.redis
            .key(&format!("{STATE_VERSION}:session:{session_key}"))
    }
}
