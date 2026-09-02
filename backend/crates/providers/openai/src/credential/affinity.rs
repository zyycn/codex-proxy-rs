//! OpenAI 根会话锚点到 Store 不透明账号亲和键及诊断上下文的单向派生。

use std::time::Duration;

use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::ProviderSessionAffinityKey;
use sha2::{Digest, Sha256};

use crate::transport::protocol::responses::CodexResponsesRequest;
use crate::transport::request::derive_conversation_anchor;

const AFFINITY_KEY_HASH_LENGTH: usize = 12;
pub(crate) const CODEX_ROOT_SESSION_TTL: Duration = Duration::from_secs(4 * 60 * 60);

/// 一次请求派生出的账号亲和键及其结构化日志上下文。
pub(crate) struct CodexSessionAffinity {
    key: ProviderSessionAffinityKey,
    key_hash: String,
    anchor_source: &'static str,
    anchor: String,
    session_id: Option<String>,
}

impl CodexSessionAffinity {
    #[must_use]
    pub(crate) const fn key(&self) -> &ProviderSessionAffinityKey {
        &self.key
    }

    #[must_use]
    pub(crate) fn key_hash(&self) -> &str {
        &self.key_hash
    }

    /// 返回可持久化的客户端作用域不透明会话关联值。
    #[must_use]
    pub(crate) fn persistence_hash(&self) -> &str {
        self.key.expose_to_store()
    }

    #[must_use]
    pub(crate) const fn anchor_source(&self) -> &'static str {
        self.anchor_source
    }

    #[must_use]
    pub(crate) fn anchor(&self) -> &str {
        &self.anchor
    }

    #[must_use]
    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    #[must_use]
    pub(crate) const fn session_id_present(&self) -> bool {
        self.session_id.is_some()
    }

    #[must_use]
    pub(crate) fn into_key(self) -> ProviderSessionAffinityKey {
        self.key
    }
}

/// 将原始 response ID 投影为客户端作用域的不可逆关联值。
#[must_use]
pub(crate) fn derive_previous_response_id_hash(
    previous_response_id: &str,
    client_api_key_id: &ClientApiKeyId,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-previous-response-observation-v1\0");
    hasher.update(client_api_key_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(previous_response_id.as_bytes());
    hex::encode(hasher.finalize())
}

pub(crate) fn derive_codex_session_affinity(
    request: &CodexResponsesRequest,
    client_api_key_id: &ClientApiKeyId,
) -> Option<CodexSessionAffinity> {
    let session_id = non_empty(request.client_session_id.as_deref()).map(str::to_owned);
    let (anchor_source, anchor) = derive_account_affinity_anchor(request)?;
    let session_key = opaque_affinity_key(anchor_source, &anchor)?;
    let key = opaque_affinity_key(
        "client-session",
        &format!(
            "{}\0{}",
            client_api_key_id.as_str(),
            session_key.expose_to_store()
        ),
    )?;
    // 亲和键本身已经是 SHA-256；日志沿用 WebSocket 诊断的 12 位短哈希长度。
    let key_hash = key
        .expose_to_store()
        .chars()
        .take(AFFINITY_KEY_HASH_LENGTH)
        .collect();
    Some(CodexSessionAffinity {
        key,
        key_hash,
        anchor_source,
        anchor,
        session_id,
    })
}

/// 账号选择优先使用根会话事实；`local_conversation_id` 仍可按 child thread 隔离
/// WebSocket 与 continuation 状态，但 child 身份不再拆分账号首选项。账号不可调度时
/// selector 仍可按既有策略切换，并由 request scope 清理账号绑定状态。
fn derive_account_affinity_anchor(
    request: &CodexResponsesRequest,
) -> Option<(&'static str, String)> {
    non_empty(request.client_session_id.as_deref())
        .map(|value| ("root-session", value.to_owned()))
        .or_else(|| {
            non_empty(request.client_conversation_id.as_deref())
                .map(|value| ("root-conversation", value.to_owned()))
        })
        .or_else(|| {
            request
                .explicit_prompt_cache_key
                .then(|| request.prompt_cache_key())
                .flatten()
                .and_then(|value| non_empty(Some(value)))
                .map(|value| ("root-prompt-cache", value.to_owned()))
        })
        .or_else(|| {
            non_empty(request.local_conversation_id.as_deref())
                .map(|value| ("local-conversation", value.to_owned()))
        })
        .or_else(|| derive_conversation_anchor(request))
}

fn opaque_affinity_key(domain: &str, value: &str) -> Option<ProviderSessionAffinityKey> {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-session-affinity-v1\0");
    hasher.update(domain.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.as_bytes());
    ProviderSessionAffinityKey::try_new(hex::encode(hasher.finalize())).ok()
}

/// 恢复旧版 `cyber_policy` 的会话隔离键。
///
/// 它只接受显式 session/conversation 或客户端明确给出的 prompt cache key，避免将
/// 请求内容哈希误当成长会话；`previous_response_id` 续写不参与该策略。
pub(crate) fn derive_codex_cyber_policy_session_key(
    request: &CodexResponsesRequest,
    client_api_key_id: &ClientApiKeyId,
) -> Option<ProviderSessionAffinityKey> {
    if request.previous_response_id().is_some() {
        return None;
    }
    let session_id = non_empty(request.client_session_id.as_deref())
        .or_else(|| non_empty(request.client_conversation_id.as_deref()))
        .or_else(|| {
            request
                .explicit_prompt_cache_key
                .then(|| request.prompt_cache_key())
                .flatten()
                .and_then(|value| non_empty(Some(value)))
        })?;
    let mut hasher = Sha256::new();
    hasher.update(b"cyber-policy-session\0");
    hasher.update(client_api_key_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(session_id.as_bytes());
    ProviderSessionAffinityKey::try_new(hex::encode(hasher.finalize())).ok()
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
