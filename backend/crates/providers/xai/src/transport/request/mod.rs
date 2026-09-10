//! OpenAI Responses 请求到官方 Grok Build wire 的转换边界。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use gateway_core::operation::GenerateRequest;
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use super::{GrokSessionAffinityKey, XAI_PROVIDER_NAME};

mod history;
mod identity;
mod model;
mod response;
mod schema;
mod tools;

pub(super) use history::strip_invalid_encrypted_reasoning_from_body;
pub(crate) use response::GrokResponseTransform;

use history::*;
use identity::*;
use model::*;
use schema::*;
use tools::*;

/// Grok CLI 在历史条目上注入、Grok Build 无法反序列化的内部键；只在已知
/// 注入位置剥离，避免误伤工具 schema 或输出中恰好同名的键。
const GROK_INTERNAL_HISTORY_KEYS: &[&str] =
    &["phase", "internal_chat_message_metadata_passthrough"];

/// 保留客户端 OpenAI Responses object 的 xAI 上游请求。
pub struct GrokResponsesRequest {
    body: Map<String, Value>,
    session_id: Option<String>,
    reasoning_replay_session_id: Option<String>,
    affinity: Option<GrokSessionAffinityKey>,
    response_transform: GrokResponseTransform,
}

impl GrokResponsesRequest {
    /// 返回发送到 `/v1/responses` 的 JSON object。
    #[must_use]
    pub const fn body(&self) -> &Map<String, Value> {
        &self.body
    }

    /// 返回按下游租户隔离后的稳定 Grok 会话 UUID。
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// 返回只有显式会话身份才启用的 reasoning replay scope。
    pub(crate) fn reasoning_replay_session_id(&self) -> Option<&str> {
        self.reasoning_replay_session_id.as_deref()
    }

    /// 返回归一化后的 xAI wire 模型。
    pub(crate) fn upstream_model(&self) -> Option<&str> {
        self.body.get("model").and_then(Value::as_str)
    }

    pub(crate) fn has_previous_response_id(&self) -> bool {
        self.body
            .get("previous_response_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
    }

    pub(crate) fn replay_input_items(&self) -> Option<Vec<Value>> {
        self.body.get("input").and_then(Value::as_array).cloned()
    }

    /// 返回与会话一致、额外绑定模型的账号亲和键。
    #[must_use]
    pub const fn affinity(&self) -> Option<&GrokSessionAffinityKey> {
        self.affinity.as_ref()
    }

    pub(crate) fn response_transform(&self) -> GrokResponseTransform {
        self.response_transform.clone()
    }

    pub(crate) fn input_items(&self) -> Vec<Value> {
        match self.body.get("input") {
            Some(Value::Array(items)) => items.clone(),
            Some(Value::String(input)) => vec![json_object([
                ("type", Value::String("message".to_owned())),
                ("role", Value::String("user".to_owned())),
                ("content", Value::String(input.clone())),
            ])],
            _ => Vec::new(),
        }
    }

    pub(crate) fn set_replay_input(
        &mut self,
        input: Vec<Value>,
    ) -> Result<(), GrokRequestEncodeError> {
        let mut normalizer = ToolNormalizer::for_replay(self.response_transform.clone());
        let mut normalized = Vec::with_capacity(input.len());
        for item in input {
            match item {
                Value::Object(item) if string_field(&item, "type") == "custom_tool_call" => {
                    normalized.push(Value::Object(
                        normalizer.normalize_custom_tool_call_input(&item)?,
                    ));
                }
                item => normalized.push(item),
            }
        }
        self.response_transform = normalizer.response;
        self.body
            .insert("input".to_owned(), Value::Array(normalized));
        Ok(())
    }

    /// 为同账号的一次 xAI `invalid_encrypted_content` 恢复请求移除被拒绝的密文。
    ///
    /// 只在至少一个 reasoning item 含非空密文时改写；可读 summary/content、ID 与
    /// 其他历史保持不变，剥离后只剩 `type` 的空壳一并删除。
    pub(crate) fn strip_invalid_encrypted_reasoning(&mut self) -> bool {
        strip_invalid_encrypted_reasoning_from_body(&mut self.body)
    }

    pub(crate) fn instructions(&self) -> Option<&Value> {
        self.body
            .get("instructions")
            .filter(|value| !value.is_null())
    }

    pub(crate) fn clear_instructions(&mut self) {
        self.body.remove("instructions");
    }

    pub(crate) fn set_previous_response_id(&mut self, response_id: Option<String>) {
        match response_id {
            Some(response_id) => {
                self.body.insert(
                    "previous_response_id".to_owned(),
                    Value::String(response_id),
                );
            }
            None => {
                self.body.remove("previous_response_id");
            }
        }
    }

    pub(crate) fn inherit_session(&mut self, session_id: Option<&str>) {
        let Some(session_id) = session_id.map(str::trim).filter(|value| !value.is_empty()) else {
            self.clear_session();
            return;
        };
        self.session_id = Some(session_id.to_owned());
        if self.reasoning_replay_session_id.is_some() {
            self.reasoning_replay_session_id = Some(session_id.to_owned());
        }
        self.affinity = None;
        self.body.insert(
            "prompt_cache_key".to_owned(),
            Value::String(session_id.to_owned()),
        );
    }

    pub(crate) fn clear_session(&mut self) {
        self.body.remove("prompt_cache_key");
        self.session_id = None;
        self.reasoning_replay_session_id = None;
        self.affinity = None;
    }

    pub fn encode(
        request: &GenerateRequest,
        upstream_model: &str,
        client_api_key_ref: &ClientApiKeyId,
    ) -> Result<Self, GrokRequestEncodeError> {
        Self::encode_inner(request, upstream_model, client_api_key_ref, true, false)
    }

    pub(super) fn encode_compaction_source(
        request: &GenerateRequest,
        upstream_model: &str,
        client_api_key_ref: &ClientApiKeyId,
    ) -> Result<Self, GrokRequestEncodeError> {
        Self::encode_inner(request, upstream_model, client_api_key_ref, false, true)
    }

    fn encode_inner(
        request: &GenerateRequest,
        upstream_model: &str,
        client_api_key_ref: &ClientApiKeyId,
        allow_cache_route: bool,
        should_consume_terminal_compaction_trigger: bool,
    ) -> Result<Self, GrokRequestEncodeError> {
        let payload = request.protocol_payload();
        if payload.protocol() != "openai" {
            return Err(GrokRequestEncodeError::InvalidProtocolPayload);
        }
        let mut body = payload.body().clone();
        if should_consume_terminal_compaction_trigger {
            consume_terminal_compaction_trigger(&mut body)?;
        }
        let upstream_model = resolve_grok_text_responses_model_id(upstream_model);
        // 这些字段属于 Codex/OpenAI 侧请求控制，不是 xAI 上游协议字段。
        // OpenAI 透明路径会保留未知字段；这里只在 xAI adapter 内做最小剥离。
        body.remove("provider_options");
        body.remove("service_tier");
        let session_seed = explicit_session_seed(request, &body);
        let enable_cache_route = allow_cache_route && session_seed.is_some();
        let identity = resolve_session_identity(
            client_api_key_ref.as_str(),
            &upstream_model,
            session_seed.as_deref(),
            &body,
        );
        sanitize_account_identity(&mut body);
        sanitize_client_metadata(&mut body);
        normalize_build_request(&mut body, &upstream_model)?;
        let mut response_transform = normalize_responses_request(&mut body)?;
        response_transform.observe_client_cache_tools();
        if enable_cache_route {
            enable_grok_prompt_cache_route(&mut body, &upstream_model, &mut response_transform);
        }
        let (session_id, affinity) = identity.map_or((None, None), |(session_id, affinity)| {
            (Some(session_id), Some(affinity))
        });
        let reasoning_replay_session_id =
            session_seed.is_some().then(|| session_id.clone()).flatten();
        match session_id.as_ref() {
            Some(session_id) => {
                body.insert(
                    "prompt_cache_key".to_owned(),
                    Value::String(session_id.clone()),
                );
            }
            None => {
                body.remove("prompt_cache_key");
            }
        }
        body.insert("model".to_owned(), Value::String(upstream_model));
        body.insert("stream".to_owned(), Value::Bool(true));
        Ok(Self {
            body,
            session_id,
            reasoning_replay_session_id,
            affinity,
            response_transform,
        })
    }

    pub(crate) fn to_json_bytes(&self) -> Result<Vec<u8>, GrokRequestEncodeError> {
        serde_json::to_vec(&self.body).map_err(|_| GrokRequestEncodeError::Serialization)
    }
}

fn resolve_grok_text_responses_model_id(model: &str) -> String {
    let stripped = strip_grok_provider_prefix(model);
    let normalized = stripped.to_ascii_lowercase();
    match normalized.as_str() {
        "grok" | "grok-latest" | "grok-4.5" | "grok-4.5-latest" | "grok-build-latest" => {
            "grok-4.5".to_owned()
        }
        "grok-4.6" | "grok-4.6-latest" => "grok-4.6".to_owned(),
        "grok-4.3" | "grok-4.3-latest" => "grok-4.3".to_owned(),
        "grok-3-mini"
        | "grok-3-mini-fast"
        | "grok-build-0.1"
        | "grok-composer-2.5-fast"
        | "grok-4.20-0309-reasoning"
        | "grok-4.20-0309-non-reasoning"
        | "grok-4.20-multi-agent-0309" => normalized,
        "grok-build" => "grok-build-0.1".to_owned(),
        "grok-composer" | "composer-2.5" => "grok-composer-2.5-fast".to_owned(),
        "grok-4.20-reasoning" => "grok-4.20-0309-reasoning".to_owned(),
        "grok-4.20-non-reasoning" => "grok-4.20-0309-non-reasoning".to_owned(),
        "grok-4.20-multi-agent" | "grok-4.20-multi-agent-latest" => {
            "grok-4.20-multi-agent-0309".to_owned()
        }
        _ => stripped.to_owned(),
    }
}

fn consume_terminal_compaction_trigger(
    body: &mut Map<String, Value>,
) -> Result<(), GrokRequestEncodeError> {
    let Some(Value::Array(input)) = body.get_mut("input") else {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    };
    let Some(Value::Object(last)) = input.last() else {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    };
    if last.get("type").and_then(Value::as_str) != Some("compaction_trigger") {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    }
    input.pop();
    Ok(())
}

impl fmt::Debug for GrokResponsesRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokResponsesRequest")
            .field("body_keys", &self.body.keys().collect::<Vec<_>>())
            .field("has_session", &self.session_id.is_some())
            .field(
                "has_response_transform",
                &!self.response_transform.is_empty(),
            )
            .field("body", &"<prompt and tool payload redacted>")
            .finish()
    }
}

/// Generate 到 Responses 的编码错误，不保留 option 与 prompt 值。
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokRequestEncodeError {
    /// 数据面只接受 OpenAI adapter 保留的原始 Responses object。
    #[error("Grok Build request is missing its OpenAI protocol payload")]
    InvalidProtocolPayload,
    /// JSON 序列化意外失败。
    #[error("Grok Build request serialization failed")]
    Serialization,
    /// Responses 兼容字段无法安全归一化。
    #[error("Grok Build request normalization failed")]
    InvalidRequestNormalization,
    /// 请求中的具体字段无法安全转换为 Grok Build 接受的形态。
    #[error("Grok Build request field `{field}` could not be normalized safely")]
    InvalidRequestField { field: &'static str },
}

impl GrokRequestEncodeError {
    fn at_field(self, field: &'static str) -> Self {
        match self {
            Self::InvalidRequestNormalization => Self::InvalidRequestField { field },
            error => error,
        }
    }
}

fn normalize_responses_request(
    body: &mut Map<String, Value>,
) -> Result<GrokResponseTransform, GrokRequestEncodeError> {
    for field in ["response_format", "reasoning_effort", "reasoningEffort"] {
        if body.contains_key(field) {
            return Err(GrokRequestEncodeError::InvalidRequestField { field });
        }
    }
    patch_reasoning_text_types(body);
    ToolNormalizer::new().normalize(body)
}

fn patch_reasoning_text_types(body: &mut Map<String, Value>) {
    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in content {
            let Some(part) = part.as_object_mut() else {
                continue;
            };
            part.entry("type".to_owned())
                .or_insert_with(|| Value::String("reasoning_text".to_owned()));
        }
    }
}

const MAX_BUILD_TOOL_ALIAS_LENGTH: usize = 128;
const MAX_TOOL_SEARCH_DESCRIPTION_BYTES: usize = 16 << 10;
const MAX_BUFFERED_FUNCTION_ARGUMENTS_BYTES: usize = 1 << 20;
const MAX_TOTAL_BUFFERED_FUNCTION_ARGUMENTS_BYTES: usize = 4 << 20;
const MAX_JSON_NUMBER_TEXT_BYTES: usize = 256;
const MAX_EXACT_JSON_INTEGER_TEXT: &str = "9007199254740991";
const MAX_EXACT_JSON_INTEGER: u64 = 9_007_199_254_740_991;
