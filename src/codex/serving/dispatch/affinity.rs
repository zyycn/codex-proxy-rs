use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use tokio::sync::RwLock;
use tokio::time::interval;

/// 会话亲和性条目 - 记录响应ID到账户的映射
#[derive(Debug, Clone)]
pub struct AffinityEntry {
    /// 账户ID
    pub account_id: String,
    /// 对话ID
    pub conversation_id: String,
    /// 上游 turn state token
    pub turn_state: Option<String>,
    /// 指令哈希（SHA-256）- 用于缓存命中检测
    pub instructions_hash: Option<String>,
    /// 输入token数
    pub input_tokens: Option<u64>,
    /// 函数调用ID列表
    pub function_call_ids: Vec<String>,
    /// 变体哈希 - 用于区分并发对话分支
    pub variant_hash: Option<String>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSessionAffinity {
    pub response_id: String,
    pub account_id: String,
    pub conversation_id: String,
    pub turn_state: Option<String>,
    pub instructions_hash: Option<String>,
    pub input_tokens: Option<u64>,
    pub function_call_ids: Vec<String>,
    pub variant_hash: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum SessionAffinityRepositoryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid session affinity timestamp: {0}")]
    InvalidTimestamp(#[from] chrono::ParseError),
    #[error("invalid session affinity function call ids: {0}")]
    InvalidFunctionCallIds(#[from] serde_json::Error),
}

pub type SessionAffinityRepositoryResult<T> = Result<T, SessionAffinityRepositoryError>;

#[derive(Clone)]
pub struct SessionAffinityRepository {
    pool: SqlitePool,
}

/// 会话亲和性映射 - 将 previous_response_id 映射到创建它的账户
///
/// 功能：
/// - 支持服务器端对话历史重用
/// - 提示缓存命中（缓存按账户存储在后端）
/// - 4小时TTL
/// - 每10分钟自动清理过期条目
pub struct SessionAffinityMap {
    map: Arc<RwLock<HashMap<String, AffinityEntry>>>,
    ttl: Duration,
    cleanup_handle: Option<tokio::task::JoinHandle<()>>,
}

const DEFAULT_TTL: Duration = Duration::from_secs(4 * 60 * 60); // 4小时
const CLEANUP_INTERVAL: Duration = Duration::from_secs(10 * 60); // 10分钟

const UPSERT_SESSION_AFFINITY_SQL: &str = r"
insert into session_affinities (
  response_id,
  account_id,
  conversation_id,
  turn_state,
  instructions_hash,
  input_tokens,
  function_call_ids_json,
  variant_hash,
  expires_at,
  created_at
) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
on conflict(response_id) do update set
  account_id = excluded.account_id,
  conversation_id = excluded.conversation_id,
  turn_state = excluded.turn_state,
  instructions_hash = excluded.instructions_hash,
  input_tokens = excluded.input_tokens,
  function_call_ids_json = excluded.function_call_ids_json,
  variant_hash = excluded.variant_hash,
  expires_at = excluded.expires_at,
  created_at = excluded.created_at";

const LIST_ACTIVE_SESSION_AFFINITIES_SQL: &str = r"
select
  response_id,
  account_id,
  conversation_id,
  turn_state,
  instructions_hash,
  input_tokens,
  function_call_ids_json,
  variant_hash,
  expires_at,
  created_at
from session_affinities
where expires_at > ?
order by created_at asc, response_id asc";

pub(super) fn compute_variant_hash(value: &impl Serialize) -> Option<String> {
    serde_json::to_string(value)
        .ok()
        .filter(|serialized| !serialized.is_empty())
}

impl SessionAffinityRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn upsert(
        &self,
        response_id: &str,
        entry: &AffinityEntry,
        ttl: Duration,
    ) -> SessionAffinityRepositoryResult<()> {
        let function_call_ids_json = serde_json::to_string(&entry.function_call_ids)?;
        let expires_at = entry.created_at + chrono_duration_from_std(ttl);
        sqlx::query(UPSERT_SESSION_AFFINITY_SQL)
            .bind(response_id)
            .bind(&entry.account_id)
            .bind(&entry.conversation_id)
            .bind(&entry.turn_state)
            .bind(&entry.instructions_hash)
            .bind(entry.input_tokens.map(u64_to_i64_saturating))
            .bind(function_call_ids_json)
            .bind(&entry.variant_hash)
            .bind(expires_at.to_rfc3339())
            .bind(entry.created_at.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_active(
        &self,
        now: DateTime<Utc>,
    ) -> SessionAffinityRepositoryResult<Vec<StoredSessionAffinity>> {
        let rows = sqlx::query(LIST_ACTIVE_SESSION_AFFINITIES_SQL)
            .bind(now.to_rfc3339())
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| stored_session_affinity_from_row(&row))
            .collect()
    }

    pub async fn delete_expired(&self, now: DateTime<Utc>) -> SessionAffinityRepositoryResult<u64> {
        let result = sqlx::query("delete from session_affinities where expires_at <= ?")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

impl SessionAffinityMap {
    pub fn new(ttl: Duration) -> Self {
        Self {
            map: Arc::new(RwLock::new(HashMap::new())),
            ttl,
            cleanup_handle: None,
        }
    }

    pub fn with_default_ttl() -> Self {
        Self::new(DEFAULT_TTL)
    }

    /// 启动自动清理任务
    pub fn start_cleanup(&mut self) {
        if self.cleanup_handle.is_some() {
            return; // 已经启动
        }

        let map = self.map.clone();
        let ttl = self.ttl;

        let handle = tokio::spawn(async move {
            let mut ticker = interval(CLEANUP_INTERVAL);
            loop {
                ticker.tick().await;
                let now = Utc::now();
                let mut map_lock = map.write().await;
                map_lock.retain(|_, entry| !entry_expired(entry.created_at, ttl, now));
            }
        });

        self.cleanup_handle = Some(handle);
    }

    /// 停止自动清理任务
    pub fn stop_cleanup(&mut self) {
        if let Some(handle) = self.cleanup_handle.take() {
            handle.abort();
        }
    }

    /// 记录响应ID到账户的映射
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        response_id: String,
        account_id: String,
        conversation_id: String,
        turn_state: Option<String>,
        instructions: Option<&str>,
        input_tokens: Option<u64>,
        function_call_ids: Option<Vec<String>>,
        variant_hash: Option<String>,
    ) -> AffinityEntry {
        let instructions_hash = instructions.map(|s| {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(s.as_bytes());
            let result = hasher.finalize();
            hex::encode(result)
        });

        let entry = AffinityEntry {
            account_id,
            conversation_id,
            turn_state,
            instructions_hash,
            input_tokens,
            function_call_ids: function_call_ids.unwrap_or_default(),
            variant_hash,
            created_at: Utc::now(),
        };

        self.map.write().await.insert(response_id, entry.clone());
        entry
    }

    pub async fn restore(&self, records: Vec<StoredSessionAffinity>) -> usize {
        let now = Utc::now();
        let mut restored = 0usize;
        let mut map = self.map.write().await;
        for record in records {
            if record.expires_at <= now || entry_expired(record.created_at, self.ttl, now) {
                continue;
            }
            map.insert(
                record.response_id,
                AffinityEntry {
                    account_id: record.account_id,
                    conversation_id: record.conversation_id,
                    turn_state: record.turn_state,
                    instructions_hash: record.instructions_hash,
                    input_tokens: record.input_tokens,
                    function_call_ids: record.function_call_ids,
                    variant_hash: record.variant_hash,
                    created_at: record.created_at,
                },
            );
            restored = restored.saturating_add(1);
        }
        restored
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// 查找响应ID对应的账户ID
    pub async fn lookup_account(&self, response_id: &str) -> Option<String> {
        self.get_entry(response_id).await.map(|e| e.account_id)
    }

    /// 查找响应ID对应的对话ID
    pub async fn lookup_conversation_id(&self, response_id: &str) -> Option<String> {
        self.get_entry(response_id).await.map(|e| e.conversation_id)
    }

    /// 查找对话ID的最新响应ID
    ///
    /// 参数：
    /// - conversation_id: 对话ID
    /// - max_age: 最大年龄（用于避免使用已过期的提示缓存）
    /// - variant_hash: 变体哈希（用于区分并发分支）
    pub async fn lookup_latest_response_by_conversation(
        &self,
        conversation_id: &str,
        max_age: Option<Duration>,
        variant_hash: Option<&str>,
    ) -> Option<String> {
        let map = self.map.read().await;
        let now = Utc::now();

        let mut latest_response_id: Option<String> = None;
        let mut latest_created_at: Option<DateTime<Utc>> = None;

        for (response_id, entry) in map.iter() {
            // 过滤对话ID
            if entry.conversation_id != conversation_id {
                continue;
            }

            // 过滤变体哈希
            if let Some(vh) = variant_hash {
                if entry.variant_hash.as_deref() != Some(vh) {
                    continue;
                }
            }

            // 检查TTL
            if entry_expired(entry.created_at, self.ttl, now) {
                continue;
            }

            // 检查最大年龄
            if let Some(max) = max_age {
                if entry_age_exceeds(entry.created_at, now, max) {
                    continue;
                }
            }

            // 保留最新的
            if latest_created_at.is_none_or(|latest| entry.created_at > latest) {
                latest_created_at = Some(entry.created_at);
                latest_response_id = Some(response_id.clone());
            }
        }

        latest_response_id
    }

    /// 查找响应ID对应的 turn state
    pub async fn lookup_turn_state(&self, response_id: &str) -> Option<String> {
        self.get_entry(response_id).await.and_then(|e| e.turn_state)
    }

    /// 查找响应ID对应的指令哈希
    pub async fn lookup_instructions_hash(&self, response_id: &str) -> Option<String> {
        self.get_entry(response_id)
            .await
            .and_then(|e| e.instructions_hash)
    }

    /// 查找响应ID对应的输入token数
    pub async fn lookup_input_tokens(&self, response_id: &str) -> Option<u64> {
        self.get_entry(response_id)
            .await
            .and_then(|e| e.input_tokens)
    }

    /// 查找响应ID对应的函数调用ID列表
    pub async fn lookup_function_call_ids(&self, response_id: &str) -> Vec<String> {
        self.get_entry(response_id)
            .await
            .map(|e| e.function_call_ids)
            .unwrap_or_default()
    }

    /// 删除响应ID（上游返回 not-found 时调用）
    pub async fn forget(&self, response_id: &str) {
        self.map.write().await.remove(response_id);
    }

    /// 获取映射大小
    pub async fn size(&self) -> usize {
        self.map.read().await.len()
    }

    /// 清空所有映射
    pub async fn clear(&self) {
        self.map.write().await.clear();
    }

    /// 获取条目（检查TTL）
    async fn get_entry(&self, response_id: &str) -> Option<AffinityEntry> {
        let map = self.map.read().await;
        let entry = map.get(response_id)?;

        let now = Utc::now();
        if entry_expired(entry.created_at, self.ttl, now) {
            return None;
        }

        Some(entry.clone())
    }
}

fn entry_expired(created_at: DateTime<Utc>, ttl: Duration, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(created_at)
        .to_std()
        .is_ok_and(|age| age >= ttl)
}

fn entry_age_exceeds(created_at: DateTime<Utc>, now: DateTime<Utc>, max_age: Duration) -> bool {
    now.signed_duration_since(created_at)
        .to_std()
        .is_ok_and(|age| age > max_age)
}

fn chrono_duration_from_std(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX))
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn optional_nonnegative_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

fn parse_rfc3339(value: &str) -> SessionAffinityRepositoryResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn stored_session_affinity_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> SessionAffinityRepositoryResult<StoredSessionAffinity> {
    let function_call_ids_json = row.get::<String, _>("function_call_ids_json");
    Ok(StoredSessionAffinity {
        response_id: row.get("response_id"),
        account_id: row.get("account_id"),
        conversation_id: row.get("conversation_id"),
        turn_state: row.get("turn_state"),
        instructions_hash: row.get("instructions_hash"),
        input_tokens: optional_nonnegative_i64_to_u64(row.get("input_tokens")),
        function_call_ids: serde_json::from_str(&function_call_ids_json)?,
        variant_hash: row.get("variant_hash"),
        expires_at: parse_rfc3339(&row.get::<String, _>("expires_at"))?,
        created_at: parse_rfc3339(&row.get::<String, _>("created_at"))?,
    })
}

impl Drop for SessionAffinityMap {
    fn drop(&mut self) {
        self.stop_cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::gateway::transport::types::CodexResponsesRequest;
    use serde_json::json;

    #[tokio::test]
    async fn session_affinity_records_and_retrieves_account() {
        let map = SessionAffinityMap::with_default_ttl();

        map.record(
            "resp_123".to_string(),
            "acc_456".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        let account = map.lookup_account("resp_123").await;
        assert_eq!(account, Some("acc_456".to_string()));
    }

    #[tokio::test]
    async fn session_affinity_returns_none_for_unknown_response() {
        let map = SessionAffinityMap::with_default_ttl();
        let account = map.lookup_account("unknown").await;
        assert_eq!(account, None);
    }

    #[tokio::test]
    async fn session_affinity_expires_old_entries() {
        let map = SessionAffinityMap::new(Duration::from_millis(100));

        map.record(
            "resp_123".to_string(),
            "acc_456".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        // 立即查找应该成功
        assert!(map.lookup_account("resp_123").await.is_some());

        // 等待过期
        tokio::time::sleep(Duration::from_millis(150)).await;

        // 过期后应该返回 None
        assert!(map.lookup_account("resp_123").await.is_none());
    }

    #[tokio::test]
    async fn session_affinity_finds_latest_response_by_conversation() {
        let map = SessionAffinityMap::with_default_ttl();

        map.record(
            "resp_1".to_string(),
            "acc_1".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        tokio::time::sleep(Duration::from_millis(10)).await;

        map.record(
            "resp_2".to_string(),
            "acc_1".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        let latest = map
            .lookup_latest_response_by_conversation("conv_789", None, None)
            .await;
        assert_eq!(latest, Some("resp_2".to_string()));
    }

    #[tokio::test]
    async fn session_affinity_forget_removes_entry() {
        let map = SessionAffinityMap::with_default_ttl();

        map.record(
            "resp_123".to_string(),
            "acc_456".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        assert!(map.lookup_account("resp_123").await.is_some());

        map.forget("resp_123").await;

        assert!(map.lookup_account("resp_123").await.is_none());
    }

    #[test]
    fn compute_variant_hash_should_follow_serialized_request_body() {
        let mut request = CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "You are Codex.",
            vec![json!({"role": "user", "content": "hello"})],
        );
        let first = compute_variant_hash(&request).unwrap();
        request.instructions = "You are Codex with different instructions.".to_string();
        let second = compute_variant_hash(&request).unwrap();

        assert_ne!(first, second);
    }
}
