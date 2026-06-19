//! Responses reasoning replay 缓存策略。

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};

const MAX_ENTRIES: usize = 512;
const MAX_ENTRY_BYTES: usize = 256 * 1024;
const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;

/// 单条 reasoning replay 缓存记录。
#[derive(Debug, Clone, PartialEq)]
struct ReasoningReplayEntry {
    account_id: String,
    conversation_id: String,
    variant_hash: String,
    items: Vec<Value>,
    byte_size: usize,
    created_at: DateTime<Utc>,
    sequence: u64,
}

/// 纯内存 reasoning replay 缓存。
#[derive(Debug, Clone)]
pub struct ReasoningReplayCache {
    entries: BTreeMap<String, ReasoningReplayEntry>,
    total_bytes: usize,
    next_sequence: u64,
    ttl: Duration,
}

impl ReasoningReplayCache {
    /// 构造指定 TTL 的缓存。
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: BTreeMap::new(),
            total_bytes: 0,
            next_sequence: 0,
            ttl,
        }
    }

    /// 记录响应对应的 replay 条目，返回实际保存的条目数。
    pub fn record(
        &mut self,
        response_id: String,
        account_id: &str,
        conversation_id: &str,
        variant_hash: &str,
        items: &[Value],
        now: DateTime<Utc>,
    ) -> usize {
        let sanitized = sanitize_reasoning_replay_items(items);
        self.cleanup_expired(now);
        self.delete_entry(&response_id);

        if sanitized.is_empty() {
            return 0;
        }
        let byte_size = estimate_items_bytes(&sanitized);
        if byte_size > MAX_ENTRY_BYTES {
            return 0;
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.total_bytes = self.total_bytes.saturating_add(byte_size);
        self.entries.insert(
            response_id,
            ReasoningReplayEntry {
                account_id: account_id.to_string(),
                conversation_id: conversation_id.to_string(),
                variant_hash: variant_hash.to_string(),
                items: sanitized,
                byte_size,
                created_at: now,
                sequence,
            },
        );
        self.evict_overflow();
        self.entries
            .values()
            .max_by_key(|entry| entry.sequence)
            .map_or(0, |entry| entry.items.len())
    }

    /// 查找指定响应与身份对应的 replay 条目。
    pub fn lookup(
        &mut self,
        response_id: &str,
        account_id: &str,
        conversation_id: &str,
        variant_hash: &str,
        now: DateTime<Utc>,
    ) -> Vec<Value> {
        self.cleanup_expired(now);
        let Some(entry) = self.entries.get(response_id) else {
            return Vec::new();
        };
        if entry.account_id != account_id
            || entry.conversation_id != conversation_id
            || entry.variant_hash != variant_hash
        {
            return Vec::new();
        }
        entry.items.clone()
    }

    /// 按账号、对话、变体驱逐 replay 条目。
    pub fn evict_by_identity(
        &mut self,
        account_id: &str,
        conversation_id: &str,
        variant_hash: &str,
        now: DateTime<Utc>,
    ) -> usize {
        self.cleanup_expired(now);
        let response_ids = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.account_id == account_id
                    && entry.conversation_id == conversation_id
                    && entry.variant_hash == variant_hash
            })
            .map(|(response_id, _)| response_id.clone())
            .collect::<Vec<_>>();
        let evicted = response_ids.len();
        for response_id in response_ids {
            self.delete_entry(&response_id);
        }
        evicted
    }

    fn cleanup_expired(&mut self, now: DateTime<Utc>) {
        let ttl = self.ttl;
        let response_ids = self
            .entries
            .iter()
            .filter(|(_, entry)| entry_expired(entry.created_at, ttl, now))
            .map(|(response_id, _)| response_id.clone())
            .collect::<Vec<_>>();
        for response_id in response_ids {
            self.delete_entry(&response_id);
        }
    }

    fn evict_overflow(&mut self) {
        while self.entries.len() > MAX_ENTRIES || self.total_bytes > MAX_TOTAL_BYTES {
            let Some(response_id) = self.oldest_response_id() else {
                return;
            };
            self.delete_entry(&response_id);
        }
    }

    fn oldest_response_id(&self) -> Option<String> {
        self.entries
            .iter()
            .min_by_key(|(_, entry)| (entry.created_at, entry.sequence))
            .map(|(response_id, _)| response_id.clone())
    }

    fn delete_entry(&mut self, response_id: &str) {
        let Some(entry) = self.entries.remove(response_id) else {
            return;
        };
        self.total_bytes = self.total_bytes.saturating_sub(entry.byte_size);
    }
}

fn sanitize_reasoning_replay_items(items: &[Value]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut sanitized = Vec::new();
    for item in items {
        let Some(replay_item) =
            sanitize_reasoning_item(item).or_else(|| sanitize_function_call(item))
        else {
            continue;
        };
        let Some(key) = replay_item_key(&replay_item) else {
            continue;
        };
        if !seen.insert(key) {
            continue;
        }
        sanitized.push(replay_item);
    }
    sanitized
}

fn sanitize_reasoning_item(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }
    let id = non_empty_str(object.get("id")?)?;
    let summary = sanitize_summary(object.get("summary")?)?;
    let encrypted_content = non_empty_str(object.get("encrypted_content")?)?;

    let mut sanitized = Map::new();
    sanitized.insert("type".to_string(), json!("reasoning"));
    sanitized.insert("id".to_string(), json!(id));
    if let Some(status) = object
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| matches!(*status, "in_progress" | "completed" | "incomplete"))
    {
        sanitized.insert("status".to_string(), json!(status));
    }
    sanitized.insert("summary".to_string(), Value::Array(summary));
    sanitized.insert("encrypted_content".to_string(), json!(encrypted_content));
    if let Some(content) = object.get("content").and_then(sanitize_content) {
        sanitized.insert("content".to_string(), Value::Array(content));
    }
    Some(Value::Object(sanitized))
}

fn sanitize_function_call(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }
    let call_id = non_empty_str(object.get("call_id")?)?;
    let name = non_empty_str(object.get("name")?)?;
    let arguments = object.get("arguments")?.as_str()?;

    let mut sanitized = Map::new();
    sanitized.insert("type".to_string(), json!("function_call"));
    if let Some(id) = object.get("id").and_then(non_empty_str) {
        sanitized.insert("id".to_string(), json!(id));
    }
    sanitized.insert("call_id".to_string(), json!(call_id));
    sanitized.insert("name".to_string(), json!(name));
    sanitized.insert("arguments".to_string(), json!(arguments));
    Some(Value::Object(sanitized))
}

fn sanitize_summary(value: &Value) -> Option<Vec<Value>> {
    Some(
        value
            .as_array()?
            .iter()
            .filter_map(|part| {
                let object = part.as_object()?;
                if object.get("type").and_then(Value::as_str) != Some("summary_text") {
                    return None;
                }
                let text = object.get("text")?.as_str()?;
                Some(json!({"type": "summary_text", "text": text}))
            })
            .collect(),
    )
}

fn sanitize_content(value: &Value) -> Option<Vec<Value>> {
    let content = value
        .as_array()?
        .iter()
        .filter_map(|part| {
            let object = part.as_object()?;
            if object.get("type").and_then(Value::as_str) != Some("reasoning_text") {
                return None;
            }
            let text = object.get("text")?.as_str()?;
            Some(json!({"type": "reasoning_text", "text": text}))
        })
        .collect::<Vec<_>>();
    (!content.is_empty()).then_some(content)
}

fn replay_item_key(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str)? {
        "reasoning" => item
            .get("id")
            .and_then(Value::as_str)
            .map(|id| format!("reasoning:{id}")),
        "function_call" => item
            .get("call_id")
            .and_then(Value::as_str)
            .map(|call_id| format!("function_call:{call_id}")),
        _ => None,
    }
}

fn non_empty_str(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}

fn estimate_items_bytes(items: &[Value]) -> usize {
    serde_json::to_vec(items).map_or(usize::MAX, |bytes| bytes.len())
}

fn entry_expired(created_at: DateTime<Utc>, ttl: Duration, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(created_at) >= ttl
}
