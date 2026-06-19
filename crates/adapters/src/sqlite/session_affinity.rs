//! SQLite 会话亲和性存储。

use chrono::{DateTime, Duration, Utc};
use codex_proxy_core::serving::affinity::{SessionAffinityEntry, StoredSessionAffinity};
use sqlx::SqlitePool;
use thiserror::Error;

/// SQLite 会话亲和性存储。
#[derive(Clone)]
pub struct SqliteSessionAffinityStore {
    pool: SqlitePool,
}

/// SQLite 会话亲和性存储错误。
#[derive(Debug, Error)]
pub enum SqliteSessionAffinityStoreError {
    /// 数据库错误。
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// 时间戳格式无效。
    #[error("invalid session affinity timestamp: {0}")]
    InvalidTimestamp(#[from] chrono::ParseError),
    /// 函数调用 ID JSON 无效。
    #[error("invalid session affinity function call ids: {0}")]
    InvalidFunctionCallIds(#[from] serde_json::Error),
}

/// SQLite 会话亲和性存储结果。
pub type SqliteSessionAffinityStoreResult<T> = Result<T, SqliteSessionAffinityStoreError>;

impl SqliteSessionAffinityStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 返回底层连接池。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 插入或更新响应 ID 的会话亲和性条目。
    pub async fn upsert(
        &self,
        response_id: &str,
        entry: &SessionAffinityEntry,
        ttl: Duration,
    ) -> SqliteSessionAffinityStoreResult<()> {
        let function_call_ids_json = serde_json::to_string(&entry.function_call_ids)?;
        let expires_at = entry
            .created_at
            .checked_add_signed(ttl)
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        sqlx::query(
            r"
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
  created_at = excluded.created_at",
        )
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

    /// 列出当前未过期的会话亲和性条目。
    pub async fn list_active(
        &self,
        now: DateTime<Utc>,
    ) -> SqliteSessionAffinityStoreResult<Vec<StoredSessionAffinity>> {
        let rows = sqlx::query(
            r"
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
order by created_at asc, response_id asc",
        )
        .bind(now.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| stored_session_affinity_from_row(&row))
            .collect()
    }

    /// 删除已过期的会话亲和性条目。
    pub async fn delete_expired(
        &self,
        now: DateTime<Utc>,
    ) -> SqliteSessionAffinityStoreResult<u64> {
        let result = sqlx::query("delete from session_affinities where expires_at <= ?")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

fn stored_session_affinity_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> SqliteSessionAffinityStoreResult<StoredSessionAffinity> {
    use sqlx::Row as _;

    let function_call_ids_json = row.get::<String, _>("function_call_ids_json");
    Ok(StoredSessionAffinity {
        response_id: row.get("response_id"),
        entry: SessionAffinityEntry {
            account_id: row.get("account_id"),
            conversation_id: row.get("conversation_id"),
            turn_state: row.get("turn_state"),
            instructions_hash: row.get("instructions_hash"),
            input_tokens: optional_nonnegative_i64_to_u64(row.get("input_tokens")),
            function_call_ids: serde_json::from_str(&function_call_ids_json)?,
            variant_hash: row.get("variant_hash"),
            created_at: parse_rfc3339(&row.get::<String, _>("created_at"))?,
        },
        expires_at: parse_rfc3339(&row.get::<String, _>("expires_at"))?,
    })
}

fn parse_rfc3339(value: &str) -> SqliteSessionAffinityStoreResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn optional_nonnegative_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}
