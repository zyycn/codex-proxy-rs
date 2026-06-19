//! 会话亲和性策略。

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    gateway::conversation::derive_stable_conversation_key,
    protocol::codex::responses::CodexResponsesRequest,
};

/// 会话亲和性键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAffinityKey {
    /// 客户端会话 ID。
    pub conversation_id: String,
}

/// 会话亲和性条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAffinityEntry {
    /// 创建该响应的账号 ID。
    pub account_id: String,
    /// 对话 ID。
    pub conversation_id: String,
    /// 上游 turn state。
    pub turn_state: Option<String>,
    /// 指令哈希。
    pub instructions_hash: Option<String>,
    /// 输入 token 数。
    pub input_tokens: Option<u64>,
    /// 函数调用 ID 列表。
    pub function_call_ids: Vec<String>,
    /// 用于区分并发对话分支的变体哈希。
    pub variant_hash: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 持久化的会话亲和性条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSessionAffinity {
    /// 响应 ID。
    pub response_id: String,
    /// 亲和性条目。
    pub entry: SessionAffinityEntry,
    /// 持久化过期时间。
    pub expires_at: DateTime<Utc>,
}

/// 纯内存会话亲和性映射。
#[derive(Debug, Clone)]
pub struct SessionAffinityMap {
    entries: BTreeMap<String, SessionAffinityEntry>,
    ttl: Duration,
}

impl SessionAffinityMap {
    /// 构造指定 TTL 的会话亲和性映射。
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: BTreeMap::new(),
            ttl,
        }
    }

    /// 记录响应 ID 到亲和性条目的映射。
    pub fn record(&mut self, response_id: String, entry: SessionAffinityEntry) {
        self.entries.insert(response_id, entry);
    }

    /// 从持久化记录恢复未过期条目。
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

    /// 根据响应 ID 查找账号 ID。
    pub fn lookup_account(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.active_entry(response_id, now)
            .map(|entry| entry.account_id.clone())
    }

    /// 根据响应 ID 查找对话 ID。
    pub fn lookup_conversation_id(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.active_entry(response_id, now)
            .map(|entry| entry.conversation_id.clone())
    }

    /// 根据响应 ID 查找 turn state。
    pub fn lookup_turn_state(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.active_entry(response_id, now)
            .and_then(|entry| entry.turn_state.clone())
    }

    /// 根据响应 ID 查找指令哈希。
    pub fn lookup_instructions_hash(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.active_entry(response_id, now)
            .and_then(|entry| entry.instructions_hash.clone())
    }

    /// 根据响应 ID 查找函数调用 ID 列表。
    pub fn lookup_function_call_ids(&self, response_id: &str, now: DateTime<Utc>) -> Vec<String> {
        self.active_entry(response_id, now)
            .map(|entry| entry.function_call_ids.clone())
            .unwrap_or_default()
    }

    /// 查找指定对话和可选变体下最新的响应 ID。
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

    /// 删除响应 ID 映射。
    pub fn forget(&mut self, response_id: &str) -> bool {
        self.entries.remove(response_id).is_some()
    }

    /// 返回当前内存条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 返回是否没有内存条目。
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
