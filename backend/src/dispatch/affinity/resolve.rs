use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    dispatch::{
        affinity::{types::ConversationIdentity, SessionAffinityEntry, SessionAffinityService},
        recovery::{
            implicit_resume::{restore_full_request_without_history, ImplicitResumeSnapshot},
            reasoning_replay::ReasoningReplayCache,
        },
        service::ResponseDispatchService,
    },
    fleet::account::AccountStatus,
    upstream::openai::protocol::{
        events::TokenUsage,
        responses::{completed_response_metadata, CodexResponsesRequest},
    },
};

#[derive(Debug, Clone, Default)]
pub(in crate::dispatch) struct AccountAffinityDecision {
    preferred_account_id: Option<String>,
    request_history_owner: RequestHistoryOwner,
}

#[derive(Debug, Clone, Default)]
enum RequestHistoryOwner {
    #[default]
    Absent,
    Known(String),
    Unknown,
}

impl AccountAffinityDecision {
    pub(in crate::dispatch) fn preferred_account_id(&self) -> Option<&str> {
        self.preferred_account_id.as_deref()
    }

    fn should_strip_request_history_for(&self, account_id: &str) -> bool {
        match &self.request_history_owner {
            RequestHistoryOwner::Absent => false,
            RequestHistoryOwner::Known(history_account_id) => history_account_id != account_id,
            RequestHistoryOwner::Unknown => true,
        }
    }
}

impl ResponseDispatchService {
    pub(in crate::dispatch) async fn account_affinity_for_request(
        &self,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> AccountAffinityDecision {
        let has_previous_response_id = request.previous_response_id().is_some();
        if let Some(account_id) = self.previous_response_account_id(request, now).await {
            return AccountAffinityDecision {
                preferred_account_id: Some(account_id.clone()),
                request_history_owner: RequestHistoryOwner::Known(account_id),
            };
        }

        let conversation_account_id = self.conversation_account_id(request, now).await;
        AccountAffinityDecision {
            preferred_account_id: conversation_account_id,
            request_history_owner: if has_previous_response_id {
                RequestHistoryOwner::Unknown
            } else {
                RequestHistoryOwner::Absent
            },
        }
    }

    async fn previous_response_account_id(
        &self,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let previous_response_id = request.previous_response_id()?;
        self.session_affinity
            .lookup_account(previous_response_id, now)
            .await
    }

    async fn conversation_account_id(
        &self,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let conversation_id = request
            .prompt_cache_key()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let variant_hash = compute_variant_hash(request);
        self.session_affinity
            .lookup_latest_account_by_conversation(conversation_id, None, Some(&variant_hash), now)
            .await
    }

    pub(in crate::dispatch) fn strip_history_if_account_changed(
        request: &mut CodexResponsesRequest,
        implicit_resume: &mut Option<ImplicitResumeSnapshot>,
        account_affinity: &AccountAffinityDecision,
        acquired_account_id: &str,
    ) {
        if request.previous_response_id().is_none()
            || !account_affinity.should_strip_request_history_for(acquired_account_id)
        {
            return;
        }

        let Some(snapshot) = implicit_resume.take() else {
            return;
        };
        restore_full_request_without_history(request, snapshot);
    }

    pub(in crate::dispatch) async fn apply_cascading_ban_defense(
        &self,
        request: &mut CodexResponsesRequest,
        implicit_resume: &mut Option<ImplicitResumeSnapshot>,
        preferred_account_id: Option<&str>,
        acquired_account_id: &str,
    ) -> bool {
        let Some(preferred_account_id) =
            preferred_account_id.filter(|account_id| *account_id != acquired_account_id)
        else {
            return false;
        };
        let has_history = request.previous_response_id().is_some()
            || request
                .turn_state
                .as_deref()
                .is_some_and(|value| !value.is_empty());
        if !has_history {
            return false;
        }
        let Some(preferred_account) = self
            .account_pool
            .account_snapshot(preferred_account_id)
            .await
        else {
            return false;
        };
        if !matches!(
            preferred_account.status,
            AccountStatus::Banned | AccountStatus::Disabled
        ) {
            return false;
        }

        let response_id_to_forget = request.previous_response_id().map(str::to_string);
        let restored_full_request = if let Some(snapshot) = implicit_resume.take() {
            restore_full_request_without_history(request, snapshot);
            true
        } else {
            false
        };
        if let Some(response_id) = response_id_to_forget {
            self.session_affinity.forget(&response_id).await;
        }
        restored_full_request
    }
}

pub(crate) async fn record_response_affinity(
    session_affinity: &Arc<SessionAffinityService>,
    reasoning_replay: &Arc<Mutex<ReasoningReplayCache>>,
    request: &CodexResponsesRequest,
    account_id: &str,
    body: &str,
    turn_state: Option<String>,
    usage: Option<TokenUsage>,
) {
    let Some(conversation_id) = request
        .prompt_cache_key()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let metadata = match completed_response_metadata(body) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to parse completed response metadata for session affinity"
            );
            return;
        }
    };

    let variant_hash = compute_variant_hash(request);
    let entry = SessionAffinityEntry {
        account_id: account_id.to_string(),
        conversation_id: conversation_id.to_string(),
        turn_state: turn_state
            .filter(|value| !value.trim().is_empty())
            .or_else(|| request.turn_state.clone()),
        instructions_hash: Some(hash_instructions(Some(request.instructions()))),
        input_tokens: usage.map(|usage| usage.input_tokens),
        function_call_ids: metadata.function_call_ids,
        variant_hash: Some(variant_hash.clone()),
        created_at: Utc::now(),
    };
    if let Err(error) = session_affinity
        .record(metadata.response_id.clone(), entry)
        .await
    {
        tracing::warn!(
            error = %error,
            response_id = %metadata.response_id,
            account_id = %account_id,
            "failed to record session affinity"
        );
    }

    reasoning_replay.lock().await.record(
        metadata.response_id,
        account_id,
        conversation_id,
        &variant_hash,
        &metadata.replay_items,
        Utc::now(),
    );
}

pub(crate) async fn evict_reasoning_replay(
    reasoning_replay: &Arc<Mutex<ReasoningReplayCache>>,
    request: &CodexResponsesRequest,
    account_id: &str,
) {
    let variant_hash = compute_variant_hash(request);
    let conversation_id = request.prompt_cache_key().unwrap_or("").to_string();
    reasoning_replay.lock().await.evict_by_identity(
        account_id,
        &conversation_id,
        &variant_hash,
        Utc::now(),
    );
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
