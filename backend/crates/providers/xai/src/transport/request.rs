use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use gateway_core::operation::GenerateRequest;
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use super::{GrokSessionAffinityKey, XAI_PROVIDER_NAME};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ToolKind {
    Function,
    Custom,
    ToolSearch,
    ApplyPatch,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ToolIdentity {
    kind: ToolKind,
    namespace: String,
    name: String,
}

impl ToolIdentity {
    fn new(kind: ToolKind, namespace: &str, name: &str) -> Self {
        Self {
            kind,
            namespace: namespace.to_owned(),
            name: name.to_owned(),
        }
    }

    fn is_root_custom_apply_patch(&self) -> bool {
        self.kind == ToolKind::Custom && self.namespace.is_empty() && self.name == "apply_patch"
    }

    fn custom_argument_field(&self) -> &'static str {
        if self.is_root_custom_apply_patch() {
            "patch"
        } else {
            "input"
        }
    }
}

#[derive(Debug, Clone)]
struct StreamCallState {
    identity: ToolIdentity,
    schema: Option<Value>,
    arguments: String,
    passthrough: bool,
    last_delta: Option<Map<String, Value>>,
    added_payload: Option<Map<String, Value>>,
}

#[derive(Clone, Default)]
pub(crate) struct GrokResponseTransform {
    aliases: BTreeMap<String, ToolIdentity>,
    function_schemas: BTreeMap<String, Value>,
    visible_tools: Vec<Value>,
    legacy_local_shell: bool,
    filter_x_search: bool,
    injected_tool_types: BTreeSet<String>,
    client_declared_tools: BTreeSet<String>,
    dropped_output_indexes: BTreeSet<u64>,
    dropped_item_ids: BTreeSet<String>,
    stream_calls: BTreeMap<String, StreamCallState>,
    stream_keys: BTreeMap<String, String>,
    stream_argument_bytes: usize,
    stream_sequence_next: Option<u64>,
}

pub(crate) struct GrokTransformedWireEvent {
    event_type: String,
    value: Value,
}

impl GrokTransformedWireEvent {
    #[must_use]
    pub(crate) fn event_type(&self) -> &str {
        &self.event_type
    }

    #[must_use]
    pub(crate) fn into_value(self) -> Value {
        self.value
    }
}

impl GrokResponseTransform {
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.aliases.is_empty()
            && self.function_schemas.is_empty()
            && self.visible_tools.is_empty()
            && !self.legacy_local_shell
            && !self.filter_x_search
            && self.injected_tool_types.is_empty()
    }

    fn observe_client_cache_tools(&mut self) {
        for tool in &self.visible_tools {
            let kind = tool.get("type").and_then(Value::as_str).unwrap_or_default();
            if kind == "x_search" {
                self.filter_x_search = true;
            }
            if matches!(kind, "function" | "custom")
                && let Some(name) = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
            {
                self.client_declared_tools.insert(name.to_owned());
            }
        }
    }

    fn observe_upstream_cache_tools(&mut self, tools: &[Value]) {
        if has_tool_type(tools, "x_search") {
            self.filter_x_search = true;
        }
    }

    fn mark_injected_cache_tool(&mut self, kind: &str) {
        self.injected_tool_types.insert(kind.to_owned());
        if kind == "x_search" {
            self.filter_x_search = true;
        }
    }

    pub(crate) fn resequence_stream_value(&mut self, value: &mut Value) {
        let Some(payload) = value.as_object_mut() else {
            return;
        };
        let Some(raw_sequence) = payload.get("sequence_number") else {
            return;
        };
        if self.stream_sequence_next.is_none() {
            let Some(sequence) = exact_nonnegative_sequence(raw_sequence) else {
                return;
            };
            self.stream_sequence_next = Some(sequence);
        }
        let Some(next) = self.stream_sequence_next.as_mut() else {
            return;
        };
        payload.insert(
            "sequence_number".to_owned(),
            Value::Number(serde_json::Number::from(*next)),
        );
        *next = next.saturating_add(1);
    }

    pub(crate) fn rewrite_stream_event(
        &mut self,
        event_type: &str,
        mut value: Value,
    ) -> Result<Vec<GrokTransformedWireEvent>, GrokRequestEncodeError> {
        if self.cache_filter_enabled() {
            if value
                .get("item")
                .is_some_and(|item| self.is_internal_cache_call(item))
            {
                self.record_dropped_cache_item(&value);
                return Ok(Vec::new());
            }
            self.filter_cache_envelope(&mut value)?;
            if self.references_dropped_cache_item(&value) {
                return Ok(Vec::new());
            }
            self.compact_cache_output_index(&mut value);
        }
        if event_type == "response.output_item.added"
            && let Some(item) = value.get("item").and_then(Value::as_object)
            && let Some(primary) = self.remember_stream_call(item)
            && self
                .stream_calls
                .get(&primary)
                .is_some_and(|state| state.identity.kind == ToolKind::ApplyPatch)
        {
            if let Some(state) = self.stream_calls.get_mut(&primary) {
                state.added_payload = value.as_object().cloned();
            }
            return Ok(Vec::new());
        }

        if event_type == "response.function_call_arguments.delta"
            && let Some(primary) = self.stream_call_key(&value)
        {
            let delta = value
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let state = self
                .stream_calls
                .get_mut(&primary)
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            match state.identity.kind {
                ToolKind::Custom => {
                    state.arguments.push_str(delta);
                    state.last_delta = value.as_object().cloned().map(|mut payload| {
                        payload.remove("delta");
                        payload
                    });
                    return Ok(Vec::new());
                }
                ToolKind::ToolSearch | ToolKind::ApplyPatch => return Ok(Vec::new()),
                ToolKind::Function if state.schema.is_some() => {
                    let call_limit_exceeded = state.arguments.len().saturating_add(delta.len())
                        > MAX_BUFFERED_FUNCTION_ARGUMENTS_BYTES;
                    let total_limit_exceeded =
                        self.stream_argument_bytes.saturating_add(delta.len())
                            > MAX_TOTAL_BUFFERED_FUNCTION_ARGUMENTS_BYTES;
                    if !state.passthrough && (call_limit_exceeded || total_limit_exceeded) {
                        let buffered = std::mem::take(&mut state.arguments);
                        self.stream_argument_bytes =
                            self.stream_argument_bytes.saturating_sub(buffered.len());
                        state.passthrough = true;
                        state.last_delta = None;
                        let mut output = Vec::with_capacity(2);
                        if !buffered.is_empty() {
                            let mut flushed = value.clone();
                            flushed
                                .as_object_mut()
                                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?
                                .insert("delta".to_owned(), Value::String(buffered));
                            output.push(GrokTransformedWireEvent {
                                event_type: event_type.to_owned(),
                                value: flushed,
                            });
                        }
                        output.push(GrokTransformedWireEvent {
                            event_type: event_type.to_owned(),
                            value,
                        });
                        return Ok(output);
                    }
                    if !state.passthrough {
                        state.arguments.push_str(delta);
                        self.stream_argument_bytes =
                            self.stream_argument_bytes.saturating_add(delta.len());
                        state.last_delta = value.as_object().cloned().map(|mut payload| {
                            payload.remove("delta");
                            payload
                        });
                        return Ok(Vec::new());
                    }
                }
                ToolKind::Function => {}
            }
        }

        if event_type == "response.function_call_arguments.done"
            && let Some(primary) = self.stream_call_key(&value)
        {
            let state = self
                .stream_calls
                .get(&primary)
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            match state.identity.kind {
                ToolKind::ToolSearch | ToolKind::ApplyPatch => return Ok(Vec::new()),
                ToolKind::Custom => {
                    let arguments = value
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|arguments| !arguments.is_empty())
                        .unwrap_or(&state.arguments);
                    let input = decode_custom_tool_input(&state.identity, arguments);
                    let mut output = Vec::with_capacity(2);
                    if let Some(delta) = state.last_delta.as_ref() {
                        output.push(GrokTransformedWireEvent {
                            event_type: "response.custom_tool_call_input.delta".to_owned(),
                            value: custom_tool_stream_payload(
                                delta,
                                "response.custom_tool_call_input.delta",
                                "delta",
                                &input,
                            ),
                        });
                    }
                    output.push(GrokTransformedWireEvent {
                        event_type: "response.custom_tool_call_input.done".to_owned(),
                        value: custom_tool_stream_payload(
                            value
                                .as_object()
                                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?,
                            "response.custom_tool_call_input.done",
                            "input",
                            &input,
                        ),
                    });
                    if let Some(state) = self.stream_calls.get_mut(&primary) {
                        state.arguments.clear();
                        state.last_delta = None;
                    }
                    return Ok(output);
                }
                ToolKind::Function if state.schema.is_some() => {
                    let schema = state
                        .schema
                        .clone()
                        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                    let passthrough = state.passthrough;
                    let buffered = state.arguments.clone();
                    let last_delta = state.last_delta.clone();
                    let arguments = value
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|arguments| !arguments.is_empty())
                        .unwrap_or(&buffered);
                    let normalized = normalize_function_arguments(arguments, &schema)
                        .unwrap_or_else(|| arguments.to_owned());
                    if passthrough {
                        value
                            .as_object_mut()
                            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?
                            .insert("arguments".to_owned(), Value::String(normalized));
                        return Ok(vec![GrokTransformedWireEvent {
                            event_type: event_type.to_owned(),
                            value,
                        }]);
                    }

                    let mut output = Vec::with_capacity(2);
                    if let Some(mut delta) = last_delta {
                        delta.insert("delta".to_owned(), Value::String(normalized.clone()));
                        output.push(GrokTransformedWireEvent {
                            event_type: "response.function_call_arguments.delta".to_owned(),
                            value: Value::Object(delta),
                        });
                    }
                    value
                        .as_object_mut()
                        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?
                        .insert("arguments".to_owned(), Value::String(normalized));
                    output.push(GrokTransformedWireEvent {
                        event_type: event_type.to_owned(),
                        value,
                    });
                    if let Some(state) = self.stream_calls.get_mut(&primary) {
                        self.stream_argument_bytes = self
                            .stream_argument_bytes
                            .saturating_sub(state.arguments.len());
                        state.arguments.clear();
                        state.last_delta = None;
                    }
                    return Ok(output);
                }
                ToolKind::Function => {}
            }
        }

        if event_type == "response.output_item.done"
            && let Some(item) = value.get("item").and_then(Value::as_object)
            && self
                .aliases
                .get(string_field(item, "name"))
                .is_some_and(|identity| identity.kind == ToolKind::ApplyPatch)
        {
            return self.rewrite_apply_patch_done_event(value);
        }

        let completed_call = (event_type == "response.output_item.done")
            .then(|| value.get("item").and_then(Value::as_object))
            .flatten()
            .and_then(|item| self.primary_stream_call(item));
        self.rewrite_response_value(&mut value)?;
        if let Some(response) = value.get_mut("response").and_then(Value::as_object_mut) {
            self.restore_visible_tools(response);
        }
        if let Some(primary) = completed_call {
            self.take_stream_call(&primary);
        }
        Ok(vec![GrokTransformedWireEvent {
            event_type: event_type.to_owned(),
            value,
        }])
    }

    fn remember_stream_call(&mut self, item: &Map<String, Value>) -> Option<String> {
        if string_field(item, "type") != "function_call" {
            return None;
        }
        let identity = self.aliases.get(string_field(item, "name"))?.clone();
        let keys = ["id", "call_id"]
            .into_iter()
            .filter_map(|field| item.get(field).and_then(Value::as_str))
            .filter(|key| !key.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let primary = keys.first()?.clone();
        self.stream_calls.insert(
            primary.clone(),
            StreamCallState {
                identity,
                schema: self
                    .function_schemas
                    .get(string_field(item, "name"))
                    .cloned(),
                arguments: String::new(),
                passthrough: false,
                last_delta: None,
                added_payload: None,
            },
        );
        for key in keys {
            self.stream_keys.insert(key, primary.clone());
        }
        Some(primary)
    }

    fn stream_call_key(&mut self, payload: &Value) -> Option<String> {
        for field in ["item_id", "call_id"] {
            if let Some(key) = payload.get(field).and_then(Value::as_str)
                && let Some(primary) = self.stream_keys.get(key)
            {
                return Some(primary.clone());
            }
        }
        let alias = payload.get("name").and_then(Value::as_str)?;
        let identity = self.aliases.get(alias)?.clone();
        let primary = ["item_id", "call_id"]
            .into_iter()
            .find_map(|field| payload.get(field).and_then(Value::as_str))?
            .to_owned();
        self.stream_calls.insert(
            primary.clone(),
            StreamCallState {
                identity,
                schema: self.function_schemas.get(alias).cloned(),
                arguments: String::new(),
                passthrough: false,
                last_delta: None,
                added_payload: None,
            },
        );
        self.stream_keys.insert(primary.clone(), primary.clone());
        Some(primary)
    }

    fn primary_stream_call(&self, item: &Map<String, Value>) -> Option<String> {
        ["id", "call_id"]
            .into_iter()
            .filter_map(|field| item.get(field).and_then(Value::as_str))
            .find_map(|key| self.stream_keys.get(key).cloned())
    }

    fn take_stream_call(&mut self, primary: &str) -> Option<StreamCallState> {
        self.stream_keys.retain(|_, owner| owner != primary);
        let state = self.stream_calls.remove(primary)?;
        self.stream_argument_bytes = self
            .stream_argument_bytes
            .saturating_sub(state.arguments.len());
        Some(state)
    }

    fn rewrite_response_value(&self, value: &mut Value) -> Result<(), GrokRequestEncodeError> {
        match value {
            Value::Array(values) => {
                for value in values {
                    self.rewrite_response_value(value)?;
                }
            }
            Value::Object(object) => {
                for value in object.values_mut() {
                    self.rewrite_response_value(value)?;
                }
                match string_field(object, "type") {
                    "function_call" => self.rewrite_function_call(object)?,
                    "shell_call" if self.legacy_local_shell => {
                        rewrite_legacy_local_shell_call(object)
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn rewrite_function_call(
        &self,
        call: &mut Map<String, Value>,
    ) -> Result<(), GrokRequestEncodeError> {
        let Some(identity) = self.aliases.get(string_field(call, "name")) else {
            return Ok(());
        };
        match identity.kind {
            ToolKind::Function => {
                let alias = string_field(call, "name").to_owned();
                if let Some(schema) = self.function_schemas.get(&alias)
                    && let Some(arguments) = call.get("arguments").and_then(Value::as_str)
                    && let Some(normalized) = normalize_function_arguments(arguments, schema)
                {
                    call.insert("arguments".to_owned(), Value::String(normalized));
                }
                call.insert("name".to_owned(), Value::String(identity.name.clone()));
                if identity.namespace.is_empty() {
                    call.remove("namespace");
                } else {
                    call.insert(
                        "namespace".to_owned(),
                        Value::String(identity.namespace.clone()),
                    );
                }
            }
            ToolKind::Custom => {
                call.insert(
                    "type".to_owned(),
                    Value::String("custom_tool_call".to_owned()),
                );
                call.insert("name".to_owned(), Value::String(identity.name.clone()));
                if identity.namespace.is_empty() {
                    call.remove("namespace");
                } else {
                    call.insert(
                        "namespace".to_owned(),
                        Value::String(identity.namespace.clone()),
                    );
                }
                let input = decode_custom_tool_input(identity, string_field(call, "arguments"));
                call.insert("input".to_owned(), Value::String(input));
                call.remove("arguments");
            }
            ToolKind::ToolSearch => {
                call.insert(
                    "type".to_owned(),
                    Value::String("tool_search_call".to_owned()),
                );
                call.insert("execution".to_owned(), Value::String("client".to_owned()));
                let arguments = decode_tool_search_arguments(call.get("arguments"));
                call.insert("arguments".to_owned(), arguments);
                call.remove("name");
                call.remove("namespace");
            }
            ToolKind::ApplyPatch => {
                let operation = decode_apply_patch_arguments(call.get("arguments"))?;
                call.insert(
                    "type".to_owned(),
                    Value::String("apply_patch_call".to_owned()),
                );
                call.insert("operation".to_owned(), Value::Object(operation));
                call.remove("name");
                call.remove("namespace");
                call.remove("arguments");
            }
        }
        Ok(())
    }

    fn rewrite_apply_patch_done_event(
        &mut self,
        mut value: Value,
    ) -> Result<Vec<GrokTransformedWireEvent>, GrokRequestEncodeError> {
        let original_item = value
            .get("item")
            .and_then(Value::as_object)
            .cloned()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        self.rewrite_response_value(&mut value)?;
        let done = value
            .as_object()
            .cloned()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let done_item = done
            .get("item")
            .and_then(Value::as_object)
            .cloned()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let state = self
            .primary_stream_call(&original_item)
            .and_then(|primary| self.take_stream_call(&primary));
        let mut added = state
            .and_then(|state| state.added_payload)
            .unwrap_or_else(|| {
                Map::from_iter([(
                    "type".to_owned(),
                    Value::String("response.output_item.added".to_owned()),
                )])
            });
        added.insert(
            "type".to_owned(),
            Value::String("response.output_item.added".to_owned()),
        );
        for key in ["output_index", "sequence_number"] {
            if !added.contains_key(key)
                && let Some(value) = done.get(key)
            {
                added.insert(key.to_owned(), value.clone());
            }
        }
        let mut added_item = done_item;
        added_item.insert("status".to_owned(), Value::String("in_progress".to_owned()));
        added.insert("item".to_owned(), Value::Object(added_item));
        Ok(vec![
            GrokTransformedWireEvent {
                event_type: "response.output_item.added".to_owned(),
                value: Value::Object(added),
            },
            GrokTransformedWireEvent {
                event_type: "response.output_item.done".to_owned(),
                value: Value::Object(done),
            },
        ])
    }

    fn cache_filter_enabled(&self) -> bool {
        self.filter_x_search || !self.injected_tool_types.is_empty()
    }

    fn is_internal_cache_call(&self, item: &Value) -> bool {
        let Some(item) = item.as_object() else {
            return false;
        };
        let kind = string_field(item, "type").trim();
        if kind == "web_search_call" {
            return self.injected_tool_types.contains("web_search");
        }
        if !matches!(kind, "custom_tool_call" | "function_call") {
            return false;
        }
        if string_field(item, "call_id").trim().starts_with("xs_call") {
            return true;
        }
        let name = string_field(item, "name").trim();
        matches!(
            name,
            "x_user_search" | "x_semantic_search" | "x_keyword_search" | "x_thread_fetch"
        ) && string_field(item, "namespace").trim().is_empty()
            && !self.client_declared_tools.contains(name)
    }

    fn record_dropped_cache_item(&mut self, payload: &Value) {
        if let Some(index) = payload.get("output_index").and_then(Value::as_u64) {
            self.dropped_output_indexes.insert(index);
        }
        let Some(item) = payload.get("item").and_then(Value::as_object) else {
            return;
        };
        for field in ["id", "call_id"] {
            if let Some(value) = item
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                self.dropped_item_ids.insert(value.to_owned());
            }
        }
    }

    fn references_dropped_cache_item(&self, payload: &Value) -> bool {
        if payload
            .get("output_index")
            .and_then(Value::as_u64)
            .is_some_and(|index| self.dropped_output_indexes.contains(&index))
        {
            return true;
        }
        ["item_id", "call_id"].into_iter().any(|field| {
            payload
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .is_some_and(|value| self.dropped_item_ids.contains(value))
        })
    }

    fn compact_cache_output_index(&self, payload: &mut Value) {
        let Some(index) = payload.get("output_index").and_then(Value::as_u64) else {
            return;
        };
        let removed_before = self
            .dropped_output_indexes
            .iter()
            .filter(|dropped| **dropped < index)
            .count() as u64;
        if removed_before > 0
            && let Some(payload) = payload.as_object_mut()
        {
            payload.insert(
                "output_index".to_owned(),
                Value::from(index.saturating_sub(removed_before)),
            );
        }
    }

    fn filter_cache_envelope(&self, payload: &mut Value) -> Result<(), GrokRequestEncodeError> {
        let Some(payload) = payload.as_object_mut() else {
            return Ok(());
        };
        self.filter_cache_output(payload)?;
        self.filter_injected_cache_tools(payload)?;
        if let Some(response) = payload.get_mut("response").and_then(Value::as_object_mut) {
            self.filter_cache_output(response)?;
            self.filter_injected_cache_tools(response)?;
        }
        Ok(())
    }

    fn filter_cache_output(
        &self,
        envelope: &mut Map<String, Value>,
    ) -> Result<(), GrokRequestEncodeError> {
        let Some(output) = envelope.get_mut("output") else {
            return Ok(());
        };
        let output = output
            .as_array_mut()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        output.retain(|item| !self.is_internal_cache_call(item));
        Ok(())
    }

    fn filter_injected_cache_tools(
        &self,
        envelope: &mut Map<String, Value>,
    ) -> Result<(), GrokRequestEncodeError> {
        if self.injected_tool_types.is_empty() {
            return Ok(());
        }
        let Some(tools) = envelope.get_mut("tools") else {
            return Ok(());
        };
        let tools = tools
            .as_array_mut()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        tools.retain(|tool| {
            tool.get("type")
                .and_then(Value::as_str)
                .is_none_or(|kind| !self.injected_tool_types.contains(kind.trim()))
        });
        if tools.is_empty() {
            envelope.remove("tools");
        }
        Ok(())
    }

    fn restore_visible_tools(&self, response: &mut Map<String, Value>) {
        if response.contains_key("tools") {
            if self.visible_tools.is_empty() {
                response.remove("tools");
            } else {
                response.insert("tools".to_owned(), Value::Array(self.visible_tools.clone()));
            }
        }
    }
}

fn custom_tool_stream_payload(
    source: &Map<String, Value>,
    kind: &str,
    value_key: &str,
    value: &str,
) -> Value {
    let mut result = Map::from_iter([
        ("type".to_owned(), Value::String(kind.to_owned())),
        (value_key.to_owned(), Value::String(value.to_owned())),
    ]);
    for key in ["item_id", "output_index", "sequence_number"] {
        if let Some(value) = source.get(key) {
            result.insert(key.to_owned(), value.clone());
        }
    }
    Value::Object(result)
}

fn decode_custom_tool_input(identity: &ToolIdentity, arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get(identity.custom_argument_field())
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| arguments.to_owned())
}

fn decode_tool_search_arguments(value: Option<&Value>) -> Value {
    let Some(text) = value.and_then(Value::as_str) else {
        return value.cloned().unwrap_or(Value::Object(Map::new()));
    };
    if text.trim().is_empty() {
        return Value::Object(Map::new());
    }
    serde_json::from_str(text)
        .unwrap_or_else(|_| json_object([("input", Value::String(text.to_owned()))]))
}

fn decode_apply_patch_arguments(
    value: Option<&Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let text = value
        .and_then(Value::as_str)
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    let wrapper = serde_json::from_str::<Value>(text)
        .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization)?;
    validate_apply_patch_operation(wrapper.get("operation"))
}

fn rewrite_legacy_local_shell_call(call: &mut Map<String, Value>) {
    let commands = call
        .get("action")
        .and_then(Value::as_object)
        .and_then(|action| action.get("commands"))
        .and_then(Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    call.insert(
        "type".to_owned(),
        Value::String("local_shell_call".to_owned()),
    );
    call.insert(
        "action".to_owned(),
        json_object([
            ("type", Value::String("exec".to_owned())),
            ("command", Value::String(commands)),
        ]),
    );
    call.remove("max_output_length");
}

impl fmt::Debug for GrokResponseTransform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokResponseTransform")
            .field("alias_count", &self.aliases.len())
            .field("function_schema_count", &self.function_schemas.len())
            .field("visible_tool_count", &self.visible_tools.len())
            .field("legacy_local_shell", &self.legacy_local_shell)
            .field("filter_x_search", &self.filter_x_search)
            .field("injected_tool_types", &self.injected_tool_types)
            .finish()
    }
}

struct ToolNormalizer {
    response: GrokResponseTransform,
    identity_aliases: BTreeMap<ToolIdentity, String>,
    deferred_surfaces: Vec<String>,
    client_search_tool: Option<Map<String, Value>>,
    server_search_eager: bool,
    native_shell: bool,
}

struct NormalizedInputItems {
    items: Vec<Value>,
    loaded_tools: Vec<Value>,
    visible_tools: Vec<Value>,
}

struct NormalizedToolSearchOutput {
    history: Map<String, Value>,
    loaded_tools: Vec<Value>,
    visible_tools: Vec<Value>,
}

impl ToolNormalizer {
    fn new() -> Self {
        Self {
            response: GrokResponseTransform::default(),
            identity_aliases: BTreeMap::new(),
            deferred_surfaces: Vec::new(),
            client_search_tool: None,
            server_search_eager: false,
            native_shell: false,
        }
    }

    fn for_replay(response: GrokResponseTransform) -> Self {
        let identity_aliases = response
            .aliases
            .iter()
            .map(|(alias, identity)| (identity.clone(), alias.clone()))
            .collect();
        Self {
            response,
            identity_aliases,
            deferred_surfaces: Vec::new(),
            client_search_tool: None,
            server_search_eager: false,
            native_shell: false,
        }
    }

    fn normalize(
        mut self,
        payload: &mut Map<String, Value>,
    ) -> Result<GrokResponseTransform, GrokRequestEncodeError> {
        let (tools, had_tools) =
            optional_array(payload.get("tools")).map_err(|error| error.at_field("tools"))?;
        if had_tools {
            self.response.visible_tools.clone_from(&tools);
        }
        let client_search = inspect_tool_search(&tools).map_err(|error| error.at_field("tools"))?;
        self.normalize_client_search_parallel(payload, client_search)
            .map_err(|error| error.at_field("parallel_tool_calls"))?;

        let mut normalized_tools = Vec::with_capacity(tools.len());
        for raw_tool in &tools {
            normalized_tools.extend(
                self.normalize_tool(raw_tool, "", client_search, false)
                    .map_err(|error| error.at_field("tools"))?,
            );
        }

        if let Some(Value::Array(items)) = payload.get("input") {
            let normalized = self
                .normalize_input_items(items)
                .map_err(|error| error.at_field("input"))?;
            normalized_tools.extend(normalized.loaded_tools);
            self.response.visible_tools.extend(normalized.visible_tools);
            payload.insert("input".to_owned(), Value::Array(normalized.items));
        } else if payload
            .get("input")
            .is_some_and(|input| !input.is_null() && !input.is_string())
        {
            return Err(GrokRequestEncodeError::InvalidRequestField { field: "input" });
        }

        if self.client_search_tool.is_some() {
            normalized_tools.push(Value::Object(
                self.build_client_search_function()
                    .map_err(|error| error.at_field("tools"))?,
            ));
        }
        normalized_tools = dedupe_normalized_tools(normalized_tools);
        if normalized_tools.is_empty() {
            if had_tools {
                payload.remove("tools");
                payload.remove("parallel_tool_calls");
            }
        } else {
            payload.insert("tools".to_owned(), Value::Array(normalized_tools.clone()));
        }
        self.normalize_tool_choice(payload, &normalized_tools)
            .map_err(|error| error.at_field("tool_choice"))?;
        Ok(self.response)
    }

    fn alias(&mut self, identity: ToolIdentity) -> String {
        if let Some(alias) = self.identity_aliases.get(&identity) {
            return alias.clone();
        }
        let base = match identity.kind {
            ToolKind::ToolSearch => "xai_proxy_tool_search".to_owned(),
            ToolKind::ApplyPatch => "xai_proxy_apply_patch".to_owned(),
            ToolKind::Function | ToolKind::Custom if !identity.namespace.is_empty() => {
                format!("{}__{}", identity.namespace, identity.name)
            }
            ToolKind::Function | ToolKind::Custom => identity.name.clone(),
        };
        let key = format!(
            "{}\0{}\0{}",
            identity.kind as u8, identity.namespace, identity.name
        );
        let mut alias = truncate_tool_alias(&base, &key);
        if self
            .response
            .aliases
            .get(&alias)
            .is_some_and(|existing| existing != &identity)
        {
            alias = hashed_tool_alias(&base, &key);
        }
        self.response
            .aliases
            .insert(alias.clone(), identity.clone());
        self.identity_aliases.insert(identity, alias.clone());
        alias
    }

    fn normalize_client_search_parallel(
        &self,
        payload: &mut Map<String, Value>,
        client_search: bool,
    ) -> Result<(), GrokRequestEncodeError> {
        if !client_search {
            return Ok(());
        }
        match payload.get("parallel_tool_calls") {
            None | Some(Value::Null) => {
                payload.insert("parallel_tool_calls".to_owned(), Value::Bool(false));
            }
            Some(Value::Bool(true)) => {
                payload.insert("parallel_tool_calls".to_owned(), Value::Bool(false));
            }
            Some(Value::Bool(false)) => {}
            Some(_) => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        }
        Ok(())
    }

    fn normalize_tool(
        &mut self,
        raw: &Value,
        namespace: &str,
        client_search: bool,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let tool = raw
            .as_object()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let kind = string_field(tool, "type").trim();
        match kind {
            "function" => self.normalize_function_tool(tool, namespace, client_search, force),
            "namespace" => self.normalize_namespace_tool(tool, client_search, force),
            "tool_search" => self.normalize_tool_search(tool, force),
            "custom" => self.normalize_custom_tool(tool, namespace),
            "web_search"
            | "web_search_preview"
            | "web_search_preview_2025_03_11"
            | "web_search_2025_08_26" => self.normalize_web_search_tool(tool, kind),
            "mcp" => self.normalize_mcp_tool(tool, client_search, force),
            "shell" => self.normalize_shell_tool(tool),
            "local_shell" => self.normalize_legacy_local_shell_tool(tool),
            "apply_patch" => self.normalize_apply_patch_tool(tool),
            "x_search" | "collections_search" | "file_search" | "code_execution"
            | "code_interpreter" => Ok(vec![Value::Object(without_defer_loading(tool))]),
            // 对未获 xAI 接受的工具做静默过滤，并在过滤后同步收敛 tool_choice；
            // 不要把客户端可选扩展升级成整单 400。
            "" | "computer_use_preview" | "image_generation" => Ok(Vec::new()),
            _ => Ok(Vec::new()),
        }
    }

    fn normalize_function_tool(
        &mut self,
        tool: &Map<String, Value>,
        namespace: &str,
        client_search: bool,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let name = string_field(tool, "name").trim();
        if name.is_empty() {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        let deferred = tool
            .get("defer_loading")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if deferred && client_search && !force {
            if namespace.is_empty() {
                self.deferred_surfaces.push(describe_deferred_tool(
                    name,
                    string_field(tool, "description"),
                ));
            }
            return Ok(Vec::new());
        }
        let mut converted = without_defer_loading(tool);
        let function_schema = tool
            .get("parameters")
            .filter(|schema| schema_contains_integer(schema, 0))
            .cloned();
        match converted.get("parameters").cloned() {
            None | Some(Value::Null) => {
                converted.insert(
                    "parameters".to_owned(),
                    json_object([
                        ("type", Value::String("object".to_owned())),
                        ("properties", Value::Object(Map::new())),
                    ]),
                );
            }
            Some(parameters) => {
                if let Some(normalized) = normalize_function_parameters_root(&parameters)? {
                    converted.insert("parameters".to_owned(), normalized);
                }
            }
        }
        let alias = self.alias(ToolIdentity::new(ToolKind::Function, namespace, name));
        if let Some(schema) = function_schema {
            self.response.function_schemas.insert(alias.clone(), schema);
        }
        converted.insert("name".to_owned(), Value::String(alias));
        Ok(vec![Value::Object(converted)])
    }

    fn normalize_namespace_tool(
        &mut self,
        tool: &Map<String, Value>,
        client_search: bool,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let name = string_field(tool, "name").trim();
        let children = tool
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        if name.is_empty() {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        if client_search && !force && namespace_has_deferred_functions(children) {
            self.deferred_surfaces.push(describe_deferred_tool(
                name,
                string_field(tool, "description"),
            ));
        }
        let mut converted = Vec::new();
        for child in children {
            if child.pointer("/type").and_then(Value::as_str) != Some("function") {
                return Err(GrokRequestEncodeError::InvalidRequestNormalization);
            }
            converted.extend(self.normalize_tool(child, name, client_search, force)?);
        }
        Ok(converted)
    }

    fn normalize_tool_search(
        &mut self,
        tool: &Map<String, Value>,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        if force {
            return Ok(Vec::new());
        }
        match string_field(tool, "execution")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "server" => {
                self.server_search_eager = true;
            }
            "client" => {
                self.client_search_tool = Some(tool.clone());
            }
            _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        }
        Ok(Vec::new())
    }

    fn normalize_custom_tool(
        &mut self,
        tool: &Map<String, Value>,
        namespace: &str,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let name = string_field(tool, "name").trim();
        if name.is_empty() || tool.get("format").is_some_and(|value| !value.is_object()) {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        let identity = ToolIdentity::new(ToolKind::Custom, namespace, name);
        let argument_field = identity.custom_argument_field();
        let description = if identity.is_root_custom_apply_patch() {
            "The apply_patch tool edits files using Codex patch format. Provide the complete raw patch text in the patch string field.".to_owned()
        } else {
            let mut description = string_field(tool, "description").trim().to_owned();
            if !description.is_empty() {
                description.push('\n');
            }
            description.push_str("Provide the custom tool input in the input string field.");
            description
        };
        let alias = self.alias(identity);
        Ok(vec![json_object([
            ("type", Value::String("function".to_owned())),
            ("name", Value::String(alias)),
            ("description", Value::String(description)),
            (
                "parameters",
                json_object([
                    ("type", Value::String("object".to_owned())),
                    (
                        "properties",
                        json_object([(
                            argument_field,
                            json_object([("type", Value::String("string".to_owned()))]),
                        )]),
                    ),
                    (
                        "required",
                        Value::Array(vec![Value::String(argument_field.to_owned())]),
                    ),
                    ("additionalProperties", Value::Bool(false)),
                ]),
            ),
        ])])
    }

    fn build_client_search_function(
        &mut self,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let tool = self
            .client_search_tool
            .as_ref()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let mut description = string_field(tool, "description").trim().to_owned();
        if description.is_empty() {
            description.push_str("Search for tools needed to continue the task.");
        }
        if !self.deferred_surfaces.is_empty() {
            description.push_str("\nDeferred tool surfaces available to search:\n- ");
            description.push_str(&self.deferred_surfaces.join("\n- "));
        }
        description.truncate(description.len().min(MAX_TOOL_SEARCH_DESCRIPTION_BYTES));
        let parameters = match tool.get("parameters") {
            None => json_object([
                ("type", Value::String("object".to_owned())),
                ("properties", Value::Object(Map::new())),
                ("additionalProperties", Value::Bool(true)),
            ]),
            Some(Value::Object(parameters)) => Value::Object(parameters.clone()),
            Some(_) => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        };
        let alias = self.alias(ToolIdentity::new(ToolKind::ToolSearch, "", "tool_search"));
        Ok(Map::from_iter([
            ("type".to_owned(), Value::String("function".to_owned())),
            ("name".to_owned(), Value::String(alias)),
            ("description".to_owned(), Value::String(description)),
            ("parameters".to_owned(), parameters),
        ]))
    }
}

fn optional_array(value: Option<&Value>) -> Result<(Vec<Value>, bool), GrokRequestEncodeError> {
    match value {
        None | Some(Value::Null) => Ok((Vec::new(), false)),
        Some(Value::Array(values)) => Ok((values.clone(), true)),
        Some(_) => Err(GrokRequestEncodeError::InvalidRequestNormalization),
    }
}

/// Grok Build 要求 function parameters 的根节点必为 object；Codex 会为可选参数
/// 生成 `object | null`，这里只移除根节点 nullability，嵌套字段保持原样。
fn normalize_function_parameters_root(
    value: &Value,
) -> Result<Option<Value>, GrokRequestEncodeError> {
    let Some(schema) = value.as_object() else {
        return Ok(None);
    };
    let mut normalized = schema.clone();
    let mut changed = false;

    if let Some(types) = normalized.get("type").and_then(Value::as_array).cloned() {
        let removed_null = types.iter().any(|value| value.as_str() == Some("null"));
        if removed_null {
            let remaining = types
                .into_iter()
                .filter(|value| value.as_str() != Some("null"))
                .collect::<Vec<_>>();
            if remaining.len() != 1 || remaining[0].as_str() != Some("object") {
                return Err(invalid_function_parameters_root());
            }
            normalized.insert("type".to_owned(), Value::String("object".to_owned()));
            changed = true;
        }
    }

    for keyword in ["anyOf", "oneOf"] {
        let Some(branches) = normalized.get(keyword).and_then(Value::as_array).cloned() else {
            continue;
        };
        let removed_null = branches.iter().any(is_null_only_schema);
        if !removed_null {
            continue;
        }
        let remaining = branches
            .into_iter()
            .filter(|branch| !is_null_only_schema(branch))
            .collect::<Vec<_>>();
        if remaining.is_empty()
            || remaining.iter().any(|branch| {
                branch.as_object().is_none_or(|branch| {
                    !is_object_root_schema(branch, &normalized, &mut BTreeSet::new())
                })
            })
        {
            return Err(invalid_function_parameters_root());
        }
        if remaining.len() == 1 && normalized.len() == 1 {
            normalized = remaining[0]
                .as_object()
                .cloned()
                .ok_or_else(invalid_function_parameters_root)?;
            normalized.insert("type".to_owned(), Value::String("object".to_owned()));
        } else {
            normalized.insert(keyword.to_owned(), Value::Array(remaining));
            normalized.insert("type".to_owned(), Value::String("object".to_owned()));
        }
        changed = true;
    }

    Ok(changed.then_some(Value::Object(normalized)))
}

fn is_null_only_schema(value: &Value) -> bool {
    let Some(schema) = value.as_object() else {
        return false;
    };
    match schema.get("type") {
        Some(Value::String(kind)) => kind == "null",
        Some(Value::Array(types)) if !types.is_empty() => {
            types.iter().all(|kind| kind.as_str() == Some("null"))
        }
        _ => false,
    }
}

fn is_object_root_schema(
    schema: &Map<String, Value>,
    root: &Map<String, Value>,
    visited: &mut BTreeSet<String>,
) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => return kind == "object",
        Some(Value::Array(types)) if !types.is_empty() => {
            return types.iter().all(|kind| kind.as_str() == Some("object"));
        }
        Some(_) => return false,
        None => {}
    }
    if schema.contains_key("properties") {
        return true;
    }
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return false;
    };
    if !visited.insert(reference.to_owned()) {
        return false;
    }
    resolve_local_schema_ref(root, reference)
        .is_some_and(|resolved| is_object_root_schema(resolved, root, visited))
}

fn resolve_local_schema_ref<'a>(
    root: &'a Map<String, Value>,
    reference: &str,
) -> Option<&'a Map<String, Value>> {
    if reference == "#" {
        return Some(root);
    }
    let path = reference.strip_prefix("#/")?;
    let mut segments = path.split('/');
    let first = decode_json_pointer_segment(segments.next()?);
    let mut current = root.get(&first)?;
    for segment in segments {
        let segment = decode_json_pointer_segment(segment);
        current = current.as_object()?.get(&segment)?;
    }
    current.as_object()
}

fn decode_json_pointer_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

const fn invalid_function_parameters_root() -> GrokRequestEncodeError {
    GrokRequestEncodeError::InvalidRequestField {
        field: "tools[].parameters",
    }
}

fn inspect_tool_search(tools: &[Value]) -> Result<bool, GrokRequestEncodeError> {
    let mut client_search = false;
    let mut server_search = false;
    for raw_tool in tools {
        let tool = raw_tool
            .as_object()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        if string_field(tool, "type") != "tool_search" {
            continue;
        }
        match string_field(tool, "execution")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "server" if !client_search => server_search = true,
            "client" if !client_search && !server_search => client_search = true,
            _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        }
    }
    Ok(client_search)
}

fn string_field<'a>(value: &'a Map<String, Value>, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn json_object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(Map::from_iter(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
    ))
}

fn without_defer_loading(tool: &Map<String, Value>) -> Map<String, Value> {
    let mut converted = tool.clone();
    converted.remove("defer_loading");
    converted
}

fn namespace_has_deferred_functions(children: &[Value]) -> bool {
    children.iter().any(|child| {
        child.pointer("/type").and_then(Value::as_str) == Some("function")
            && child
                .pointer("/defer_loading")
                .and_then(Value::as_bool)
                .unwrap_or(false)
    })
}

fn describe_deferred_tool(name: &str, description: &str) -> String {
    let description = description.trim();
    if description.is_empty() {
        return name.to_owned();
    }
    let description = description.chars().take(240).collect::<String>();
    format!("{name}: {description}")
}

fn truncate_tool_alias(base: &str, key: &str) -> String {
    if base.len() <= MAX_BUILD_TOOL_ALIAS_LENGTH {
        base.to_owned()
    } else {
        hashed_tool_alias(base, key)
    }
}

fn hashed_tool_alias(base: &str, key: &str) -> String {
    let suffix = format!("__{}", short_tool_hash(key));
    let limit = MAX_BUILD_TOOL_ALIAS_LENGTH.saturating_sub(suffix.len());
    let mut end = limit.min(base.len());
    while !base.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{suffix}", &base[..end])
}

fn short_tool_hash(value: &str) -> String {
    Sha256::digest(value)
        .iter()
        .take(5)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..9]
        .to_owned()
}

fn dedupe_normalized_tools(tools: Vec<Value>) -> Vec<Value> {
    let mut result = Vec::with_capacity(tools.len());
    let mut seen = BTreeSet::new();
    for tool in tools {
        if normalized_tool_dedupe_key(&tool).is_none_or(|key| seen.insert(key)) {
            result.push(tool);
        }
    }
    result
}

fn normalized_tool_dedupe_key(tool: &Value) -> Option<String> {
    let object = tool.as_object()?;
    let kind = string_field(object, "type").trim();
    if !kind.is_empty() {
        if let Some(name) = object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return Some(format!("type:{kind}\0name:{name}"));
        }
        if kind == "mcp"
            && let Some(label) = object
                .get("server_label")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
        {
            return Some(format!("type:mcp\0server_label:{label}"));
        }
    }
    serde_json::to_string(tool)
        .ok()
        .map(|encoded| format!("json:{encoded}"))
}

impl ToolNormalizer {
    fn normalize_web_search_tool(
        &mut self,
        tool: &Map<String, Value>,
        kind: &str,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let filters = normalize_web_search_filters(tool)?;
        if let Some(content_types) = tool.get("search_content_types") {
            content_types
                .as_array()
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        }
        if kind == "web_search" && tool.len() == 1 {
            return Ok(vec![Value::Object(tool.clone())]);
        }
        let mut converted =
            Map::from_iter([("type".to_owned(), Value::String("web_search".to_owned()))]);
        if let Some(filters) = filters {
            converted.insert("filters".to_owned(), Value::Object(filters));
        }
        Ok(vec![Value::Object(converted)])
    }

    fn normalize_mcp_tool(
        &mut self,
        tool: &Map<String, Value>,
        client_search: bool,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let deferred = tool
            .get("defer_loading")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if deferred && client_search && !force {
            let label = ["server_label", "name"]
                .into_iter()
                .find_map(|field| tool.get(field).and_then(Value::as_str))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            self.deferred_surfaces.push(describe_deferred_tool(
                label,
                string_field(tool, "description"),
            ));
            return Ok(Vec::new());
        }
        Ok(vec![Value::Object(without_defer_loading(tool))])
    }

    fn normalize_shell_tool(
        &mut self,
        tool: &Map<String, Value>,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        if self.response.legacy_local_shell {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        self.native_shell = true;
        Ok(vec![Value::Object(without_defer_loading(tool))])
    }

    fn normalize_legacy_local_shell_tool(
        &mut self,
        _tool: &Map<String, Value>,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        if self.native_shell || self.response.legacy_local_shell {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        self.response.legacy_local_shell = true;
        Ok(vec![json_object([
            ("type", Value::String("shell".to_owned())),
            (
                "environment",
                json_object([("type", Value::String("local".to_owned()))]),
            ),
        ])])
    }

    fn normalize_apply_patch_tool(
        &mut self,
        _tool: &Map<String, Value>,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let alias = self.alias(ToolIdentity::new(ToolKind::ApplyPatch, "", "apply_patch"));
        let operation = json_object([
            ("type", Value::String("object".to_owned())),
            (
                "properties",
                json_object([
                    (
                        "type",
                        json_object([
                            ("type", Value::String("string".to_owned())),
                            (
                                "enum",
                                Value::Array(
                                    ["create_file", "update_file", "delete_file"]
                                        .into_iter()
                                        .map(|value| Value::String(value.to_owned()))
                                        .collect(),
                                ),
                            ),
                        ]),
                    ),
                    (
                        "path",
                        json_object([
                            ("type", Value::String("string".to_owned())),
                            ("minLength", Value::from(1)),
                        ]),
                    ),
                    (
                        "diff",
                        json_object([("type", Value::String("string".to_owned()))]),
                    ),
                ]),
            ),
            (
                "required",
                Value::Array(
                    ["type", "path"]
                        .into_iter()
                        .map(|value| Value::String(value.to_owned()))
                        .collect(),
                ),
            ),
            ("additionalProperties", Value::Bool(false)),
        ]);
        Ok(vec![json_object([
            ("type", Value::String("function".to_owned())),
            ("name", Value::String(alias)),
            (
                "description",
                Value::String(
                    "Create, update, or delete one file using a structured V4A patch operation. create_file and update_file require path and diff; delete_file requires path."
                        .to_owned(),
                ),
            ),
            (
                "parameters",
                json_object([
                    ("type", Value::String("object".to_owned())),
                    ("properties", json_object([("operation", operation)])),
                    (
                        "required",
                        Value::Array(vec![Value::String("operation".to_owned())]),
                    ),
                    ("additionalProperties", Value::Bool(false)),
                ]),
            ),
            ("strict", Value::Bool(true)),
        ])])
    }

    fn normalize_tool_choice(
        &mut self,
        payload: &mut Map<String, Value>,
        normalized_tools: &[Value],
    ) -> Result<(), GrokRequestEncodeError> {
        let Some(choice) = payload.get("tool_choice").cloned() else {
            return Ok(());
        };
        if choice.is_null() {
            return Ok(());
        }
        if normalized_tools.is_empty() {
            payload.remove("tool_choice");
            return Ok(());
        }
        let Some(mut object) = choice.as_object().cloned() else {
            return choice
                .as_str()
                .filter(|value| matches!(*value, "none" | "auto" | "required"))
                .map(|_| ())
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization);
        };
        let kind = string_field(&object, "type").to_owned();
        match kind.as_str() {
            "tool_search" => {
                if self.client_search_tool.is_none() {
                    if self.server_search_eager {
                        payload.insert("tool_choice".to_owned(), Value::String("auto".to_owned()));
                        return Ok(());
                    }
                    return Err(GrokRequestEncodeError::InvalidRequestNormalization);
                }
                let alias = self.alias(ToolIdentity::new(ToolKind::ToolSearch, "", "tool_search"));
                payload.insert(
                    "tool_choice".to_owned(),
                    json_object([
                        ("type", Value::String("function".to_owned())),
                        ("name", Value::String(alias)),
                    ]),
                );
            }
            "custom" => {
                let identity = ToolIdentity::new(
                    ToolKind::Custom,
                    string_field(&object, "namespace").trim(),
                    string_field(&object, "name").trim(),
                );
                let alias = self
                    .identity_aliases
                    .get(&identity)
                    .cloned()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                object.insert("type".to_owned(), Value::String("function".to_owned()));
                object.insert("name".to_owned(), Value::String(alias));
                object.remove("namespace");
                payload.insert("tool_choice".to_owned(), Value::Object(object));
            }
            "apply_patch" => {
                let identity = ToolIdentity::new(ToolKind::ApplyPatch, "", "apply_patch");
                let alias = self
                    .identity_aliases
                    .get(&identity)
                    .cloned()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                payload.insert(
                    "tool_choice".to_owned(),
                    json_object([
                        ("type", Value::String("function".to_owned())),
                        ("name", Value::String(alias)),
                    ]),
                );
            }
            "allowed_tools" => {
                payload.remove("tool_choice");
            }
            "function" => {
                self.normalize_function_tool_choice(payload, object, normalized_tools)?;
            }
            _ => {
                if let Some(hosted_kind) = normalize_hosted_tool_choice_kind(&kind) {
                    let matching = tools_of_type(normalized_tools, hosted_kind);
                    if matching.is_empty() {
                        payload.remove("tool_choice");
                        return Ok(());
                    }
                    object.insert("type".to_owned(), Value::String(hosted_kind.to_owned()));
                    payload.insert("tool_choice".to_owned(), Value::Object(object));
                } else {
                    payload.remove("tool_choice");
                }
            }
        }
        Ok(())
    }

    fn normalize_function_tool_choice(
        &self,
        payload: &mut Map<String, Value>,
        mut object: Map<String, Value>,
        normalized_tools: &[Value],
    ) -> Result<(), GrokRequestEncodeError> {
        if let Some(Value::Object(function)) = object.get_mut("function") {
            rewrite_namespace_choice(function, &self.identity_aliases)?;
            let name = string_field(function, "name").trim();
            if !name.is_empty() && !has_named_tool(normalized_tools, "function", name) {
                payload.remove("tool_choice");
                return Ok(());
            }
            payload.insert("tool_choice".to_owned(), Value::Object(object));
            return Ok(());
        }
        rewrite_namespace_choice(&mut object, &self.identity_aliases)?;
        let name = string_field(&object, "name").trim();
        if !name.is_empty() && !has_named_tool(normalized_tools, "function", name) {
            payload.remove("tool_choice");
            return Ok(());
        }
        payload.insert("tool_choice".to_owned(), Value::Object(object));
        Ok(())
    }
}

fn normalize_web_search_filters(
    tool: &Map<String, Value>,
) -> Result<Option<Map<String, Value>>, GrokRequestEncodeError> {
    let nested = match tool.get("filters") {
        None | Some(Value::Null) => None,
        Some(Value::Object(filters)) => filters
            .get("allowed_domains")
            .map(normalize_allowed_domains)
            .transpose()?,
        Some(_) => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
    };
    let top_level = tool
        .get("allowed_domains")
        .map(normalize_allowed_domains)
        .transpose()?;
    if nested.is_some() && top_level.is_some() && nested != top_level {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    }
    Ok(nested
        .or(top_level)
        .map(|domains| Map::from_iter([("allowed_domains".to_owned(), Value::Array(domains))])))
}

fn normalize_allowed_domains(value: &Value) -> Result<Vec<Value>, GrokRequestEncodeError> {
    let domains = value
        .as_array()
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    if domains
        .iter()
        .any(|domain| domain.as_str().map(str::trim).is_none_or(str::is_empty))
    {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    }
    Ok(domains.clone())
}

fn normalize_hosted_tool_choice_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "web_search"
        | "web_search_preview"
        | "web_search_preview_2025_03_11"
        | "web_search_2025_08_26" => Some("web_search"),
        "x_search" => Some("x_search"),
        "image_generation" => Some("image_generation"),
        "collections_search" => Some("collections_search"),
        "file_search" => Some("file_search"),
        "code_execution" => Some("code_execution"),
        "code_interpreter" => Some("code_interpreter"),
        "mcp" => Some("mcp"),
        "shell" | "local_shell" => Some("shell"),
        _ => None,
    }
}

fn has_tool_type(tools: &[Value], kind: &str) -> bool {
    tools
        .iter()
        .any(|tool| tool.pointer("/type").and_then(Value::as_str) == Some(kind))
}

fn has_named_tool(tools: &[Value], kind: &str, name: &str) -> bool {
    tools.iter().any(|tool| {
        tool.pointer("/type").and_then(Value::as_str) == Some(kind)
            && tool.pointer("/name").and_then(Value::as_str) == Some(name)
    })
}

fn tools_of_type(tools: &[Value], kind: &str) -> Vec<Value> {
    tools
        .iter()
        .filter(|tool| tool.pointer("/type").and_then(Value::as_str) == Some(kind))
        .cloned()
        .collect()
}

fn rewrite_namespace_choice(
    object: &mut Map<String, Value>,
    aliases: &BTreeMap<ToolIdentity, String>,
) -> Result<(), GrokRequestEncodeError> {
    let name = string_field(object, "name").trim();
    let namespace = string_field(object, "namespace").trim();
    if name.is_empty() || namespace.is_empty() {
        return Ok(());
    }
    let identity = ToolIdentity::new(ToolKind::Function, namespace, name);
    let alias = aliases
        .get(&identity)
        .cloned()
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    object.insert("name".to_owned(), Value::String(alias));
    object.remove("namespace");
    Ok(())
}

impl ToolNormalizer {
    fn normalize_input_items(
        &mut self,
        items: &[Value],
    ) -> Result<NormalizedInputItems, GrokRequestEncodeError> {
        let mut rewritten = Vec::with_capacity(items.len());
        let mut loaded_tools = Vec::new();
        let mut visible_tools = Vec::new();
        for raw_item in items {
            let Some(item) = raw_item.as_object() else {
                rewritten.push(raw_item.clone());
                continue;
            };
            let mut item_type = string_field(item, "type").trim();
            if item_type.is_empty() && !string_field(item, "role").trim().is_empty() {
                item_type = "message";
            }
            match item_type {
                "message" => rewritten.push(Value::Object(self.normalize_message_input(item)?)),
                "function_call" => {
                    rewritten.push(Value::Object(self.normalize_function_call_input(item)?));
                }
                "function_call_output" => rewritten.push(Value::Object(
                    self.normalize_function_call_output_input(item, true)?,
                )),
                "reasoning" => rewritten.push(Value::Object(sanitize_reasoning_input(item))),
                "file_search_call"
                | "web_search_call"
                | "image_generation_call"
                | "code_interpreter_call"
                | "shell_call"
                | "mcp_list_tools"
                | "mcp_approval_request"
                | "mcp_approval_response"
                | "mcp_call" => {
                    rewritten.push(Value::Object(sanitize_native_history_input(
                        item, item_type,
                    )));
                }
                "compaction" | "compaction_summary" => {
                    rewritten.extend(normalize_compaction_input(item)?);
                }
                "tool_search_call" => {
                    rewritten.push(Value::Object(self.normalize_tool_search_call(item)?));
                }
                "tool_search_output" => {
                    let normalized = self.normalize_tool_search_output(item)?;
                    rewritten.push(Value::Object(normalized.history));
                    loaded_tools.extend(normalized.loaded_tools);
                    visible_tools.extend(normalized.visible_tools);
                }
                "custom_tool_call" => {
                    rewritten.push(Value::Object(self.normalize_custom_tool_call_input(item)?));
                }
                "custom_tool_call_output" => rewritten.push(Value::Object(
                    self.normalize_function_call_output_input(item, false)?,
                )),
                "apply_patch_call" => {
                    rewritten.push(Value::Object(self.normalize_apply_patch_call_input(item)?));
                }
                "apply_patch_call_output" => {
                    rewritten.push(Value::Object(normalize_apply_patch_output_input(item)?));
                }
                "agent_message" => rewritten.push(normalize_agent_message_input(item)),
                "local_shell_call" => {
                    rewritten.push(Value::Object(normalize_legacy_local_shell_call_input(
                        item,
                    )?));
                }
                "local_shell_call_output" => rewritten.push(Value::Object(
                    normalize_legacy_local_shell_output_input(item)?,
                )),
                "shell_call_output" => {
                    rewritten.push(Value::Object(normalize_shell_call_output_input(item)?));
                }
                "mcp_tool_call_output" => rewritten.push(normalize_mcp_output_input(item)?),
                "compaction_trigger" => {
                    return Err(GrokRequestEncodeError::InvalidRequestNormalization);
                }
                "additional_tools" => {
                    let (tools, visible) = self.normalize_additional_tools_input(item)?;
                    loaded_tools.extend(tools);
                    visible_tools.extend(visible);
                }
                "" => rewritten.push(raw_item.clone()),
                unsupported => {
                    rewritten.push(unsupported_input_history_boundary(item, unsupported))
                }
            }
        }
        Ok(NormalizedInputItems {
            items: rewritten,
            loaded_tools,
            visible_tools,
        })
    }

    fn normalize_message_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let role = match string_field(item, "role").trim() {
            "" => "assistant",
            "model" => "assistant",
            role => role,
        };
        let content = self.normalize_message_content(item.get("content"), role)?;
        // 透明代理合法形态：只覆盖归一化字段并剥离 Grok 注入键，未知官方
        // 字段原样保留。
        let mut converted = item.clone();
        strip_grok_internal_keys(&mut converted);
        converted.insert("type".to_owned(), Value::String("message".to_owned()));
        converted.insert("role".to_owned(), Value::String(role.to_owned()));
        converted.insert("content".to_owned(), content);
        Ok(converted)
    }

    fn normalize_message_content(
        &mut self,
        value: Option<&Value>,
        role: &str,
    ) -> Result<Value, GrokRequestEncodeError> {
        if let Some(Value::String(text)) = value {
            return Ok(Value::String(text.clone()));
        }
        let items = value
            .and_then(Value::as_array)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        if role == "assistant" {
            let texts = items
                .iter()
                .map(|raw| {
                    let item = raw.as_object()?;
                    match string_field(item, "type") {
                        "text" | "input_text" | "output_text" => {
                            item.get("text").and_then(Value::as_str)
                        }
                        "refusal" => item.get("refusal").and_then(Value::as_str),
                        _ => None,
                    }
                })
                .collect::<Option<Vec<_>>>();
            if let Some(texts) = texts {
                return Ok(Value::String(texts.join("\n")));
            }
        }
        let text_part_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        let mut normalized = Vec::with_capacity(items.len());
        for raw in items {
            let item = raw
                .as_object()
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            let converted = match string_field(item, "type") {
                "text" | "input_text" | "output_text" => json_object([
                    ("type", Value::String(text_part_type.to_owned())),
                    ("text", Value::String(string_field(item, "text").to_owned())),
                ]),
                "refusal" => json_object([
                    ("type", Value::String(text_part_type.to_owned())),
                    (
                        "text",
                        Value::String(string_field(item, "refusal").to_owned()),
                    ),
                ]),
                "input_image" => Value::Object(self.normalize_input_image_part(item)?),
                "input_file" => Value::Object(normalize_input_file_part(item)),
                _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
            };
            normalized.push(converted);
        }
        Ok(Value::Array(normalized))
    }

    fn normalize_input_image_part(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let detail = match item.get("detail") {
            None | Some(Value::Null) => "auto",
            Some(Value::String(detail)) if detail.trim().is_empty() => "auto",
            Some(Value::String(detail)) => detail.trim(),
            Some(_) => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        };
        let detail = match detail {
            "auto" | "low" | "high" => detail,
            "original" => "high",
            _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        };
        let mut converted = Map::from_iter([
            ("type".to_owned(), Value::String("input_image".to_owned())),
            ("detail".to_owned(), Value::String(detail.to_owned())),
        ]);
        if let Some(value) = item.get("image_url").or_else(|| item.get("url"))
            && !value.is_null()
        {
            converted.insert("image_url".to_owned(), value.clone());
        }
        if let Some(value) = item.get("file_id")
            && !value.is_null()
        {
            converted.insert("file_id".to_owned(), value.clone());
        }
        Ok(converted)
    }

    fn normalize_function_call_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let mut name = required_trimmed_string(item, "name")?.to_owned();
        let call_id = required_trimmed_string(item, "call_id")?;
        let arguments = encode_function_arguments(item.get("arguments"))?;
        let namespace = string_field(item, "namespace").trim();
        if !namespace.is_empty() {
            name = self.alias(ToolIdentity::new(ToolKind::Function, namespace, &name));
        }
        let mut converted = item.clone();
        strip_grok_internal_keys(&mut converted);
        converted.remove("namespace");
        converted.insert("type".to_owned(), Value::String("function_call".to_owned()));
        converted.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
        converted.insert("name".to_owned(), Value::String(name));
        converted.insert("arguments".to_owned(), Value::String(arguments));
        Ok(converted)
    }

    fn normalize_custom_tool_call_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let name = required_trimmed_string(item, "name")?;
        let input = item
            .get("input")
            .and_then(Value::as_str)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let call_id = required_trimmed_string(item, "call_id")?;
        let namespace = string_field(item, "namespace").trim();
        let identity = ToolIdentity::new(ToolKind::Custom, namespace, name);
        let arguments = serde_json::to_string(&json_object([(
            identity.custom_argument_field(),
            Value::String(input.to_owned()),
        )]))
        .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization)?;
        let alias = self.alias(identity);
        let mut converted = item.clone();
        strip_grok_internal_keys(&mut converted);
        converted.remove("namespace");
        converted.remove("input");
        converted.insert("type".to_owned(), Value::String("function_call".to_owned()));
        converted.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
        converted.insert("name".to_owned(), Value::String(alias));
        converted.insert("arguments".to_owned(), Value::String(arguments));
        Ok(converted)
    }

    fn normalize_function_call_output_input(
        &mut self,
        item: &Map<String, Value>,
        allow_content_blocks: bool,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let call_id = required_trimmed_string(item, "call_id")?;
        let output = match item.get("output") {
            Some(Value::Array(blocks))
                if allow_content_blocks && is_function_output_content_array(blocks) =>
            {
                Value::Array(self.normalize_function_output_blocks(blocks)?)
            }
            output => Value::String(encode_tool_output(output)?),
        };
        let mut converted = item.clone();
        strip_grok_internal_keys(&mut converted);
        converted.insert(
            "type".to_owned(),
            Value::String("function_call_output".to_owned()),
        );
        converted.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
        converted.insert("output".to_owned(), output);
        Ok(converted)
    }

    fn normalize_function_output_blocks(
        &mut self,
        blocks: &[Value],
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        blocks
            .iter()
            .map(|raw| {
                let block = raw
                    .as_object()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                match string_field(block, "type") {
                    "input_text" => Ok(json_object([
                        ("type", Value::String("input_text".to_owned())),
                        (
                            "text",
                            Value::String(
                                block
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?
                                    .to_owned(),
                            ),
                        ),
                    ])),
                    "input_image" => {
                        require_content_source(block, &["image_url", "file_id"])?;
                        self.normalize_input_image_part(block).map(Value::Object)
                    }
                    "input_file" => {
                        require_content_source(block, &["file_data", "file_id", "file_url"])?;
                        Ok(Value::Object(normalize_input_file_part(block)))
                    }
                    _ => Err(GrokRequestEncodeError::InvalidRequestNormalization),
                }
            })
            .collect()
    }

    fn normalize_tool_search_call(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let call_id = required_trimmed_string(item, "call_id")?;
        let execution = string_field(item, "execution").trim().to_ascii_lowercase();
        if execution.is_empty() || execution == "server" {
            self.server_search_eager = true;
            return Ok(boundary_message(
                "A server-side tool search occurred here; selected tools are made available directly.",
            ));
        }
        if execution != "client" {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        let arguments = encode_function_arguments(item.get("arguments"))?;
        let alias = self.alias(ToolIdentity::new(ToolKind::ToolSearch, "", "tool_search"));
        Ok(Map::from_iter([
            ("type".to_owned(), Value::String("function_call".to_owned())),
            ("call_id".to_owned(), Value::String(call_id.to_owned())),
            ("name".to_owned(), Value::String(alias)),
            ("arguments".to_owned(), Value::String(arguments)),
        ]))
    }

    fn normalize_tool_search_output(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<NormalizedToolSearchOutput, GrokRequestEncodeError> {
        let execution = string_field(item, "execution").trim().to_ascii_lowercase();
        if !matches!(execution.as_str(), "" | "client" | "server") {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        let call_id = required_trimmed_string(item, "call_id")?;
        let tools = item
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let mut normalized = Vec::new();
        for tool in tools {
            normalized.extend(self.normalize_tool(tool, "", false, true)?);
        }
        let message = format!(
            "Tool search completed; {} selected tool definitions are now available.",
            tools.len()
        );
        let history = if execution == "client" {
            Map::from_iter([
                (
                    "type".to_owned(),
                    Value::String("function_call_output".to_owned()),
                ),
                ("call_id".to_owned(), Value::String(call_id.to_owned())),
                ("output".to_owned(), Value::String(message)),
            ])
        } else {
            self.server_search_eager = true;
            boundary_message(&message)
        };
        Ok(NormalizedToolSearchOutput {
            history,
            loaded_tools: normalized,
            visible_tools: tools.clone(),
        })
    }

    fn normalize_apply_patch_call_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let call_id = required_trimmed_string(item, "call_id")?;
        let operation = validate_apply_patch_operation(item.get("operation"))?;
        let arguments =
            serde_json::to_string(&json_object([("operation", Value::Object(operation))]))
                .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization)?;
        let alias = self.alias(ToolIdentity::new(ToolKind::ApplyPatch, "", "apply_patch"));
        Ok(Map::from_iter([
            ("type".to_owned(), Value::String("function_call".to_owned())),
            ("call_id".to_owned(), Value::String(call_id.to_owned())),
            ("name".to_owned(), Value::String(alias)),
            ("arguments".to_owned(), Value::String(arguments)),
        ]))
    }

    fn normalize_additional_tools_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<(Vec<Value>, Vec<Value>), GrokRequestEncodeError> {
        let tools = item
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let mut normalized = Vec::new();
        for raw_tool in tools {
            normalized.extend(self.normalize_tool(raw_tool, "", false, true)?);
        }
        Ok((normalized, tools.clone()))
    }
}

fn normalize_input_file_part(item: &Map<String, Value>) -> Map<String, Value> {
    let mut converted =
        Map::from_iter([("type".to_owned(), Value::String("input_file".to_owned()))]);
    for key in ["file_data", "file_id", "filename", "file_url"] {
        if let Some(value) = item.get(key)
            && !value.is_null()
        {
            converted.insert(key.to_owned(), value.clone());
        }
    }
    converted
}

fn required_trimmed_string<'a>(
    item: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, GrokRequestEncodeError> {
    item.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)
}

fn encode_function_arguments(value: Option<&Value>) -> Result<String, GrokRequestEncodeError> {
    match value {
        Some(Value::String(value)) => Ok(value.clone()),
        None | Some(Value::Null) => Ok("{}".to_owned()),
        Some(value) => serde_json::to_string(value)
            .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization),
    }
}

fn normalize_function_arguments(arguments: &str, schema: &Value) -> Option<String> {
    if arguments.trim().is_empty() || !schema.is_object() {
        return None;
    }
    let mut value = serde_json::from_str::<Value>(arguments).ok()?;
    if !normalize_argument_value(&mut value, schema, schema, 0) {
        return None;
    }
    serde_json::to_string(&value).ok()
}

fn normalize_argument_value(value: &mut Value, schema: &Value, root: &Value, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    let Some(schema) = schema.as_object() else {
        return false;
    };
    let mut changed = false;
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && let Some(resolved) = resolve_local_schema_value_ref(root, reference)
    {
        changed |= normalize_argument_value(value, resolved, root, depth + 1);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                changed |= normalize_argument_value(value, branch, root, depth + 1);
            }
        }
    }
    if schema_requires_integer(schema)
        && let Value::Number(number) = value
        && let Some(normalized) = normalize_integral_number(number)
    {
        *number = normalized;
        return true;
    }
    match value {
        Value::Object(object) => {
            let properties = schema.get("properties").and_then(Value::as_object);
            let additional = schema
                .get("additionalProperties")
                .filter(|value| value.is_object());
            for (key, item) in object {
                let property = properties
                    .and_then(|properties| properties.get(key))
                    .or(additional);
                if let Some(property) = property {
                    changed |= normalize_argument_value(item, property, root, depth + 1);
                }
            }
        }
        Value::Array(items) => {
            let prefix = schema.get("prefixItems").and_then(Value::as_array);
            let item_schema = schema.get("items").filter(|value| value.is_object());
            for (index, item) in items.iter_mut().enumerate() {
                let schema = prefix
                    .and_then(|prefix| prefix.get(index))
                    .filter(|value| value.is_object())
                    .or(item_schema);
                if let Some(schema) = schema {
                    changed |= normalize_argument_value(item, schema, root, depth + 1);
                }
            }
        }
        _ => {}
    }
    changed
}

fn schema_requires_integer(schema: &Map<String, Value>) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => kind == "integer",
        Some(Value::Array(kinds)) => {
            let mut integer = false;
            for kind in kinds.iter().filter_map(Value::as_str) {
                if kind == "number" {
                    return false;
                }
                integer |= kind == "integer";
            }
            integer
        }
        _ => false,
    }
}

fn normalize_integral_number(number: &serde_json::Number) -> Option<serde_json::Number> {
    let raw = number.to_string();
    if raw.len() > MAX_JSON_NUMBER_TEXT_BYTES || !raw.contains(['.', 'e', 'E']) {
        return None;
    }
    let (mantissa, exponent_text) = raw.find(['e', 'E']).map_or((raw.as_str(), ""), |index| {
        (&raw[..index], &raw[index + 1..])
    });
    let (negative, mantissa) = mantissa
        .strip_prefix('-')
        .map_or((false, mantissa), |value| (true, value));
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{whole}{fraction}")
        .trim_start_matches('0')
        .to_owned();
    if digits.is_empty() {
        return "0".parse().ok().filter(|_| raw != "0");
    }
    let exponent = parse_bounded_decimal_exponent(exponent_text)?;
    let decimal_shift = exponent - i64::try_from(fraction.len()).ok()?;
    if decimal_shift < 0 {
        let fractional_digits = usize::try_from(-decimal_shift).ok()?;
        if fractional_digits > digits.len()
            || !digits[digits.len() - fractional_digits..]
                .bytes()
                .all(|byte| byte == b'0')
        {
            return None;
        }
        digits.truncate(digits.len() - fractional_digits);
        let trimmed = digits.trim_start_matches('0');
        if trimmed.is_empty() {
            return "0".parse().ok();
        }
        digits = trimmed.to_owned();
    } else if decimal_shift > 0 {
        let shift = usize::try_from(decimal_shift).ok()?;
        if shift
            > MAX_EXACT_JSON_INTEGER_TEXT
                .len()
                .saturating_sub(digits.len())
        {
            return None;
        }
        digits.extend(std::iter::repeat_n('0', shift));
    }
    if digits.len() > MAX_EXACT_JSON_INTEGER_TEXT.len()
        || (digits.len() == MAX_EXACT_JSON_INTEGER_TEXT.len()
            && digits.as_str() > MAX_EXACT_JSON_INTEGER_TEXT)
    {
        return None;
    }
    let normalized = if negative {
        format!("-{digits}")
    } else {
        digits
    };
    if normalized == raw {
        None
    } else {
        normalized.parse().ok()
    }
}

fn exact_nonnegative_sequence(value: &Value) -> Option<u64> {
    let number = value.as_number()?;
    let sequence = number
        .as_u64()
        .or_else(|| normalize_integral_number(number)?.as_u64())?;
    (sequence <= MAX_EXACT_JSON_INTEGER).then_some(sequence)
}

fn parse_bounded_decimal_exponent(raw: &str) -> Option<i64> {
    if raw.is_empty() {
        return Some(0);
    }
    let (sign, digits) = if let Some(value) = raw.strip_prefix('+') {
        (1_i64, value)
    } else if let Some(value) = raw.strip_prefix('-') {
        (-1_i64, value)
    } else {
        (1_i64, raw)
    };
    if digits.is_empty() {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    if digits.len() > 9 {
        return None;
    }
    digits.parse::<i64>().ok().map(|value| sign * value)
}

fn resolve_local_schema_value_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    if reference == "#" {
        return Some(root);
    }
    let pointer = reference.strip_prefix('#')?;
    if !pointer.starts_with('/') {
        return None;
    }
    root.pointer(pointer)
}

fn schema_contains_integer(schema: &Value, depth: usize) -> bool {
    let mut visited = BTreeSet::new();
    schema_contains_reachable_integer(schema, schema, &mut visited, depth)
}

fn schema_contains_reachable_integer(
    schema: &Value,
    root: &Value,
    visited_refs: &mut BTreeSet<String>,
    depth: usize,
) -> bool {
    if depth > 64 {
        return false;
    }
    let Some(schema) = schema.as_object() else {
        return false;
    };
    if schema_requires_integer(schema) {
        return true;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && visited_refs.insert(reference.to_owned())
        && let Some(resolved) = resolve_local_schema_value_ref(root, reference)
        && schema_contains_reachable_integer(resolved, root, visited_refs, depth + 1)
    {
        return true;
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if schema
            .get(keyword)
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                branches.iter().any(|branch| {
                    schema_contains_reachable_integer(branch, root, visited_refs, depth + 1)
                })
            })
        {
            return true;
        }
    }
    for keyword in ["items", "additionalProperties"] {
        if schema.get(keyword).is_some_and(|child| {
            schema_contains_reachable_integer(child, root, visited_refs, depth + 1)
        }) {
            return true;
        }
    }
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties.values().any(|property| {
                schema_contains_reachable_integer(property, root, visited_refs, depth + 1)
            })
        })
}

fn encode_tool_output(value: Option<&Value>) -> Result<String, GrokRequestEncodeError> {
    match value {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(value) => serde_json::to_string(value)
            .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization),
    }
}

fn is_function_output_content_array(blocks: &[Value]) -> bool {
    blocks.iter().any(|raw| {
        raw.pointer("/type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.starts_with("input_"))
    })
}

fn require_content_source(
    block: &Map<String, Value>,
    fields: &[&str],
) -> Result<(), GrokRequestEncodeError> {
    let has_source = fields.iter().any(|field| {
        block
            .get(*field)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    });
    has_source
        .then_some(())
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)
}

fn sanitize_reasoning_input(item: &Map<String, Value>) -> Map<String, Value> {
    let mut converted =
        copy_non_null_history_fields(item, &["id", "summary", "content", "encrypted_content"]);
    // Grok CLI 会在 summary/content 条目上注入 phase 等内部键。
    strip_grok_internal_entry_keys(&mut converted, &["summary", "content"]);
    converted.insert("type".to_owned(), Value::String("reasoning".to_owned()));
    if has_portable_reasoning_content(&converted) {
        converted
    } else {
        boundary_message(
            "A prior model reasoning item was omitted because it has no portable content for Grok Build.",
        )
    }
}

fn normalize_compaction_input(
    item: &Map<String, Value>,
) -> Result<Vec<Value>, GrokRequestEncodeError> {
    let encrypted = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let has_structured_compaction_shape = item.contains_key("summary")
        || item.contains_key("id")
        || item.contains_key("status")
        || string_field(item, "type").trim() == "compaction_summary";

    // 旧版 adapter 曾将明文摘要直接写入 encrypted_content，且不带
    // id/status/summary。仅对这一可识别的历史形态保留明文恢复；新形态严格回放为
    // xAI reasoning 密文 + 可见 conversation summary。
    if !has_structured_compaction_shape {
        let Some(summary) = encrypted else {
            return Ok(Vec::new());
        };
        let continuation = format!(
            "This session is being continued from a previous conversation that ran out of context. \
             The summary below covers the earlier portion of the conversation.\n\n{summary}"
        );
        return Ok(vec![Value::Object(compaction_summary_message(
            &continuation,
        ))]);
    }

    let mut converted = Vec::with_capacity(2);
    if let Some(encrypted) = encrypted {
        converted.push(json!({
            "type": "reasoning",
            "summary": [],
            "encrypted_content": encrypted,
        }));
    }
    if let Some(summary) = compact_summary_text(item.get("summary")) {
        converted.push(Value::Object(compaction_summary_message(&format!(
            "<conversation_summary>\n{summary}\n</conversation_summary>"
        ))));
    }
    Ok(converted)
}

fn compact_summary_text(value: Option<&Value>) -> Option<String> {
    let text = value?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn compaction_summary_message(text: &str) -> Map<String, Value> {
    Map::from_iter([
        ("type".to_owned(), Value::String("message".to_owned())),
        ("role".to_owned(), Value::String("user".to_owned())),
        (
            "content".to_owned(),
            Value::Array(vec![json_object([
                ("type", Value::String("input_text".to_owned())),
                ("text", Value::String(text.to_owned())),
            ])]),
        ),
    ])
}

pub(super) fn strip_invalid_encrypted_reasoning_from_body(body: &mut Map<String, Value>) -> bool {
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return false;
    };
    let has_encrypted_reasoning = input.iter().any(|item| {
        item.as_object().is_some_and(|item| {
            string_field(item, "type").trim() == "reasoning"
                && item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty())
        })
    });
    if !has_encrypted_reasoning {
        return false;
    }

    input.retain_mut(|item| {
        let Some(item) = item.as_object_mut() else {
            return true;
        };
        if string_field(item, "type").trim() != "reasoning" {
            return true;
        }
        item.remove("encrypted_content");
        if item.get("content").is_some_and(Value::is_null) {
            item.remove("content");
        }
        item.len() > 1
    });
    true
}

fn has_portable_reasoning_content(item: &Map<String, Value>) -> bool {
    item.get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        || ["summary", "content"].into_iter().any(|field| {
            item.get(field)
                .and_then(Value::as_array)
                .is_some_and(|values| !values.is_empty())
        })
}

fn sanitize_native_history_input(item: &Map<String, Value>, item_type: &str) -> Map<String, Value> {
    let fields: &[&str] = match item_type {
        "file_search_call" => &["id", "queries", "status", "results"],
        "web_search_call" => &["action", "id", "status"],
        "image_generation_call" => &["id", "result", "status"],
        "code_interpreter_call" => &["code", "container_id", "id", "outputs", "status"],
        "shell_call" => &["id", "call_id", "action", "status", "environment"],
        "mcp_list_tools" => &["id", "server_label", "tools", "error"],
        "mcp_approval_request" => &["arguments", "id", "name", "server_label"],
        "mcp_approval_response" => &["approval_request_id", "approve", "id", "reason"],
        "mcp_call" => &[
            "arguments",
            "id",
            "name",
            "server_label",
            "approval_request_id",
            "error",
            "output",
            "status",
        ],
        _ => &[],
    };
    let mut converted = copy_non_null_history_fields(item, fields);
    // Grok CLI 会在 shell_call 的 action object 里注入内部键与 null 占位
    // 字段（如 `timeout_ms: null`）；只在这一层剥离，深层内容原样保留。
    if item_type == "shell_call"
        && let Some(Value::Object(action)) = converted.get_mut("action")
    {
        strip_grok_internal_keys(action);
        action.retain(|_, value| !value.is_null());
    }
    converted.insert("type".to_owned(), Value::String(item_type.to_owned()));
    converted
}

fn copy_non_null_history_fields(item: &Map<String, Value>, fields: &[&str]) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|field| {
            item.get(*field)
                .filter(|value| !value.is_null())
                .map(|value| ((*field).to_owned(), value.clone()))
        })
        .collect()
}

fn strip_grok_internal_keys(object: &mut Map<String, Value>) {
    for key in GROK_INTERNAL_HISTORY_KEYS {
        object.remove(*key);
    }
}

fn strip_grok_internal_entry_keys(converted: &mut Map<String, Value>, fields: &[&str]) {
    for field in fields {
        let Some(Value::Array(entries)) = converted.get_mut(*field) else {
            continue;
        };
        for entry in entries {
            if let Some(object) = entry.as_object_mut() {
                strip_grok_internal_keys(object);
            }
        }
    }
}

fn unsupported_input_history_boundary(item: &Map<String, Value>, kind: &str) -> Value {
    let mut lines = vec![
        "A prior Responses history item was omitted because Grok Build cannot deserialize this Codex item type."
            .to_owned(),
        format!("Type: {kind}"),
    ];
    for key in ["id", "call_id", "name", "status"] {
        let value = string_field(item, key).trim();
        if !value.is_empty() {
            lines.push(format!("{}: {value}", key.replace('_', " ")));
        }
    }
    Value::Object(boundary_message(&lines.join("\n")))
}

fn boundary_message(text: &str) -> Map<String, Value> {
    Map::from_iter([
        ("type".to_owned(), Value::String("message".to_owned())),
        ("role".to_owned(), Value::String("developer".to_owned())),
        (
            "content".to_owned(),
            Value::Array(vec![json_object([
                ("type", Value::String("input_text".to_owned())),
                ("text", Value::String(text.to_owned())),
            ])]),
        ),
    ])
}

fn normalize_agent_message_input(item: &Map<String, Value>) -> Value {
    let Some(content) = item.get("content").and_then(text_input_content) else {
        return Value::Object(boundary_message(
            "An encrypted inter-agent message occurred here but is not portable to the Grok Build account.",
        ));
    };
    let author = non_empty_or(string_field(item, "author"), "agent");
    let recipient = non_empty_or(string_field(item, "recipient"), "recipient");
    Value::Object(boundary_message(&format!(
        "Agent message ({author} -> {recipient}):\n{content}"
    )))
}

fn text_input_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                let item = item.as_object()?;
                matches!(
                    string_field(item, "type"),
                    "input_text" | "output_text" | "text"
                )
                .then(|| string_field(item, "text").to_owned())
            })
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join("\n")),
        _ => None,
    }
}

fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    let value = value.trim();
    if value.is_empty() { fallback } else { value }
}

fn normalize_mcp_output_input(item: &Map<String, Value>) -> Result<Value, GrokRequestEncodeError> {
    let output = serde_json::to_string(item.get("output").unwrap_or(&Value::Null))
        .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization)?;
    let call_id = non_empty_or(string_field(item, "call_id"), "unknown");
    Ok(Value::Object(boundary_message(&format!(
        "MCP tool output for call {call_id}: {output}"
    ))))
}

fn validate_apply_patch_operation(
    value: Option<&Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let operation = value
        .and_then(Value::as_object)
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    let kind = required_trimmed_string(operation, "type")?;
    required_trimmed_string(operation, "path")?;
    match kind {
        "create_file" | "update_file" if operation.get("diff").is_some_and(Value::is_string) => {}
        "delete_file" => {}
        _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
    }
    Ok(operation.clone())
}

fn normalize_apply_patch_output_input(
    item: &Map<String, Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let call_id = required_trimmed_string(item, "call_id")?;
    let status = match string_field(item, "status").trim() {
        "" | "completed" => "completed",
        "failed" => "failed",
        _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
    };
    let output = encode_tool_output(item.get("output"))?;
    let mut message = format!("Apply patch status: {status}");
    if !output.is_empty() {
        message.push('\n');
        message.push_str(&output);
    }
    Ok(Map::from_iter([
        (
            "type".to_owned(),
            Value::String("function_call_output".to_owned()),
        ),
        ("call_id".to_owned(), Value::String(call_id.to_owned())),
        ("output".to_owned(), Value::String(message)),
    ]))
}

fn normalize_legacy_local_shell_call_input(
    item: &Map<String, Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let call_id = required_trimmed_string(item, "call_id")?;
    let action = legacy_shell_action(item.get("action"))?;
    let mut converted = Map::from_iter([
        ("type".to_owned(), Value::String("shell_call".to_owned())),
        ("call_id".to_owned(), Value::String(call_id.to_owned())),
        ("action".to_owned(), Value::Object(action)),
    ]);
    for key in ["id", "status", "timeout_ms", "max_output_length"] {
        if let Some(value) = item.get(key) {
            converted.insert(key.to_owned(), value.clone());
        }
    }
    Ok(converted)
}

fn legacy_shell_action(
    value: Option<&Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let action = value
        .and_then(Value::as_object)
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    if !matches!(string_field(action, "type").trim(), "" | "exec") {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    }
    let command = legacy_shell_command(action)?;
    Ok(Map::from_iter([
        ("type".to_owned(), Value::String("exec".to_owned())),
        (
            "commands".to_owned(),
            Value::Array(vec![Value::String(command)]),
        ),
    ]))
}

fn legacy_shell_command(action: &Map<String, Value>) -> Result<String, GrokRequestEncodeError> {
    let mut command = match action.get("command") {
        Some(Value::String(value)) => value.trim().to_owned(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                part.as_str()
                    .map(quote_shell_argument)
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(" "),
        _ => action
            .get("commands")
            .and_then(Value::as_array)
            .map(|commands| {
                commands
                    .iter()
                    .map(|command| {
                        command
                            .as_str()
                            .map(ToOwned::to_owned)
                            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|commands| commands.join("\n"))
            })
            .transpose()?
            .unwrap_or_default(),
    };
    if command.is_empty() {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    }
    if let Some(environment) = action.get("env").and_then(Value::as_object)
        && !environment.is_empty()
    {
        let assignments = environment
            .iter()
            .map(|(name, value)| {
                if !valid_environment_name(name) {
                    return Err(GrokRequestEncodeError::InvalidRequestNormalization);
                }
                let value = value
                    .as_str()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                Ok(format!("{name}={}", quote_shell_argument(value)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        command = format!("env {} {command}", assignments.join(" "));
    }
    if let Some(directory) = action
        .get("working_directory")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|directory| !directory.is_empty())
    {
        command = format!("cd {} && {command}", quote_shell_argument(directory));
    }
    Ok(command)
}

fn normalize_legacy_local_shell_output_input(
    item: &Map<String, Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let call_id = required_trimmed_string(item, "call_id")?;
    let output = match item.get("output") {
        Some(Value::Array(output)) => output.clone(),
        Some(Value::String(output)) => {
            let exit_code = item
                .get("exit_code")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| {
                    i64::from(string_field(item, "status").eq_ignore_ascii_case("failed"))
                });
            vec![shell_output_block(output, "", "exit", Some(exit_code))]
        }
        _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
    };
    let mut converted = Map::from_iter([
        (
            "type".to_owned(),
            Value::String("shell_call_output".to_owned()),
        ),
        ("call_id".to_owned(), Value::String(call_id.to_owned())),
        ("output".to_owned(), Value::Array(output)),
    ]);
    if let Some(value) = item.get("max_output_length")
        && !value.is_null()
    {
        converted.insert("max_output_length".to_owned(), value.clone());
    }
    Ok(converted)
}

fn normalize_shell_call_output_input(
    item: &Map<String, Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let call_id = required_trimmed_string(item, "call_id")?;
    let output = normalize_shell_output_blocks(item.get("output"), item.get("status"))?;
    let mut converted = Map::from_iter([
        (
            "type".to_owned(),
            Value::String("shell_call_output".to_owned()),
        ),
        ("call_id".to_owned(), Value::String(call_id.to_owned())),
        ("output".to_owned(), Value::Array(output)),
    ]);
    if let Some(value) = item.get("max_output_length")
        && !value.is_null()
    {
        converted.insert("max_output_length".to_owned(), value.clone());
    }
    Ok(converted)
}

fn normalize_shell_output_blocks(
    value: Option<&Value>,
    status: Option<&Value>,
) -> Result<Vec<Value>, GrokRequestEncodeError> {
    match value {
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|raw| {
                let block = raw
                    .as_object()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                let stdout = string_field(block, "stdout");
                let stderr = string_field(block, "stderr");
                let outcome = block
                    .get("outcome")
                    .and_then(Value::as_object)
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                match string_field(outcome, "type").trim() {
                    "exit" => {
                        let exit_code = outcome
                            .get("exit_code")
                            .or_else(|| outcome.get("exitCode"))
                            .and_then(Value::as_i64)
                            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                        Ok(shell_output_block(stdout, stderr, "exit", Some(exit_code)))
                    }
                    "timeout" => Ok(shell_output_block(stdout, stderr, "timeout", None)),
                    _ => Err(GrokRequestEncodeError::InvalidRequestNormalization),
                }
            })
            .collect(),
        Some(Value::String(output)) => {
            let failed = status
                .and_then(Value::as_str)
                .is_some_and(|status| status.eq_ignore_ascii_case("failed"));
            Ok(vec![shell_output_block(
                output,
                "",
                "exit",
                Some(i64::from(failed)),
            )])
        }
        _ => Err(GrokRequestEncodeError::InvalidRequestNormalization),
    }
}

fn shell_output_block(
    stdout: &str,
    stderr: &str,
    outcome_type: &str,
    exit_code: Option<i64>,
) -> Value {
    let mut outcome = Map::from_iter([("type".to_owned(), Value::String(outcome_type.to_owned()))]);
    if let Some(exit_code) = exit_code {
        outcome.insert("exit_code".to_owned(), Value::from(exit_code));
    }
    json_object([
        ("stdout", Value::String(stdout.to_owned())),
        ("stderr", Value::String(stderr.to_owned())),
        ("outcome", Value::Object(outcome)),
    ])
}

fn quote_shell_argument(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_@%+=:,./-".contains(character))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn valid_environment_name(value: &str) -> bool {
    value.chars().enumerate().all(|(index, character)| {
        character.is_ascii_alphabetic()
            || character == '_'
            || (index > 0 && character.is_ascii_digit())
    }) && !value.is_empty()
}
