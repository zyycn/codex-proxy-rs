//! 会话亲和性策略、Conversation identity 构建、SQLite 存储与运行时会话亲和性服务。

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    infra::time::parse_rfc3339_utc as parse_rfc3339,
    upstream::protocol::responses::CodexResponsesRequest,
};

// ====================================================================
// 亲和性核心类型
// ====================================================================

/// 会话亲和性条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAffinityEntry {
    pub account_id: String,
    pub conversation_id: String,
    pub turn_state: Option<String>,
    pub instructions_hash: Option<String>,
    pub input_tokens: Option<u64>,
    pub function_call_ids: Vec<String>,
    pub variant_hash: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// 持久化的会话亲和性条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSessionAffinity {
    pub response_id: String,
    pub entry: SessionAffinityEntry,
    pub expires_at: DateTime<Utc>,
}

/// 纯内存会话亲和性映射。
#[derive(Debug, Clone)]
pub struct SessionAffinityMap {
    entries: BTreeMap<String, SessionAffinityEntry>,
    ttl: Duration,
}

impl SessionAffinityMap {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: BTreeMap::new(),
            ttl,
        }
    }

    pub fn record(&mut self, response_id: String, entry: SessionAffinityEntry) {
        self.entries.insert(response_id, entry);
    }

    pub fn restore(&mut self, records: Vec<StoredSessionAffinity>, now: DateTime<Utc>) -> usize {
        let mut restored = 0usize;
        for record in records {
            if self.stored_record_is_active(&record, now) {
                self.entries.insert(record.response_id, record.entry);
                restored = restored.saturating_add(1);
            }
        }
        restored
    }

    pub fn lookup_account(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.active_entry(response_id, now)
            .map(|entry| entry.account_id.clone())
    }

    pub fn lookup_conversation_id(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.active_entry(response_id, now)
            .map(|entry| entry.conversation_id.clone())
    }

    pub fn lookup_turn_state(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.active_entry(response_id, now)
            .and_then(|entry| entry.turn_state.clone())
    }

    pub fn lookup_instructions_hash(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.active_entry(response_id, now)
            .and_then(|entry| entry.instructions_hash.clone())
    }

    pub fn lookup_function_call_ids(&self, response_id: &str, now: DateTime<Utc>) -> Vec<String> {
        self.active_entry(response_id, now)
            .map(|entry| entry.function_call_ids.clone())
            .unwrap_or_default()
    }

    pub fn lookup_latest_response_by_conversation(
        &self,
        conversation_id: &str,
        max_age: Option<Duration>,
        variant_hash: Option<&str>,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.conversation_id == conversation_id)
            .filter(|(_, entry)| self.entry_is_active(entry, now))
            .filter(|(_, entry)| {
                variant_hash
                    .is_none_or(|variant_hash| entry.variant_hash.as_deref() == Some(variant_hash))
            })
            .filter(|(_, entry)| {
                max_age.is_none_or(|max_age| now.signed_duration_since(entry.created_at) <= max_age)
            })
            .max_by_key(|(_, entry)| entry.created_at)
            .map(|(response_id, _)| response_id.clone())
    }

    pub fn forget(&mut self, response_id: &str) -> bool {
        self.entries.remove(response_id).is_some()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn active_entry(&self, response_id: &str, now: DateTime<Utc>) -> Option<&SessionAffinityEntry> {
        self.entries
            .get(response_id)
            .filter(|entry| self.entry_is_active(entry, now))
    }

    fn stored_record_is_active(&self, record: &StoredSessionAffinity, now: DateTime<Utc>) -> bool {
        record.expires_at > now && self.entry_is_active(&record.entry, now)
    }

    fn entry_is_active(&self, entry: &SessionAffinityEntry, now: DateTime<Utc>) -> bool {
        entry
            .created_at
            .checked_add_signed(self.ttl)
            .is_some_and(|expires_at| expires_at > now)
    }
}

// ====================================================================
// Variant identity helpers
// ====================================================================

/// 准备用于区分并发分支的变体身份。
pub fn prepare_variant_identity(request: &mut CodexResponsesRequest) {
    request.variant_identity = build_variant_identity(request);
}

/// 计算请求变体哈希。
pub fn compute_variant_hash(request: &CodexResponsesRequest) -> String {
    compute_variant_hash_with_identity(request, request.variant_identity.as_deref())
}

fn compute_variant_hash_with_identity(
    request: &CodexResponsesRequest,
    identity: Option<&str>,
) -> String {
    let tools_json = request
        .tools
        .as_ref()
        .map_or_else(|| "[]".to_string(), |tools| tools_json(tools));
    let mut hasher = Sha256::new();
    hasher.update(request.instructions.as_bytes());
    hasher.update(b"\0");
    hasher.update(tools_json.as_bytes());
    if let Some(identity) = identity
        .map(str::trim)
        .filter(|identity| !identity.is_empty())
    {
        hasher.update(b"\0");
        hasher.update(identity.as_bytes());
    }
    hex::encode(hasher.finalize()).chars().take(12).collect()
}

fn tools_json(tools: &[Value]) -> String {
    serde_json::to_string(tools).unwrap_or_else(|_| "[]".to_string())
}

fn build_variant_identity(request: &CodexResponsesRequest) -> Option<String> {
    let mut parts = Vec::with_capacity(2);
    if let Some(window_id) = non_empty_str(request.codex_window_id.as_deref()) {
        parts.push(format!("window:{window_id}"));
    }
    if request.explicit_prompt_cache_key
        || non_empty_str(request.client_conversation_id.as_deref()).is_some()
    {
        if let Some(anchor) = derive_stable_conversation_key(request) {
            parts.push(format!("anchor:{anchor}"));
        }
    }

    (!parts.is_empty()).then(|| parts.join("\0"))
}

fn non_empty_str(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// 计算 instructions 哈希。
pub fn hash_instructions(instructions: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(instructions.unwrap_or_default().as_bytes());
    hex::encode(hasher.finalize())
}

// ====================================================================
// Conversation identity 构建器
// ====================================================================

const LEADING_SYSTEM_REMINDER_OPEN: &str = "<system-reminder>";
const LEADING_SYSTEM_REMINDER_CLOSE: &str = "</system-reminder>";

/// 从请求上下文派生的 conversation identity
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationIdentity {
    pub conversation_id: Option<String>,
    pub window_id: Option<String>,
}

/// 从 prompt_cache_key 和可选的 window_id 构建 conversation identity
pub fn build_conversation_identity(
    prompt_cache_key: Option<&str>,
    client_window_id: Option<&str>,
    account_scope: &str,
) -> ConversationIdentity {
    let conversation_id = prompt_cache_key
        .filter(|s| !s.trim().is_empty())
        .map(|key| build_account_scoped_identity("conversation", account_scope, key));

    let window_id = if let Some(client_win) = client_window_id.filter(|s| !s.trim().is_empty()) {
        Some(build_account_scoped_identity(
            "window",
            account_scope,
            client_win,
        ))
    } else {
        conversation_id
            .as_ref()
            .map(|conv_id| format!("{}:0", conv_id))
    };

    ConversationIdentity {
        conversation_id,
        window_id,
    }
}

/// 确保请求拥有上游可复用的 prompt cache key。
pub fn ensure_prompt_cache_key(request: &mut CodexResponsesRequest) {
    if let Some(existing) = request
        .prompt_cache_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        request.prompt_cache_key = Some(existing.to_string());
        return;
    }

    request.prompt_cache_key =
        Some(derive_stable_conversation_key(request).unwrap_or_else(|| Uuid::new_v4().to_string()));
}

/// 按原版 `stable-conversation-key.ts` 的规则派生稳定 conversation key。
pub fn derive_stable_conversation_key(request: &CodexResponsesRequest) -> Option<String> {
    let instructions = request.instructions.chars().take(2000).collect::<String>();
    let first_user_text = first_user_text(&request.input);
    let normalized_first_user_text = normalize_conversation_anchor_text(&first_user_text);
    let first_user_text = if normalized_first_user_text.is_empty() {
        first_user_text
    } else {
        normalized_first_user_text
    };
    if instructions.is_empty() && first_user_text.is_empty() {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(request.model.as_bytes());
    hasher.update(b"\0");
    hasher.update(instructions.as_bytes());
    hasher.update(b"\0");
    hasher.update(first_user_text.as_bytes());
    let hex = hex::encode(hasher.finalize());

    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

fn first_user_text(input: &[Value]) -> String {
    for item in input {
        if item.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(content) = item.get("content") else {
            return String::new();
        };
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(parts) = content.as_array() {
            return parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("input_text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<String>();
        }
        return String::new();
    }

    String::new()
}

fn normalize_conversation_anchor_text(text: &str) -> String {
    let mut rest = text.trim_start();
    loop {
        let lower = rest.to_ascii_lowercase();
        if !lower.starts_with(LEADING_SYSTEM_REMINDER_OPEN) {
            break;
        }
        let Some(close_start) = lower.find(LEADING_SYSTEM_REMINDER_CLOSE) else {
            break;
        };
        let close_end = close_start + LEADING_SYSTEM_REMINDER_CLOSE.len();
        rest = rest[close_end..].trim_start();
    }
    rest.to_string()
}

/// 构建账号作用域的身份哈希。
fn build_account_scoped_identity(kind: &str, account_scope: &str, client_value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(account_scope.as_bytes());
    hasher.update(b"\0");
    hasher.update(client_value.as_bytes());

    let digest = hasher.finalize();
    let hex = hex::encode(digest);
    let truncated = &hex[..32];

    let prefix = match kind {
        "conversation" => "cp",
        "window" => "cw",
        _ => "cx",
    };

    format!("{}_{}", prefix, truncated)
}

// ====================================================================
// SQLite 会话亲和性存储
// ====================================================================

/// SQLite 会话亲和性存储。
#[derive(Clone)]
pub struct SqliteSessionAffinityStore {
    pool: SqlitePool,
}

/// SQLite 会话亲和性存储错误。
#[derive(Debug, Error)]
pub enum SqliteSessionAffinityStoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid session affinity timestamp: {0}")]
    InvalidTimestamp(#[from] chrono::ParseError),
    #[error("invalid session affinity function call ids: {0}")]
    InvalidFunctionCallIds(#[from] serde_json::Error),
}

pub type SqliteSessionAffinityStoreResult<T> = Result<T, SqliteSessionAffinityStoreError>;

impl SqliteSessionAffinityStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

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

    pub async fn forget(&self, response_id: &str) -> SqliteSessionAffinityStoreResult<bool> {
        let result = sqlx::query("delete from session_affinities where response_id = ?")
            .bind(response_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
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

fn optional_nonnegative_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

// ====================================================================
// 运行时会话亲和性服务
// ====================================================================

use std::sync::Arc;
use tokio::sync::RwLock;

/// 默认会话亲和性 TTL 秒数。
const DEFAULT_SESSION_AFFINITY_TTL_SECS: i64 = 4 * 60 * 60;

/// 运行时会话亲和性服务。
#[derive(Clone)]
pub struct RuntimeSessionAffinityService {
    store: SqliteSessionAffinityStore,
    map: Arc<RwLock<SessionAffinityMap>>,
    ttl: Duration,
}

/// 运行时会话亲和性错误。
#[derive(Debug, Error)]
pub enum RuntimeSessionAffinityError {
    #[error("session affinity store error: {0}")]
    Store(#[from] SqliteSessionAffinityStoreError),
}

impl RuntimeSessionAffinityService {
    pub fn new(store: SqliteSessionAffinityStore) -> Self {
        let ttl = Duration::seconds(DEFAULT_SESSION_AFFINITY_TTL_SECS);
        Self {
            store,
            map: Arc::new(RwLock::new(SessionAffinityMap::new(ttl))),
            ttl,
        }
    }

    pub async fn restore_from_repository(
        &self,
        now: DateTime<Utc>,
    ) -> Result<usize, RuntimeSessionAffinityError> {
        let records = self.store.list_active(now).await?;
        Ok(self.map.write().await.restore(records, now))
    }

    pub async fn record(
        &self,
        response_id: String,
        entry: SessionAffinityEntry,
    ) -> Result<(), RuntimeSessionAffinityError> {
        self.store.upsert(&response_id, &entry, self.ttl).await?;
        self.map.write().await.record(response_id, entry);
        Ok(())
    }

    pub async fn lookup_account(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.map.read().await.lookup_account(response_id, now)
    }

    pub async fn lookup_conversation_id(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.map
            .read()
            .await
            .lookup_conversation_id(response_id, now)
    }

    pub async fn lookup_turn_state(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.map.read().await.lookup_turn_state(response_id, now)
    }

    pub async fn lookup_instructions_hash(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.map
            .read()
            .await
            .lookup_instructions_hash(response_id, now)
    }

    pub async fn lookup_function_call_ids(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        self.map
            .read()
            .await
            .lookup_function_call_ids(response_id, now)
    }

    pub async fn lookup_latest_response_by_conversation(
        &self,
        conversation_id: &str,
        max_age: Option<Duration>,
        variant_hash: Option<&str>,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.map
            .read()
            .await
            .lookup_latest_response_by_conversation(conversation_id, max_age, variant_hash, now)
    }

    pub async fn forget(&self, response_id: &str) -> bool {
        let in_memory = self.map.write().await.forget(response_id);
        match self.store.forget(response_id).await {
            Ok(persisted) => in_memory || persisted,
            Err(error) => {
                tracing::warn!(
                    response_id,
                    error = %error,
                    "failed to persist session affinity removal"
                );
                in_memory
            }
        }
    }
}
