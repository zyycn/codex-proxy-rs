//! OpenAI 会话锚点到 Store 不透明亲和键的单向派生。

use gateway_core::provider_ports::ProviderSessionAffinityKey;
use sha2::{Digest, Sha256};

use crate::transport::protocol::responses::CodexResponsesRequest;
use crate::transport::request::derive_conversation_anchor;

pub(crate) fn derive_codex_session_affinity_key(
    request: &CodexResponsesRequest,
) -> Option<ProviderSessionAffinityKey> {
    let (domain, value) = derive_conversation_anchor(request)?;
    let mut hasher = Sha256::new();
    hasher.update(b"codex-session-affinity-v1\0");
    hasher.update(domain.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.as_bytes());
    ProviderSessionAffinityKey::try_new(hex::encode(hasher.finalize())).ok()
}
