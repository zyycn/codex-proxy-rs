use std::collections::BTreeMap;
use std::fmt;

use gateway_core::operation::GenerateRequest;
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use super::{GrokSessionAffinityKey, XAI_PROVIDER_NAME};

const GROK_OPTION_FIELDS: &[&str] = &["schema_version", "transport", "turn_index"];
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
const FOREIGN_CLIENT_METADATA_FIELDS: &[&str] = &["x-openai-subagent"];
const MAX_SESSION_SEED_BYTES: usize = 1_024;

/// 保留客户端 OpenAI Responses object 的 xAI 上游请求。
pub struct GrokResponsesRequest {
    body: Map<String, Value>,
    session_id: Option<String>,
    affinity: Option<GrokSessionAffinityKey>,
    turn_index: Option<String>,
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

    /// 返回与会话一致、额外绑定模型的账号亲和键。
    #[must_use]
    pub const fn affinity(&self) -> Option<&GrokSessionAffinityKey> {
        self.affinity.as_ref()
    }

    /// 返回客户端真实提供且仅在稳定会话内有效的非负轮次。
    #[must_use]
    pub fn turn_index(&self) -> Option<&str> {
        self.turn_index.as_deref()
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

    pub(crate) fn set_input(&mut self, input: Vec<Value>) {
        self.body.insert("input".to_owned(), Value::Array(input));
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
        self.affinity = None;
        self.body.insert(
            "prompt_cache_key".to_owned(),
            Value::String(session_id.to_owned()),
        );
    }

    pub(crate) fn clear_session(&mut self) {
        self.body.remove("prompt_cache_key");
        self.session_id = None;
        self.affinity = None;
        self.turn_index = None;
    }

    pub fn strip_reasoning_encrypted_content(&mut self) -> bool {
        let Some(items) = self.body.get_mut("input").and_then(Value::as_array_mut) else {
            return false;
        };
        let original_len = items.len();
        let mut changed = false;
        items.retain_mut(|item| {
            let Some(item) = item.as_object_mut() else {
                return true;
            };
            if string_field(item, "type") != "reasoning" {
                return true;
            }
            for field in ["encrypted_content", "id", "status"] {
                changed |= item.remove(field).is_some();
            }
            let portable = ["summary", "content"].into_iter().any(|field| {
                item.get(field)
                    .and_then(Value::as_array)
                    .is_some_and(|values| !values.is_empty())
            });
            changed |= !portable;
            portable
        });
        changed || items.len() != original_len
    }

    pub fn encode(
        request: &GenerateRequest,
        upstream_model: &str,
        client_api_key_ref: &ClientApiKeyId,
    ) -> Result<Self, GrokRequestEncodeError> {
        let payload = request
            .protocol_payload()
            .filter(|payload| payload.protocol() == "openai")
            .ok_or(GrokRequestEncodeError::InvalidProtocolPayload)?;
        let mut body = payload.body().clone();
        let session_seed = explicit_session_seed(request, &body);
        let identity = resolve_session_identity(
            client_api_key_ref.as_str(),
            upstream_model,
            session_seed.as_deref(),
            &body,
        );
        sanitize_account_identity(&mut body);
        sanitize_foreign_client_metadata(&mut body);
        let response_transform = normalize_responses_request(&mut body)?;
        let (session_id, affinity) = identity.map_or((None, None), |(session_id, affinity)| {
            (Some(session_id), Some(affinity))
        });
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
        body.insert("model".to_owned(), Value::String(upstream_model.to_owned()));
        body.insert("stream".to_owned(), Value::Bool(true));
        let turn_index = request
            .provider_options()
            .get(XAI_PROVIDER_NAME)
            .map(validate_provider_options)
            .transpose()?
            .flatten()
            .filter(|_| session_id.is_some());
        Ok(Self {
            body,
            session_id,
            affinity,
            turn_index,
            response_transform,
        })
    }

    pub(crate) fn to_json_bytes(&self) -> Result<Vec<u8>, GrokRequestEncodeError> {
        serde_json::to_vec(&self.body).map_err(|_| GrokRequestEncodeError::Serialization)
    }
}

impl fmt::Debug for GrokResponsesRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokResponsesRequest")
            .field("body_keys", &self.body.keys().collect::<Vec<_>>())
            .field("has_session", &self.session_id.is_some())
            .field("has_turn_index", &self.turn_index.is_some())
            .field(
                "has_response_transform",
                &!self.response_transform.is_empty(),
            )
            .field("body", &"<prompt and tool payload redacted>")
            .finish()
    }
}

/// Generate-to-Responses encoding error that never retains option or prompt
/// values.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokRequestEncodeError {
    /// 数据面只接受 OpenAI adapter 保留的原始 Responses object。
    #[error("Grok Build request is missing its OpenAI protocol payload")]
    InvalidProtocolPayload,
    /// Grok provider option schema or value is malformed.
    #[error("Grok Build provider option schema is invalid")]
    InvalidProviderOptions,
    /// A Grok-specific option is unknown or unsupported.
    #[error("Grok Build provider option is unsupported")]
    UnsupportedProviderOption,
    /// JSON serialization unexpectedly failed.
    #[error("Grok Build request serialization failed")]
    Serialization,
    /// Responses compatibility fields could not be normalized safely.
    #[error("Grok Build request normalization failed")]
    InvalidRequestNormalization,
}

fn validate_provider_options(
    options: &Map<String, Value>,
) -> Result<Option<String>, GrokRequestEncodeError> {
    if options
        .keys()
        .any(|field| !GROK_OPTION_FIELDS.contains(&field.as_str()))
    {
        return Err(GrokRequestEncodeError::UnsupportedProviderOption);
    }
    if options.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(GrokRequestEncodeError::InvalidProviderOptions);
    }
    if let Some(transport) = options.get("transport")
        && transport.as_str() != Some("http_sse")
    {
        return Err(GrokRequestEncodeError::InvalidProviderOptions);
    }
    let turn_index = options
        .get("turn_index")
        .map(|value| {
            value
                .as_str()
                .and_then(normalize_turn_index)
                .map(ToOwned::to_owned)
                .ok_or(GrokRequestEncodeError::InvalidProviderOptions)
        })
        .transpose()?;
    Ok(turn_index)
}

fn normalize_turn_index(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok())
    .then_some(value)
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

fn sanitize_foreign_client_metadata(body: &mut Map<String, Value>) {
    let Some(Value::Object(metadata)) = body.get_mut("client_metadata") else {
        return;
    };
    for field in FOREIGN_CLIENT_METADATA_FIELDS {
        metadata.remove(*field);
    }
    if metadata.is_empty() {
        body.remove("client_metadata");
    }
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
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        if text.get("format").is_none_or(Value::is_null) {
            text.insert(
                "format".to_owned(),
                normalize_response_format(response_format)?,
            );
        }
    }
    patch_reasoning_text_types(body);
    ToolNormalizer::new().normalize(body)
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
}

#[derive(Debug, Clone)]
struct StreamCallState {
    identity: ToolIdentity,
    arguments: String,
    last_delta: Option<Map<String, Value>>,
    added_payload: Option<Map<String, Value>>,
}

#[derive(Clone, Default)]
pub(crate) struct GrokResponseTransform {
    aliases: BTreeMap<String, ToolIdentity>,
    visible_tools: Vec<Value>,
    legacy_local_shell: bool,
    stream_calls: BTreeMap<String, StreamCallState>,
    stream_keys: BTreeMap<String, String>,
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
        self.aliases.is_empty() && self.visible_tools.is_empty() && !self.legacy_local_shell
    }

    pub(crate) fn rewrite_stream_event(
        &mut self,
        event_type: &str,
        mut value: Value,
    ) -> Result<Vec<GrokTransformedWireEvent>, GrokRequestEncodeError> {
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
            let state = self
                .stream_calls
                .get_mut(&primary)
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            if matches!(
                state.identity.kind,
                ToolKind::ToolSearch | ToolKind::Custom | ToolKind::ApplyPatch
            ) {
                state.arguments.push_str(
                    value
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
                if state.identity.kind == ToolKind::Custom {
                    state.last_delta = value.as_object().cloned();
                }
                return Ok(Vec::new());
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
                    let input = decode_custom_tool_input(arguments);
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

        self.rewrite_response_value(&mut value)?;
        if let Some(response) = value.get_mut("response").and_then(Value::as_object_mut) {
            self.restore_visible_tools(response);
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
                arguments: String::new(),
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
                arguments: String::new(),
                last_delta: None,
                added_payload: None,
            },
        );
        self.stream_keys.insert(primary.clone(), primary.clone());
        Some(primary)
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
                let input = decode_custom_tool_input(string_field(call, "arguments"));
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
        let primary = ["id", "call_id"]
            .into_iter()
            .find_map(|field| original_item.get(field).and_then(Value::as_str))
            .and_then(|key| self.stream_keys.get(key))
            .cloned();
        let mut added = primary
            .as_ref()
            .and_then(|key| self.stream_calls.get(key))
            .and_then(|state| state.added_payload.clone())
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

    fn restore_visible_tools(&self, response: &mut Map<String, Value>) {
        if response.contains_key("tools") {
            response.insert("tools".to_owned(), Value::Array(self.visible_tools.clone()));
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

fn decode_custom_tool_input(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get("input")
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
            .field("visible_tool_count", &self.visible_tools.len())
            .field("legacy_local_shell", &self.legacy_local_shell)
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
    web_search_disabled: bool,
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
            web_search_disabled: false,
        }
    }

    fn normalize(
        mut self,
        payload: &mut Map<String, Value>,
    ) -> Result<GrokResponseTransform, GrokRequestEncodeError> {
        let (tools, had_tools) = optional_array(payload.get("tools"))?;
        if had_tools {
            self.response.visible_tools.clone_from(&tools);
        }
        let client_search = inspect_tool_search(&tools)?;
        self.normalize_client_search_parallel(payload, client_search)?;

        let mut normalized_tools = Vec::with_capacity(tools.len());
        for raw_tool in &tools {
            normalized_tools.extend(self.normalize_tool(raw_tool, "", client_search, false)?);
        }

        if let Some(Value::Array(items)) = payload.get("input") {
            let normalized = self.normalize_input_items(items)?;
            normalized_tools.extend(normalized.loaded_tools);
            self.response.visible_tools.extend(normalized.visible_tools);
            payload.insert("input".to_owned(), Value::Array(normalized.items));
        } else if payload
            .get("input")
            .is_some_and(|input| !input.is_null() && !input.is_string())
        {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }

        if self.client_search_tool.is_some() {
            normalized_tools.push(Value::Object(self.build_client_search_function()?));
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
        self.normalize_tool_choice(payload, &normalized_tools)?;
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
            "x_search" | "image_generation" | "collections_search" | "file_search"
            | "code_execution" | "code_interpreter" => {
                Ok(vec![Value::Object(without_defer_loading(tool))])
            }
            "" | "computer_use_preview" => Err(GrokRequestEncodeError::InvalidRequestNormalization),
            _ => Err(GrokRequestEncodeError::InvalidRequestNormalization),
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
        let alias = self.alias(ToolIdentity::new(ToolKind::Function, namespace, name));
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
        let mut description = string_field(tool, "description").trim().to_owned();
        if !description.is_empty() {
            description.push('\n');
        }
        description.push_str("Provide the custom tool input in the input string field.");
        let alias = self.alias(ToolIdentity::new(ToolKind::Custom, namespace, name));
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
                            "input",
                            json_object([("type", Value::String("string".to_owned()))]),
                        )]),
                    ),
                    (
                        "required",
                        Value::Array(vec![Value::String("input".to_owned())]),
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
    let mut positions = BTreeMap::new();
    for tool in tools {
        let key = tool.as_object().map(|tool| {
            let kind = string_field(tool, "type");
            let name = ["name", "server_label"]
                .into_iter()
                .find_map(|field| tool.get(field).and_then(Value::as_str))
                .unwrap_or_default();
            format!("{kind}\0{name}")
        });
        if let Some(position) = key.as_ref().and_then(|key| positions.get(key)).copied() {
            result[position] = tool;
        } else {
            if let Some(key) = key {
                positions.insert(key, result.len());
            }
            result.push(tool);
        }
    }
    result
}

impl ToolNormalizer {
    fn normalize_web_search_tool(
        &mut self,
        tool: &Map<String, Value>,
        kind: &str,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        if let Some(external) = tool.get("external_web_access") {
            let enabled = external
                .as_bool()
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            if !enabled {
                self.web_search_disabled = true;
                return Ok(Vec::new());
            }
        }
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
        if self.web_search_disabled
            && normalize_hosted_tool_choice_kind(&kind) == Some("web_search")
            && !has_tool_type(normalized_tools, "web_search")
        {
            payload.insert("tool_choice".to_owned(), Value::String("none".to_owned()));
            return Ok(());
        }
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
            "function" => self.normalize_function_tool_choice(payload, object)?,
            _ => {
                if let Some(hosted_kind) = normalize_hosted_tool_choice_kind(&kind) {
                    let matching = tools_of_type(normalized_tools, hosted_kind);
                    if matching.is_empty() {
                        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
                    }
                    if matching.len() != normalized_tools.len() {
                        payload.insert("tools".to_owned(), Value::Array(matching));
                    }
                    payload.insert(
                        "tool_choice".to_owned(),
                        Value::String("required".to_owned()),
                    );
                } else {
                    return Err(GrokRequestEncodeError::InvalidRequestNormalization);
                }
            }
        }
        Ok(())
    }

    fn normalize_function_tool_choice(
        &self,
        payload: &mut Map<String, Value>,
        mut object: Map<String, Value>,
    ) -> Result<(), GrokRequestEncodeError> {
        if let Some(Value::Object(function)) = object.get_mut("function") {
            rewrite_namespace_choice(function, &self.identity_aliases)?;
            payload.insert("tool_choice".to_owned(), Value::Object(object));
            return Ok(());
        }
        rewrite_namespace_choice(&mut object, &self.identity_aliases)?;
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
                | "mcp_call"
                | "compaction" => {
                    rewritten.push(Value::Object(sanitize_native_history_input(
                        item, item_type,
                    )));
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
                    let (marker, tools, visible) = self.normalize_additional_tools_input(item)?;
                    rewritten.push(marker);
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
        Ok(Map::from_iter([
            ("type".to_owned(), Value::String("message".to_owned())),
            ("role".to_owned(), Value::String(role.to_owned())),
            ("content".to_owned(), content),
        ]))
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
        Ok(Map::from_iter([
            ("type".to_owned(), Value::String("function_call".to_owned())),
            ("call_id".to_owned(), Value::String(call_id.to_owned())),
            ("name".to_owned(), Value::String(name)),
            ("arguments".to_owned(), Value::String(arguments)),
        ]))
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
        let arguments =
            serde_json::to_string(&json_object([("input", Value::String(input.to_owned()))]))
                .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization)?;
        let call_id = required_trimmed_string(item, "call_id")?;
        let namespace = string_field(item, "namespace").trim();
        let alias = self.alias(ToolIdentity::new(ToolKind::Custom, namespace, name));
        Ok(Map::from_iter([
            ("type".to_owned(), Value::String("function_call".to_owned())),
            ("call_id".to_owned(), Value::String(call_id.to_owned())),
            ("name".to_owned(), Value::String(alias)),
            ("arguments".to_owned(), Value::String(arguments)),
        ]))
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
        Ok(Map::from_iter([
            (
                "type".to_owned(),
                Value::String("function_call_output".to_owned()),
            ),
            ("call_id".to_owned(), Value::String(call_id.to_owned())),
            ("output".to_owned(), output),
        ]))
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
    ) -> Result<(Value, Vec<Value>, Vec<Value>), GrokRequestEncodeError> {
        let tools = item
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let mut normalized = Vec::new();
        let mut names = Vec::new();
        for raw_tool in tools {
            normalized.extend(self.normalize_tool(raw_tool, "", false, true)?);
            if let Some(tool) = raw_tool.as_object()
                && let Some(name) = ["name", "server_label", "type"]
                    .into_iter()
                    .find_map(|field| tool.get(field).and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            {
                names.push(name.to_owned());
            }
        }
        let mut message =
            "Additional tools become available at this point in the conversation.".to_owned();
        if !names.is_empty() {
            message.push_str("\nTools: ");
            message.push_str(&names.join(", "));
        }
        Ok((
            Value::Object(boundary_message(&message)),
            normalized,
            tools.clone(),
        ))
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
    converted.insert("type".to_owned(), Value::String("reasoning".to_owned()));
    if converted
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        && !converted.contains_key("summary")
    {
        converted.insert("summary".to_owned(), Value::Array(Vec::new()));
    }
    if has_portable_reasoning_content(&converted) {
        converted
    } else {
        boundary_message(
            "A prior model reasoning item was omitted because it has no portable content for Grok Build.",
        )
    }
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
        "compaction" => &["id", "encrypted_content"],
        _ => &[],
    };
    let mut converted = copy_non_null_history_fields(item, fields);
    converted.insert("type".to_owned(), Value::String(item_type.to_owned()));
    converted
}

fn copy_non_null_history_fields(item: &Map<String, Value>, fields: &[&str]) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|field| {
            item.get(*field)
                .and_then(sanitize_history_json_value)
                .map(|value| ((*field).to_owned(), value))
        })
        .collect()
}

fn sanitize_history_json_value(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Object(object) => Some(Value::Object(
            object
                .iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "phase" | "internal_chat_message_metadata_passthrough"
                    )
                })
                .filter_map(|(key, value)| {
                    sanitize_history_json_value(value).map(|value| (key.clone(), value))
                })
                .collect(),
        )),
        Value::Array(values) => Some(Value::Array(
            values
                .iter()
                .filter_map(sanitize_history_json_value)
                .collect(),
        )),
        value => Some(value.clone()),
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
