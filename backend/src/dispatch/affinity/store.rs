//! Redis 会话亲和存储。

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use redis::AsyncCommands;
use serde_json::Value;
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
            .cmd("ZADD")
            .arg(&account_key)
            .arg(entry.created_at.timestamp_millis())
            .arg(response_id)
            .ignore()
            .cmd("EXPIRE")
            .arg(&account_key)
            .arg(ttl_seconds)
            .ignore()
            .cmd("ZREMRANGEBYSCORE")
            .arg(conversation_key)
            .arg("-inf")
            .arg(cutoff_ms)
            .ignore()
            .cmd("ZREMRANGEBYSCORE")
            .arg(&account_key)
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

    pub async fn replay_input(
        &self,
        response_id: &str,
        head: &SessionAffinityEntry,
        now: DateTime<Utc>,
        ttl: Duration,
        max_depth: u16,
        max_total_bytes: u64,
    ) -> RedisSessionAffinityStoreResult<Option<Vec<Value>>> {
        let Some(head_snapshot) = head.replay.as_ref() else {
            return Ok(None);
        };
        if head_snapshot.depth == 0
            || head_snapshot.depth > max_depth
            || head_snapshot.total_bytes > max_total_bytes
        {
            return Ok(None);
        }

        let conversation_key = self.conversation_key(&head.conversation_id);
        let mut connection = self.redis.manager();
        let response_ids: Vec<String> = redis::cmd("ZREVRANGEBYSCORE")
            .arg(&conversation_key)
            .arg(head.created_at.timestamp_millis())
            .arg("-inf")
            .arg("LIMIT")
            .arg(0)
            .arg(max_depth)
            .query_async(&mut connection)
            .await?;
        if !response_ids
            .iter()
            .any(|candidate| candidate == response_id)
        {
            return Ok(None);
        }

        let response_keys = response_ids
            .iter()
            .map(|candidate| self.response_key(candidate))
            .collect::<Vec<_>>();
        let values: Vec<Option<String>> = redis::cmd("MGET")
            .arg(&response_keys)
            .query_async(&mut connection)
            .await?;
        let mut entries = HashMap::with_capacity(values.len());
        for (candidate, value) in response_ids.into_iter().zip(values) {
            let Some(value) = value else {
                continue;
            };
            let entry: SessionAffinityEntry = serde_json::from_str(&value)?;
            if entry.conversation_id == head.conversation_id
                && entry
                    .created_at
                    .checked_add_signed(ttl)
                    .is_some_and(|expires_at| expires_at > now)
            {
                entries.insert(candidate, entry);
            }
        }

        let mut current_id = response_id.to_string();
        let mut expected_depth = head_snapshot.depth;
        let mut expected_total_bytes = head_snapshot.total_bytes;
        let mut nodes = Vec::with_capacity(usize::from(expected_depth));
        loop {
            let Some(entry) = entries.get(&current_id) else {
                return Ok(None);
            };
            let Some(snapshot) = entry.replay.as_ref() else {
                return Ok(None);
            };
            if snapshot.depth != expected_depth || snapshot.total_bytes != expected_total_bytes {
                return Ok(None);
            }
            let node_bytes = replay_node_bytes(snapshot)?;
            if node_bytes > expected_total_bytes {
                return Ok(None);
            }
            nodes.push((snapshot.turn_input.clone(), snapshot.turn_output.clone()));

            match snapshot.parent_response_id.as_deref() {
                None if expected_depth == 1 && expected_total_bytes == node_bytes => break,
                Some(parent_response_id) if expected_depth > 1 => {
                    expected_depth -= 1;
                    expected_total_bytes -= node_bytes;
                    current_id = parent_response_id.to_string();
                }
                None | Some(_) => return Ok(None),
            }
        }

        nodes.reverse();
        let mut full_input = Vec::new();
        for (mut input, mut output) in nodes {
            full_input.append(&mut input);
            full_input.append(&mut output);
        }
        Ok(Some(full_input))
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
            .cmd("ZREM")
            .arg(self.account_key(&entry.account_id))
            .arg(response_id)
            .ignore();
        let _: () = transaction.query_async(&mut connection).await?;
        Ok(true)
    }

    pub async fn forget_account(&self, account_id: &str) -> RedisSessionAffinityStoreResult<u64> {
        let account_key = self.account_key(account_id);
        let mut connection = self.redis.manager();
        let response_ids: Vec<String> = connection.zrange(&account_key, 0, -1).await?;
        if response_ids.is_empty() {
            let _: usize = connection.del(account_key).await?;
            return Ok(0);
        }

        let response_keys = response_ids
            .iter()
            .map(|response_id| self.response_key(response_id))
            .collect::<Vec<_>>();
        let values: Vec<Option<String>> = redis::cmd("MGET")
            .arg(&response_keys)
            .query_async(&mut connection)
            .await?;
        let mut conversation_members = Vec::with_capacity(response_ids.len());
        for (response_id, value) in response_ids.iter().zip(values) {
            let Some(value) = value else {
                continue;
            };
            match serde_json::from_str::<SessionAffinityEntry>(&value) {
                Ok(entry) if entry.account_id == account_id => {
                    conversation_members.push((response_id, entry.conversation_id));
                }
                Ok(_) => {
                    tracing::warn!(
                        response_id,
                        account_id,
                        "account affinity index points to another account"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        response_id,
                        account_id,
                        error = %error,
                        "invalid account affinity entry removed during account cleanup"
                    );
                }
            }
        }

        let mut transaction = redis::pipe();
        transaction.atomic().cmd("DEL").arg(&response_keys).ignore();
        for (response_id, conversation_id) in &conversation_members {
            transaction
                .cmd("ZREM")
                .arg(self.conversation_key(conversation_id))
                .arg(response_id)
                .ignore();
        }
        transaction.cmd("DEL").arg(account_key).ignore();
        let _: () = transaction.query_async(&mut connection).await?;
        Ok(response_ids.len() as u64)
    }

    fn response_key(&self, response_id: &str) -> String {
        self.redis.key(&format!("affinity:v2:resp:{response_id}"))
    }

    fn conversation_key(&self, conversation_id: &str) -> String {
        self.redis
            .key(&format!("affinity:v2:conv:{conversation_id}"))
    }

    fn account_key(&self, account_id: &str) -> String {
        self.redis.key(&format!("affinity:v2:account:{account_id}"))
    }
}

fn replay_node_bytes(
    snapshot: &super::types::ResponseReplaySnapshot,
) -> Result<u64, serde_json::Error> {
    serde_json::to_vec(&(&snapshot.turn_input, &snapshot.turn_output))
        .map(|value| value.len() as u64)
}
