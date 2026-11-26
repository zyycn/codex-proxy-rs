//! Grok 原生 Responses 请求的模型绑定与租户隔离

use super::{GrokSessionAffinityKey, XAI_PROVIDER_NAME};
use gateway_core::{operation::GenerateRequest, policy::ClientApiKeyId};
use gateway_protocol::openai::OPENAI_PROTOCOL;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use std::fmt;

mod chat;
mod identity;
use identity::*;

/// 保留原生业务正文，仅绑定已选模型、传输和账号隔离身份
pub struct GrokResponsesRequest {
    body: Map<String, Value>,
    session_id: Option<String>,
    affinity: Option<GrokSessionAffinityKey>,
}

impl GrokResponsesRequest {
    #[must_use]
    pub const fn body(&self) -> &Map<String, Value> {
        &self.body
    }
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    pub(crate) fn upstream_model(&self) -> Option<&str> {
        self.body.get("model").and_then(Value::as_str)
    }
    #[must_use]
    pub const fn affinity(&self) -> Option<&GrokSessionAffinityKey> {
        self.affinity.as_ref()
    }
    pub(crate) fn input_items(&self) -> Vec<Value> {
        match self.body.get("input") {
            Some(Value::Array(items)) => items.clone(),
            Some(Value::String(input)) => {
                vec![json!({"type":"message","role":"user","content":input})]
            }
            _ => Vec::new(),
        }
    }
    pub(crate) fn set_replay_input(&mut self, input: Vec<Value>) {
        self.body.insert("input".into(), input.into());
    }
    pub(crate) fn instructions(&self) -> Option<&Value> {
        self.body.get("instructions").filter(|v| !v.is_null())
    }
    pub(crate) fn clear_instructions(&mut self) {
        self.body.remove("instructions");
    }
    pub(crate) fn set_previous_response_id(&mut self, response_id: Option<String>) {
        if let Some(id) = response_id {
            self.body.insert("previous_response_id".into(), id.into());
        } else {
            self.body.remove("previous_response_id");
        }
    }
    pub(crate) fn inherit_session(&mut self, session_id: Option<&str>) {
        self.session_id = session_id
            .filter(|v| !v.trim().is_empty())
            .map(str::to_owned);
        self.affinity = None;
        if let Some(id) = &self.session_id {
            self.body
                .insert("prompt_cache_key".into(), id.clone().into());
        } else {
            self.body.remove("prompt_cache_key");
        }
    }
    pub fn encode(
        request: &GenerateRequest,
        upstream_model: &str,
        client: &ClientApiKeyId,
    ) -> Result<Self, GrokRequestEncodeError> {
        if request.protocol_payload().protocol() != OPENAI_PROTOCOL {
            return Err(GrokRequestEncodeError::InvalidProtocolPayload);
        }
        let mut body = request.protocol_payload().body().clone();
        // HTTP 生成链路没有非生成预热语义，必须在计费请求发送前拒绝
        if body.get("generate") == Some(&Value::Bool(false)) {
            return Err(GrokRequestEncodeError::UnsupportedPrewarm);
        }
        let seed = explicit_session_seed(request, &body);
        let identity =
            resolve_session_identity(client.as_str(), upstream_model, seed.as_deref(), &body);
        sanitize_account_identity(&mut body);
        let (session_id, affinity) =
            identity.map_or((None, None), |(id, affinity)| (Some(id), Some(affinity)));
        if let Some(id) = &session_id {
            body.insert("prompt_cache_key".into(), id.clone().into());
        } else {
            body.remove("prompt_cache_key");
        }
        body.insert("model".into(), upstream_model.into());
        body.insert("stream".into(), true.into());
        Ok(Self {
            body,
            session_id,
            affinity,
        })
    }
    pub(crate) fn to_json_bytes(&self) -> Result<Vec<u8>, GrokRequestEncodeError> {
        serde_json::to_vec(&self.body).map_err(|_| GrokRequestEncodeError::Serialization)
    }
}

impl fmt::Debug for GrokResponsesRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrokResponsesRequest")
            .field("body_keys", &self.body.keys().collect::<Vec<_>>())
            .field("has_session", &self.session_id.is_some())
            .field("body", &"<prompt and tool payload redacted>")
            .finish()
    }
}

/// 编码错误仅包含字段名，不包含请求或账号数据
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum GrokRequestEncodeError {
    #[error("The request protocol is not supported by this Grok path.")]
    InvalidProtocolPayload,
    #[error("Grok HTTP generation does not support prewarm requests (generate=false).")]
    UnsupportedPrewarm,
    #[error("Grok request serialization failed.")]
    Serialization,
    #[error(transparent)]
    Chat(#[from] gateway_protocol::chat_completions::ChatError),
}
