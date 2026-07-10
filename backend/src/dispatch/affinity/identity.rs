//! 会话亲和变体与 Conversation identity 构建。

use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::types::ConversationIdentity;
use crate::upstream::openai::protocol::responses::CodexResponsesRequest;

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
    let tools_json = request.tools().map_or_else(|| "[]".to_string(), tools_json);
    let mut hasher = Sha256::new();
    hasher.update(request.instructions().as_bytes());
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
        .prompt_cache_key()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let existing = existing.to_string();
        request.set_prompt_cache_key(Some(existing));
        return;
    }

    request.set_prompt_cache_key(Some(
        derive_stable_conversation_key(request).unwrap_or_else(|| Uuid::new_v4().to_string()),
    ));
}

/// 按原版 `stable-conversation-key.ts` 的规则派生稳定 conversation key。
pub fn derive_stable_conversation_key(request: &CodexResponsesRequest) -> Option<String> {
    let instructions = request
        .instructions()
        .chars()
        .take(2000)
        .collect::<String>();
    let first_user_text = first_user_text(request.input());
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
    hasher.update(request.model().as_bytes());
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
