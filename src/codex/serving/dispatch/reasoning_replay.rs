use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use tokio::sync::RwLock;

const REASONING_REPLAY_TTL: Duration = Duration::from_secs(55 * 60);
const MAX_ENTRIES: usize = 512;
const MAX_ENTRY_BYTES: usize = 256 * 1024;
const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
struct ReasoningReplayEntry {
    account_id: String,
    conversation_id: String,
    variant_hash: String,
    items: Vec<Value>,
    byte_size: usize,
    created_at: DateTime<Utc>,
    sequence: u64,
}

#[derive(Debug, Default)]
struct ReasoningReplayState {
    entries: HashMap<String, ReasoningReplayEntry>,
    total_bytes: usize,
    next_sequence: u64,
}

#[derive(Debug, Default)]
pub(super) struct ReasoningReplayCache {
    state: RwLock<ReasoningReplayState>,
}

impl ReasoningReplayCache {
    pub(super) async fn record(
        &self,
        response_id: &str,
        account_id: &str,
        conversation_id: &str,
        variant_hash: &str,
        items: &[Value],
    ) -> usize {
        let sanitized = sanitize_reasoning_replay_items(items);
        let mut state = self.state.write().await;
        cleanup_expired(&mut state, Utc::now());
        delete_entry(&mut state, response_id);

        if sanitized.is_empty() {
            return 0;
        }
        let byte_size = estimate_items_bytes(&sanitized);
        if byte_size > MAX_ENTRY_BYTES {
            return 0;
        }

        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        state.total_bytes = state.total_bytes.saturating_add(byte_size);
        state.entries.insert(
            response_id.to_string(),
            ReasoningReplayEntry {
                account_id: account_id.to_string(),
                conversation_id: conversation_id.to_string(),
                variant_hash: variant_hash.to_string(),
                items: sanitized,
                byte_size,
                created_at: Utc::now(),
                sequence,
            },
        );
        evict_overflow(&mut state);
        state
            .entries
            .get(response_id)
            .map_or(0, |entry| entry.items.len())
    }

    pub(super) async fn lookup(
        &self,
        response_id: &str,
        account_id: &str,
        conversation_id: &str,
        variant_hash: &str,
    ) -> Vec<Value> {
        let mut state = self.state.write().await;
        cleanup_expired(&mut state, Utc::now());
        let Some(entry) = state.entries.get(response_id) else {
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

    pub(super) async fn evict_by_identity(
        &self,
        account_id: &str,
        conversation_id: &str,
        variant_hash: &str,
    ) -> usize {
        let mut state = self.state.write().await;
        cleanup_expired(&mut state, Utc::now());
        let response_ids = state
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
            delete_entry(&mut state, &response_id);
        }
        evicted
    }
}

fn sanitize_reasoning_replay_items(items: &[Value]) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
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

fn cleanup_expired(state: &mut ReasoningReplayState, now: DateTime<Utc>) {
    let response_ids = state
        .entries
        .iter()
        .filter(|(_, entry)| entry_expired(entry.created_at, now))
        .map(|(response_id, _)| response_id.clone())
        .collect::<Vec<_>>();
    for response_id in response_ids {
        delete_entry(state, &response_id);
    }
}

fn entry_expired(created_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(created_at)
        .to_std()
        .is_ok_and(|age| age > REASONING_REPLAY_TTL)
}

fn evict_overflow(state: &mut ReasoningReplayState) {
    while state.entries.len() > MAX_ENTRIES || state.total_bytes > MAX_TOTAL_BYTES {
        let Some(response_id) = oldest_response_id(state) else {
            return;
        };
        delete_entry(state, &response_id);
    }
}

fn oldest_response_id(state: &ReasoningReplayState) -> Option<String> {
    state
        .entries
        .iter()
        .min_by_key(|(_, entry)| (entry.created_at, entry.sequence))
        .map(|(response_id, _)| response_id.clone())
}

fn delete_entry(state: &mut ReasoningReplayState, response_id: &str) {
    let Some(entry) = state.entries.remove(response_id) else {
        return;
    };
    state.total_bytes = state.total_bytes.saturating_sub(entry.byte_size);
}
