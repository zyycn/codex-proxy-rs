//! 在协议转换前识别会话，按 Client Key 和模型隔离账号绑定

use base64::{Engine as _, engine::general_purpose::STANDARD};
use gateway_core::{
    operation::Operation, policy::ClientApiKeyId, provider_ports::ProviderSessionAffinityKey,
    routing::UpstreamModelId,
};
use gateway_protocol::{anthropic, genai, openai};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

pub(super) fn session_key(
    operation: &Operation,
    client: &ClientApiKeyId,
    model: &UpstreamModelId,
) -> Option<ProviderSessionAffinityKey> {
    let Operation::Generate(generate) = operation else {
        return None;
    };
    let payload = generate.protocol_payload();
    let session = match payload.protocol() {
        openai::OPENAI_PROTOCOL => {
            openai::codex_account_session_id(payload.body(), payload.context())
        }
        anthropic::ANTHROPIC_PROTOCOL => anthropic::claude_session_id(
            payload.body(),
            header(payload.context(), "x-claude-code-session-id").as_deref(),
        ),
        genai::GENAI_PROTOCOL => header(payload.context(), "x-http-session-id")
            .or_else(|| header(payload.context(), "x-session-id"))
            .or_else(|| {
                payload
                    .body()
                    .get("sessionId")
                    .or_else(|| payload.body().get("session_id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
        _ => None,
    }?;
    let session = session.trim();
    if session.is_empty() || session.len() > 1024 || session.chars().any(char::is_control) {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"antigravity-session-affinity-v1\0");
    // 长度前缀隔离各字段，不把正文或不同租户的同名会话混为一体
    for part in [client.as_str(), model.as_str(), payload.protocol(), session] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    ProviderSessionAffinityKey::try_new(hex::encode(hash.finalize())).ok()
}

fn header(context: &Map<String, Value>, name: &str) -> Option<String> {
    context
        .get("headers")
        .and_then(|headers| headers.get(name))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            context
                .get("opaque_request_headers")?
                .as_array()?
                .iter()
                .find_map(|entry| {
                    let pair = entry.as_array()?;
                    if !pair.first()?.as_str()?.eq_ignore_ascii_case(name) {
                        return None;
                    }
                    String::from_utf8(STANDARD.decode(pair.get(1)?.as_str()?).ok()?).ok()
                })
        })
}
