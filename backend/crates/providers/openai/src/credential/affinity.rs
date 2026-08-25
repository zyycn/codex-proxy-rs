//! OpenAI 根会话锚点到 Store 不透明账号亲和键的单向派生。

use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::ProviderSessionAffinityKey;
use sha2::{Digest, Sha256};

use crate::transport::protocol::responses::CodexResponsesRequest;
use crate::transport::request::derive_conversation_anchor;

pub(crate) fn derive_codex_session_affinity_key(
    request: &CodexResponsesRequest,
    client_api_key_id: &ClientApiKeyId,
) -> Option<ProviderSessionAffinityKey> {
    let (domain, value) = derive_account_affinity_anchor(request)?;
    let session_key = opaque_affinity_key(domain, &value)?;
    opaque_affinity_key(
        "client-session",
        &format!(
            "{}\0{}",
            client_api_key_id.as_str(),
            session_key.expose_to_store()
        ),
    )
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
