//! Redis 会话亲和存储。

use chrono::{DateTime, Duration, Utc};
use redis::AsyncCommands;
use thiserror::Error;

use crate::infra::redis::RedisConnection;

use super::types::SessionAffinityEntry;

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
        let value = serde_json::to_string(entry)?;
        let expires_at = entry
            .created_at
            .checked_add_signed(ttl)
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        if expires_at <= Utc::now() {
            return Ok(());
        }

        let response_key = self.response_key(response_id);
        let conversation_key = self.conversation_key(&entry.conversation_id);
        let account_key = self.account_key(&entry.account_id);
        let ttl_seconds = ttl.num_seconds().max(1);
        let cutoff_ms = Utc::now().timestamp_millis() - ttl.num_milliseconds();
        let mut connection = self.redis.manager();
        let mut transaction = redis::pipe();
        transaction
            .atomic()
            .cmd("SET")
            .arg(response_key)
            .arg(value)
            .arg("EXAT")
            .arg(expires_at.timestamp())
            .ignore()
            .cmd("ZADD")
            .arg(&conversation_key)
            .arg(entry.created_at.timestamp_millis())
            .arg(response_id)
            .ignore()
            .cmd("EXPIRE")
            .arg(&conversation_key)
            .arg(ttl_seconds)
            .ignore()
            .cmd("SADD")
            .arg(&account_key)
            .arg(response_id)
            .ignore()
            .cmd("EXPIRE")
            .arg(account_key)
            .arg(ttl_seconds)
            .ignore()
            .cmd("ZREMRANGEBYSCORE")
            .arg(conversation_key)
            .arg("-inf")
            .arg(cutoff_ms)
            .ignore();
        let _: () = transaction.query_async(&mut connection).await?;
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
        let response_ids: Vec<String> = connection.zrevrange(&conversation_key, 0, -1).await?;
        for response_id in response_ids {
            let Some(entry) = self.get(&response_id, now, ttl).await? else {
                let _: usize = connection.zrem(&conversation_key, &response_id).await?;
                continue;
            };
            if entry.conversation_id != conversation_id {
                let _: usize = connection.zrem(&conversation_key, &response_id).await?;
                continue;
            }
            if variant_hash.is_some_and(|expected| entry.variant_hash.as_deref() != Some(expected))
            {
                continue;
            }
            if max_age.is_some_and(|max_age| now.signed_duration_since(entry.created_at) > max_age)
            {
                continue;
            }
            return Ok(Some((response_id, entry)));
        }
        Ok(None)
    }

    pub async fn forget(&self, response_id: &str) -> RedisSessionAffinityStoreResult<bool> {
        let response_key = self.response_key(response_id);
        let mut connection = self.redis.manager();
        let value: Option<String> = connection.get(&response_key).await?;
        let Some(value) = value else {
            return Ok(false);
        };
        let entry: SessionAffinityEntry = serde_json::from_str(&value)?;
        let mut transaction = redis::pipe();
        transaction
            .atomic()
            .cmd("DEL")
            .arg(response_key)
            .ignore()
            .cmd("ZREM")
            .arg(self.conversation_key(&entry.conversation_id))
            .arg(response_id)
            .ignore()
            .cmd("SREM")
            .arg(self.account_key(&entry.account_id))
            .arg(response_id)
            .ignore();
        let _: () = transaction.query_async(&mut connection).await?;
        Ok(true)
    }

    pub async fn forget_account(&self, account_id: &str) -> RedisSessionAffinityStoreResult<u64> {
        let account_key = self.account_key(account_id);
        let mut connection = self.redis.manager();
        let response_ids: Vec<String> = connection.smembers(&account_key).await?;
        let mut forgotten = 0u64;
        for response_id in response_ids {
            forgotten += u64::from(self.forget(&response_id).await?);
        }
        let _: usize = connection.del(account_key).await?;
        Ok(forgotten)
    }

    fn response_key(&self, response_id: &str) -> String {
        self.redis.key(&format!("affinity:resp:{response_id}"))
    }

    fn conversation_key(&self, conversation_id: &str) -> String {
        self.redis.key(&format!("affinity:conv:{conversation_id}"))
    }

    fn account_key(&self, account_id: &str) -> String {
        self.redis.key(&format!("affinity:account:{account_id}"))
    }
}
