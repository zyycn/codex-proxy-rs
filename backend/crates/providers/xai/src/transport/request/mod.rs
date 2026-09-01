//! OpenAI Responses 请求到官方 Grok Build wire 的转换边界。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use gateway_core::operation::GenerateRequest;
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use super::{GrokSessionAffinityKey, XAI_PROVIDER_NAME};

mod history;
mod response;
mod tools;

pub(super) use history::strip_invalid_encrypted_reasoning_from_body;
pub(crate) use response::GrokResponseTransform;

use history::*;
use tools::*;

const IDENTITY_FIELDS: &[&str] = &[
    "Authorization",
    "authorization",
    "Cookie",
    "cookie",
    "accessToken",
    "access_token",
    "accountId",
    "account_id",
    "cookies",
    "email",
    "idToken",
    "id_token",
    "refreshToken",
    "refresh_token",
    "sessionToken",
    "session_token",
    "teamId",
    "team_id",
    "token",
    "userId",
    "user_id",
    "x-email",
    "x-grok-user-id",
    "x-userid",
];
const ACCOUNT_BOUND_FIELDS: &[&str] = &[
    "agentId",
    "agent_id",
    "conversation",
    "conversationId",
    "conversation_id",
    "previousResponseId",
    "previous_response_id",
    "responseId",
    "response_id",
    "sessionId",
    "session_id",
    "x-grok-agent-id",
    "x-grok-conv-id",
    "x-grok-session-id",
];
const SESSION_FIELDS: &[&str] = &[
    "prompt_cache_key",
    "session_id",
    "sessionId",
    "conversation_id",
    "conversationId",
];
const PROTOCOL_CONTEXT_SESSION_FIELDS: &[&str] = &["conversation_id", "session_id", "thread_id"];
const MAX_SESSION_SEED_BYTES: usize = 1_024;
const GROK_CACHE_ROUTE_TOOLS: &[&str] = &["web_search", "x_search"];
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

fn sanitize_account_identity(body: &mut Map<String, Value>) {
    for field in IDENTITY_FIELDS.iter().chain(ACCOUNT_BOUND_FIELDS) {
        body.remove(*field);
    }
    let Some(Value::Object(metadata)) = body.get_mut("metadata") else {
        return;
    };
    for field in IDENTITY_FIELDS.iter().chain(ACCOUNT_BOUND_FIELDS) {
        metadata.remove(*field);
    }
}

fn sanitize_client_metadata(body: &mut Map<String, Value>) {
    // Codex 的 client_metadata 是本地 transport envelope，可能包含工作目录、仓库地址、
    // installation/session 标识。会话亲和信息已在调用本函数前提取，整个 envelope 都不能
    // 越过 Grok Build 边界。
    body.remove("client_metadata");
}

fn enable_grok_prompt_cache_route(
    body: &mut Map<String, Value>,
    upstream_model: &str,
    response: &mut GrokResponseTransform,
) {
    let tools = body
        .entry("tools".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Value::Array(tools) = tools else {
        return;
    };
    response.observe_upstream_cache_tools(tools);
    if is_cache_media_model(upstream_model) || has_tool_type(tools, "image_generation") {
        return;
    }
    if tools.is_empty() {
        for tool in GROK_CACHE_ROUTE_TOOLS {
            tools.push(json_object([("type", Value::String((*tool).to_owned()))]));
            response.mark_injected_cache_tool(tool);
        }
        // 无客户端工具时以 none 选中缓存路由，同时不授予搜索能力。
        body.insert("tool_choice".to_owned(), Value::String("none".to_owned()));
    } else if !has_tool_type(tools, "x_search") {
        tools.push(json_object([(
            "type",
            Value::String("x_search".to_owned()),
        )]));
        response.mark_injected_cache_tool("x_search");
    }
}

fn is_cache_media_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    ["image", "imagine", "video"]
        .into_iter()
        .any(|marker| model.contains(marker))
}

fn explicit_session_seed(request: &GenerateRequest, body: &Map<String, Value>) -> Option<String> {
    request
        .prompt_cache_key()
        .and_then(valid_session_seed)
        .map(ToOwned::to_owned)
        .or_else(|| first_session_value(body))
        .or_else(|| {
            body.get("metadata")
                .and_then(Value::as_object)
                .and_then(first_session_value)
        })
        .or_else(|| {
            PROTOCOL_CONTEXT_SESSION_FIELDS.iter().find_map(|field| {
                request
                    .protocol_payload()
                    .context()
                    .get(*field)
                    .and_then(Value::as_str)
                    .and_then(valid_session_seed)
                    .map(ToOwned::to_owned)
            })
        })
}

fn first_session_value(body: &Map<String, Value>) -> Option<String> {
    let prompt_cache_key = body
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .and_then(valid_session_seed)
        .map(ToOwned::to_owned);
    prompt_cache_key
        .or_else(|| {
            let metadata = body.get("metadata")?.as_object()?;
            ["session_id", "sessionId"]
                .into_iter()
                .find_map(|field| metadata.get(field).and_then(Value::as_str))
                .and_then(valid_session_seed)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    metadata
                        .get("user_id")
                        .and_then(Value::as_str)
                        .and_then(session_seed_from_user_id)
                })
        })
        .or_else(|| {
            SESSION_FIELDS[1..]
                .iter()
                .find_map(|field| body.get(*field).and_then(Value::as_str))
                .and_then(valid_session_seed)
                .map(ToOwned::to_owned)
        })
}

fn valid_session_seed(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= MAX_SESSION_SEED_BYTES
        && !value.chars().any(char::is_control))
    .then_some(value)
}

fn session_seed_from_user_id(value: &str) -> Option<String> {
    let value = value.trim();
    if let Ok(Value::Object(embedded)) = serde_json::from_str::<Value>(value)
        && let Some(seed) = ["session_id", "sessionId"]
            .into_iter()
            .find_map(|field| embedded.get(field).and_then(Value::as_str))
            .and_then(valid_session_seed)
    {
        return Some(seed.to_owned());
    }
    value
        .rfind("_session_")
        .and_then(|index| valid_session_seed(&value[index + "_session_".len()..]))
        .map(ToOwned::to_owned)
}

fn resolve_session_identity(
    client_api_key_ref: &str,
    upstream_model: &str,
    explicit_seed: Option<&str>,
    body: &Map<String, Value>,
) -> Option<(String, GrokSessionAffinityKey)> {
    let model = upstream_model.trim().to_ascii_lowercase();
    if client_api_key_ref.is_empty() || model.is_empty() {
        return None;
    }
    if let Some(seed) = explicit_seed {
        let upstream_source =
            format!("xai:build-session:v2:{client_api_key_ref}:{XAI_PROVIDER_NAME}:{seed}");
        let affinity_source = format!(
            "xai:build-affinity:v2:{client_api_key_ref}:{XAI_PROVIDER_NAME}:{model}:{seed}"
        );
        return Some((
            digest_uuid(&upstream_source),
            GrokSessionAffinityKey::from_digest(Sha256::digest(affinity_source).into()),
        ));
    }
    let (system, first_user) = message_anchors(body);
    let first_user = truncate_anchor(&first_user, 200);
    if first_user.is_empty() {
        return None;
    }
    let system = truncate_anchor(&system, 100);
    let upstream_source = format!(
        "xai:build-soft-session:v2:{client_api_key_ref}:{XAI_PROVIDER_NAME}:{system}:{first_user}"
    );
    let affinity_source = format!(
        "xai:build-soft-affinity:v2:{client_api_key_ref}:{XAI_PROVIDER_NAME}:{model}:{system}:{first_user}"
    );
    Some((
        digest_uuid(&upstream_source),
        GrokSessionAffinityKey::from_digest(Sha256::digest(affinity_source).into()),
    ))
}

fn digest_uuid(source: &str) -> String {
    let digest = Sha256::digest(source);
    let value = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &value[0..8],
        &value[8..12],
        &value[12..16],
        &value[16..20],
        &value[20..32]
    )
}

fn truncate_anchor(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

fn message_anchors(body: &Map<String, Value>) -> (String, String) {
    let mut system = body
        .get("instructions")
        .map(flatten_message_content)
        .filter(|value| !value.is_empty())
        .or_else(|| body.get("system").map(flatten_message_content))
        .unwrap_or_default();
    let mut first_user = String::new();
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        collect_role_anchors(messages, &mut system, &mut first_user);
    }
    if first_user.is_empty() {
        match body.get("input") {
            Some(Value::String(value)) => first_user = value.trim().to_owned(),
            Some(Value::Array(items)) => {
                collect_role_anchors(items, &mut system, &mut first_user);
            }
            _ => {}
        }
    }
    (system, first_user)
}

fn collect_role_anchors(items: &[Value], system: &mut String, first_user: &mut String) {
    for item in items {
        let Some(item) = item.as_object() else {
            continue;
        };
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if !item_type.is_empty() && item_type != "message" {
            continue;
        }
        let role = item
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let content = item
            .get("content")
            .map(flatten_message_content)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                item.get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_default();
        if content.is_empty() {
            continue;
        }
        match role.as_str() {
            "system" | "developer" if system.is_empty() => *system = content,
            "user" if first_user.is_empty() => *first_user = content,
            "" if first_user.is_empty() => *first_user = content,
            _ => {}
        }
        if !first_user.is_empty() && !system.is_empty() {
            break;
        }
    }
}

fn flatten_message_content(value: &Value) -> String {
    match value {
        Value::String(value) => value.trim().to_owned(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(Value::as_object)
            .filter(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str).unwrap_or_default(),
                    "" | "text" | "input_text" | "output_text"
                )
            })
            .filter_map(|part| part.get("text").and_then(Value::as_str).map(str::trim))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn normalize_responses_request(
    body: &mut Map<String, Value>,
) -> Result<GrokResponseTransform, GrokRequestEncodeError> {
    if let Some(response_format) = body.remove("response_format") {
        let text = body
            .entry("text".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if text.is_null() {
            *text = Value::Object(Map::new());
        }
        let text = text
            .as_object_mut()
            .ok_or(GrokRequestEncodeError::InvalidRequestField { field: "text" })?;
        if text.get("format").is_none_or(Value::is_null) {
            text.insert(
                "format".to_owned(),
                normalize_response_format(response_format)
                    .map_err(|error| error.at_field("response_format"))?,
            );
        }
    }
    patch_reasoning_text_types(body);
    ToolNormalizer::new().normalize(body)
}

fn normalize_build_request(
    body: &mut Map<String, Value>,
    upstream_model: &str,
) -> Result<(), GrokRequestEncodeError> {
    apply_build_response_defaults(body)?;
    normalize_build_reasoning_effort(body, upstream_model);
    sanitize_build_model_fields(body, upstream_model);
    Ok(())
}

fn apply_build_response_defaults(
    body: &mut Map<String, Value>,
) -> Result<(), GrokRequestEncodeError> {
    if body.get("store").is_none_or(Value::is_null) {
        body.insert("store".to_owned(), Value::Bool(false));
    }

    let include = body
        .entry("include".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if include.is_null() {
        *include = Value::Array(Vec::new());
    }
    let include = include
        .as_array_mut()
        .ok_or(GrokRequestEncodeError::InvalidRequestField { field: "include" })?;
    if include.iter().any(|value| !value.is_string()) {
        return Err(GrokRequestEncodeError::InvalidRequestField { field: "include" });
    }
    if !include
        .iter()
        .any(|value| value.as_str() == Some("reasoning.encrypted_content"))
    {
        include.push(Value::String("reasoning.encrypted_content".to_owned()));
    }
    Ok(())
}

fn normalize_build_reasoning_effort(body: &mut Map<String, Value>, upstream_model: &str) {
    let supports_effort = grok_model_supports_reasoning_effort(upstream_model);
    if let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut) {
        if let Some(effort) = reasoning.remove("effort")
            && supports_effort
            && let Some(effort) = normalize_grok_reasoning_effort_value(&effort)
        {
            reasoning.insert("effort".to_owned(), Value::String(effort.to_owned()));
        }
        if reasoning.is_empty() {
            body.remove("reasoning");
        }
    }

    if let Some(effort) = body.remove("reasoning_effort")
        && supports_effort
        && let Some(effort) = normalize_grok_reasoning_effort_value(&effort)
    {
        body.insert(
            "reasoning_effort".to_owned(),
            Value::String(effort.to_owned()),
        );
    }

    let camel_effort = body.remove("reasoningEffort");
    if !body.contains_key("reasoning_effort")
        && supports_effort
        && let Some(effort) = camel_effort
            .as_ref()
            .and_then(normalize_grok_reasoning_effort_value)
    {
        body.insert(
            "reasoning_effort".to_owned(),
            Value::String(effort.to_owned()),
        );
    }

    if is_grok_composer_model(upstream_model) {
        body.remove("reasoning");
        body.remove("reasoning_effort");
        body.remove("reasoningEffort");
    }
}

fn normalize_grok_reasoning_effort_value(value: &Value) -> Option<&'static str> {
    let normalized = value
        .as_str()?
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !matches!(character, '-' | '_' | ' '))
        .collect::<String>();
    match normalized.as_str() {
        "none" => Some("none"),
        "minimal" | "low" => Some("low"),
        "medium" => Some("medium"),
        "high" | "xhigh" | "extrahigh" | "max" | "ultra" => Some("high"),
        _ => None,
    }
}

fn strip_grok_provider_prefix(model: &str) -> &str {
    let model = model.trim();
    let lower = model.to_ascii_lowercase();
    ["xai/", "x-ai/", "grok/"]
        .into_iter()
        .find_map(|prefix| {
            lower
                .starts_with(prefix)
                .then(|| model[prefix.len()..].trim())
        })
        .unwrap_or(model)
}

fn grok_model_last_slug(model: &str) -> String {
    model
        .trim()
        .rsplit_once('/')
        .map_or_else(|| model.trim(), |(_, slug)| slug.trim())
        .to_ascii_lowercase()
}

fn is_grok_composer_model(model: &str) -> bool {
    matches!(
        grok_model_last_slug(model).as_str(),
        "grok-composer" | "grok-composer-2.5-fast" | "composer-2.5"
    )
}

fn grok_model_supports_reasoning_effort(model: &str) -> bool {
    matches!(
        strip_grok_provider_prefix(model)
            .to_ascii_lowercase()
            .as_str(),
        "grok-4.5"
            | "grok-4.5-latest"
            | "grok-4.6"
            | "grok-4.6-latest"
            | "grok-4.3"
            | "grok-4.3-latest"
            | "grok-3-mini"
            | "grok-3-mini-fast"
            | "grok-4.20-0309-reasoning"
            | "grok-4.20-reasoning"
            | "grok-4.20-multi-agent-0309"
    )
}

fn sanitize_build_model_fields(body: &mut Map<String, Value>, upstream_model: &str) {
    for field in ["prompt_cache_retention", "safety_identifier"] {
        body.remove(field);
    }
    if upstream_model.trim().eq_ignore_ascii_case("grok-4.5") {
        for field in [
            "presence_penalty",
            "presencePenalty",
            "frequency_penalty",
            "frequencyPenalty",
            "stop",
        ] {
            body.remove(field);
        }
    }
    if grok_model_last_slug(upstream_model).starts_with("grok-4.20") {
        body.remove("logprobs");
        body.remove("top_logprobs");
    }
    delete_json_field_recursive(body, "external_web_access");
}

fn delete_json_field_recursive(object: &mut Map<String, Value>, field: &str) {
    object.remove(field);
    for value in object.values_mut() {
        delete_json_field_from_value(value, field);
    }
}

fn delete_json_field_from_value(value: &mut Value, field: &str) {
    match value {
        Value::Object(child) => delete_json_field_recursive(child, field),
        Value::Array(children) => {
            for child in children {
                delete_json_field_from_value(child, field);
            }
        }
        _ => {}
    }
}

fn normalize_response_format(value: Value) -> Result<Value, GrokRequestEncodeError> {
    let Some(format) = value.as_object() else {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    };
    if format.get("type").and_then(Value::as_str) != Some("json_schema") {
        return Ok(value);
    }
    let Some(schema) = format.get("json_schema").and_then(Value::as_object) else {
        return Ok(value);
    };
    let mut normalized = Map::new();
    normalized.insert("type".to_owned(), Value::String("json_schema".to_owned()));
    normalized.extend(
        schema
            .iter()
            .filter(|(key, _)| key.as_str() != "type")
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    Ok(Value::Object(normalized))
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
