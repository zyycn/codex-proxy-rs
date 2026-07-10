//! Redis token 刷新租约存储。

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::infra::redis::RedisConnection;

/// Redis 刷新租约存储结果。
pub type RedisRefreshLeaseStoreResult<T> = Result<T, RedisRefreshLeaseStoreError>;

/// Redis 刷新租约存储错误。
#[derive(Debug, Error)]
pub enum RedisRefreshLeaseStoreError {
    #[error("Redis refresh lease operation failed: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("failed to encode refresh lease owner: {0}")]
    Json(#[from] serde_json::Error),
}

/// Redis 刷新租约存储。
#[derive(Clone)]
pub struct RedisRefreshLeaseStore {
    redis: RedisConnection,
}

impl RedisRefreshLeaseStore {
    /// 构造存储。
    pub fn new(redis: RedisConnection) -> Self {
        Self { redis }
    }

    /// 尝试获取账号刷新租约。
    pub async fn try_acquire(
        &self,
        account_id: &str,
        owner: &str,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> RedisRefreshLeaseStoreResult<bool> {
        let ttl_ms = expires_at.signed_duration_since(now).num_milliseconds();
        if ttl_ms <= 0 {
            return Ok(false);
        }
        let key = self.lease_key(account_id);
        let owner = serde_json::to_string(owner)?;
        let mut connection = self.redis.manager();
        let acquired: i64 = redis::Script::new(
            r"
if redis.call('SET', KEYS[1], ARGV[1], 'NX', 'PX', ARGV[2]) then
  return 1
end
if redis.call('GET', KEYS[1]) == ARGV[1] then
  redis.call('PEXPIRE', KEYS[1], ARGV[2])
  return 1
end
return 0",
        )
        .key(key)
        .arg(owner)
        .arg(ttl_ms)
        .invoke_async(&mut connection)
        .await?;
        Ok(acquired == 1)
    }

    /// 释放账号刷新租约。
    pub async fn release(
        &self,
        account_id: &str,
        owner: &str,
    ) -> RedisRefreshLeaseStoreResult<bool> {
        let mut connection = self.redis.manager();
        let released: i64 = redis::Script::new(
            r"
if redis.call('GET', KEYS[1]) == ARGV[1] then
  return redis.call('DEL', KEYS[1])
end
return 0",
        )
        .key(self.lease_key(account_id))
        .arg(serde_json::to_string(owner)?)
        .invoke_async(&mut connection)
        .await?;
        Ok(released == 1)
    }

    /// 返回给定账号集合中仍有效的刷新租约。
    pub async fn active_account_ids(
        &self,
        account_ids: &[String],
        _now: DateTime<Utc>,
    ) -> RedisRefreshLeaseStoreResult<HashSet<String>> {
        if account_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let keys = account_ids
            .iter()
            .map(|account_id| self.lease_key(account_id))
            .collect::<Vec<_>>();
        let mut connection = self.redis.manager();
        let values: Vec<Option<String>> = redis::cmd("MGET")
            .arg(&keys)
            .query_async(&mut connection)
            .await?;
        Ok(account_ids
            .iter()
            .zip(values)
            .filter_map(|(account_id, value)| value.map(|_| account_id.clone()))
            .collect())
    }

    fn lease_key(&self, account_id: &str) -> String {
        self.redis.key(&format!("lease:refresh:{account_id}"))
    }
}
