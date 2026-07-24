//! 核心 Generate operation 到 Codex Responses wire request 的严格编码。

use base64::Engine as _;
use gateway_core::operation::{
    ContentPart, GenerateRequest, ImageSource, MessageRole, OutputFormat, ReasoningEffort,
    ReasoningSummary, ResponsePersistence,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::transport::protocol::responses::CodexResponsesRequest;

const CODEX_OPTION_FIELDS: &[&str] = &[
    "beta_features",
    "client_request_id",
    "codex_window_id",
    "conversation_id",
    "include_timing_metrics",
    "memgen_request",
    "parent_thread_id",
    "prompt_cache_key",
    "responses_lite",
    "schema_version",
    "service_tier",
    "session_id",
    "thread_id",
    "transport",
    "turn_id",
    "turn_metadata",
    "turn_state",
    "version",
];

const CROSS_ACCOUNT_IDENTITY_KEYS: &[&str] = &[
    "authorization",
    "Authorization",
    "cookie",
    "Cookie",
    "chatgpt-account-id",
    "chatgpt_account_id",
    "chatgptAccountId",
    "account_id",
    "accountId",
    "user_id",
    "userId",
    "chatgpt_user_id",
    "chatgptUserId",
    "access_token",
    "accessToken",
    "session_token",
    "sessionToken",
    "refresh_token",
    "refreshToken",
    "id_token",
    "idToken",
    "token",
    "cookies",
    "cookie_header",
    "cookieHeader",
    "cf_clearance",
];

const ACCOUNT_BOUND_STATE_KEYS: &[&str] = &[
    "turnState",
    "turn_state",
    "x-codex-turn-state",
    "turnMetadata",
    "turn_metadata",
    "x-codex-turn-metadata",
    "previous_response_id",
    "previousResponseId",
    "response_id",
    "responseId",
    "conversation",
    "conversation_id",
    "conversationId",
];

const UNTRUSTED_CONTINUATION_KEYS: &[&str] = &[
    "previous_response_id",
    "previousResponseId",
    "response_id",
    "responseId",
];

const INSTALLATION_ID_KEYS: &[&str] = &[
    "installation_id",
    "installationId",
    "x-codex-installation-id",
];

/// Provider 专属编码错误；不保存 prompt、schema 或 option 值。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodexRequestEncodeError {
    #[error("Codex provider option schema is invalid")]
    InvalidProviderOptions,
    #[error("Codex provider option is unsupported")]
    UnsupportedProviderOption,
    #[error("Codex request contains unsupported content")]
    UnsupportedContent,
}

pub fn encode_generate_request(
    request: &GenerateRequest,
    upstream_model: &str,
) -> Result<CodexResponsesRequest, CodexRequestEncodeError> {
    let (mut body, protocol_payload) = match request.protocol_payload() {
        Some(payload) if payload.protocol() == "openai" => (payload.body().clone(), true),
        Some(_) => return Err(CodexRequestEncodeError::InvalidProviderOptions),
        None => (Map::new(), false),
    };
    body.insert("model".to_owned(), Value::String(upstream_model.to_owned()));
    if !protocol_payload {
        body.insert("input".to_owned(), Value::Array(encode_messages(request)?));
    }
    if !protocol_payload {
        body.insert("stream".to_owned(), Value::Bool(true));
        body.insert(
            "store".to_owned(),
            Value::Bool(matches!(
                request.response_persistence(),
                ResponsePersistence::Store
            )),
        );
    }

    if !protocol_payload && !request.tools().is_empty() {
        body.insert(
            "tools".to_owned(),
            Value::Array(
                request
                    .tools()
                    .iter()
                    .map(|tool| {
                        let mut value = Map::new();
                        value.insert("type".to_owned(), Value::String("function".to_owned()));
                        value.insert("name".to_owned(), Value::String(tool.name().to_owned()));
                        if let Some(description) = tool.description() {
                            value.insert(
                                "description".to_owned(),
                                Value::String(description.to_owned()),
                            );
                        }
                        value.insert(
                            "parameters".to_owned(),
                            Value::Object(tool.input_schema().clone()),
                        );
                        value.insert("strict".to_owned(), Value::Bool(tool.strict()));
                        Value::Object(value)
                    })
                    .collect(),
            ),
        );
    }

    if !protocol_payload && !matches!(request.output_format(), OutputFormat::Text) {
        body.insert(
            "text".to_owned(),
            encode_output_format(request.output_format()),
        );
    }
    if !protocol_payload && let Some(reasoning) = request.reasoning() {
        let mut value = Map::new();
        if let Some(effort) = reasoning.effort {
            value.insert(
                "effort".to_owned(),
                Value::String(reasoning_effort(effort).to_owned()),
            );
        }
        if let Some(summary) = reasoning.summary {
            value.insert(
                "summary".to_owned(),
                Value::String(reasoning_summary(summary).to_owned()),
            );
        }
        body.insert("reasoning".to_owned(), Value::Object(value));
    }
    if !protocol_payload && let Some(tokens) = request.max_output_tokens() {
        body.insert("max_output_tokens".to_owned(), Value::from(tokens));
    }
    if !protocol_payload && let Some(key) = request.prompt_cache_key() {
        body.insert("prompt_cache_key".to_owned(), Value::String(key.to_owned()));
    }

    let mut encoded = CodexResponsesRequest::from_body(body);
    encoded.explicit_prompt_cache_key = encoded.prompt_cache_key().is_some();
    extract_request_context(&mut encoded);
    if let Some(options) = request.provider_options().get("openai") {
        apply_codex_options(&mut encoded, options)?;
    }
    Ok(encoded)
}

fn encode_messages(request: &GenerateRequest) -> Result<Vec<Value>, CodexRequestEncodeError> {
    let mut input = Vec::new();
    for message in request.messages() {
        let mut content = Vec::new();
        for part in message.content() {
            match part {
                ContentPart::Text(text) => content.push(json!({
                    "type": match message.role() {
                        MessageRole::Assistant => "output_text",
                        MessageRole::System
                        | MessageRole::Developer
                        | MessageRole::User => "input_text",
                    },
                    "text": text,
                })),
                ContentPart::Image(source) => {
                    if message.role() != MessageRole::User {
                        return Err(CodexRequestEncodeError::UnsupportedContent);
                    }
                    content.push(json!({
                        "type": "input_image",
                        "image_url": image_url(source),
                    }));
                }
                ContentPart::ToolResult { call_id, output } => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                }
                ContentPart::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    if message.role() != MessageRole::Assistant {
                        return Err(CodexRequestEncodeError::UnsupportedContent);
                    }
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments,
                    }));
                }
                _ => return Err(CodexRequestEncodeError::UnsupportedContent),
            }
        }
        if !content.is_empty() {
            input.push(json!({
                "type": "message",
                "role": message_role(message.role()),
                "content": content,
            }));
        }
    }
    if input.is_empty() {
        return Err(CodexRequestEncodeError::UnsupportedContent);
    }
    Ok(input)
}

fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Url(url) => url.clone(),
        ImageSource::Bytes { media_type, data } => format!(
            "data:{media_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(data)
        ),
    }
}

fn encode_output_format(format: &OutputFormat) -> Value {
    let format = match format {
        OutputFormat::Text => json!({"type": "text"}),
        OutputFormat::JsonObject => json!({"type": "json_object"}),
        OutputFormat::JsonSchema(schema) => {
            let mut value = Map::new();
            value.insert("type".to_owned(), Value::String("json_schema".to_owned()));
            value.insert("name".to_owned(), Value::String(schema.name().to_owned()));
            if let Some(description) = schema.description() {
                value.insert(
                    "description".to_owned(),
                    Value::String(description.to_owned()),
                );
            }
            value.insert("schema".to_owned(), Value::Object(schema.schema().clone()));
            value.insert("strict".to_owned(), Value::Bool(schema.strict()));
            Value::Object(value)
        }
    };
    json!({"format": format})
}

fn extract_request_context(request: &mut CodexResponsesRequest) {
    let context = ExtractedRequestContext::from_body(request.body());
    request.turn_state = context.turn_state;
    request.turn_metadata = context.turn_metadata;
    request.beta_features = context.beta_features;
    request.version = context.version;
    request.include_timing_metrics = context.include_timing_metrics;
    request.codex_window_id = context.codex_window_id;
    request.parent_thread_id = context.parent_thread_id;
    request.client_conversation_id = context.conversation_id;
    request.client_session_id = context.session_id;
    request.client_thread_id = context.thread_id;
    request.client_request_id = context.client_request_id;
    request.client_turn_id = context.turn_id;
    request.responses_lite = context.responses_lite;
    request.memgen_request = context.memgen_request;
}

struct ExtractedRequestContext {
    turn_state: Option<String>,
    turn_metadata: Option<String>,
    beta_features: Option<String>,
    version: Option<String>,
    include_timing_metrics: Option<String>,
    codex_window_id: Option<String>,
    parent_thread_id: Option<String>,
    conversation_id: Option<String>,
    session_id: Option<String>,
    thread_id: Option<String>,
    client_request_id: Option<String>,
    turn_id: Option<String>,
    responses_lite: Option<String>,
    memgen_request: Option<String>,
}

impl ExtractedRequestContext {
    fn from_body(body: &Map<String, Value>) -> Self {
        Self {
            turn_state: body_or_metadata_string(
                body,
                &["turnState", "turn_state", "x-codex-turn-state"],
            ),
            turn_metadata: body_or_metadata_string(
                body,
                &["turnMetadata", "turn_metadata", "x-codex-turn-metadata"],
            ),
            beta_features: body_or_metadata_string(body, &["betaFeatures", "beta_features"]),
            version: body_or_metadata_string(body, &["version"]),
            include_timing_metrics: body_or_metadata_string(
                body,
                &["includeTimingMetrics", "include_timing_metrics"],
            ),
            codex_window_id: body_or_metadata_string(
                body,
                &["codexWindowId", "codex_window_id", "x-codex-window-id"],
            ),
            parent_thread_id: body_or_metadata_string(
                body,
                &[
                    "parentThreadId",
                    "parent_thread_id",
                    "x-codex-parent-thread-id",
                ],
            ),
            conversation_id: body_or_metadata_string(body, &["conversation_id", "conversationId"]),
            session_id: body_or_metadata_string(body, &["session_id", "sessionId"]),
            thread_id: body_or_metadata_string(body, &["thread_id", "threadId"]),
            client_request_id: body_or_metadata_string(
                body,
                &[
                    "x-client-request-id",
                    "client_request_id",
                    "clientRequestId",
                ],
            ),
            turn_id: body_or_metadata_string(body, &["turn_id", "turnId", "x-codex-turn-id"]),
            responses_lite: body_or_metadata_string(
                body,
                &["ws_request_header_x_openai_internal_codex_responses_lite"],
            ),
            memgen_request: body_or_metadata_string(
                body,
                &["x-openai-memgen-request", "memgen_request"],
            ),
        }
    }
}

fn body_or_metadata_string(body: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| non_empty_string(body.get(*key)))
        .or_else(|| {
            let metadata = body.get("client_metadata")?.as_object()?;
            keys.iter()
                .find_map(|key| non_empty_string(metadata.get(*key)))
        })
}

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn derive_account_scoped_local_conversation_id(
    request: &mut CodexResponsesRequest,
    installation_id: &str,
) {
    let anchor = derive_conversation_anchor(request);
    let Some((domain, value)) = anchor else {
        return;
    };
    let digest = hmac_sha256(
        installation_id.as_bytes(),
        &[
            b"codex-local-conversation-v2\0",
            domain.as_bytes(),
            b"\0",
            value.as_bytes(),
        ],
    );
    request.local_conversation_id = Some(format!("lc_{}", hex::encode(digest)));
}

pub(crate) fn derive_conversation_anchor(
    request: &CodexResponsesRequest,
) -> Option<(&'static str, String)> {
    request
        .client_session_id
        .as_deref()
        .map(|value| ("session", value.to_owned()))
        .or_else(|| {
            request
                .client_conversation_id
                .as_deref()
                .map(|value| ("conversation", value.to_owned()))
        })
        .or_else(|| {
            request
                .client_thread_id
                .as_deref()
                .map(|value| ("thread", value.to_owned()))
        })
        .or_else(|| {
            request
                .prompt_cache_key()
                .map(|value| ("prompt-cache", value.to_owned()))
        })
        .or_else(|| derive_stable_conversation_key(request).map(|value| ("request", value)))
}

const LEADING_SYSTEM_REMINDER_OPEN: &str = "<system-reminder>";
const LEADING_SYSTEM_REMINDER_CLOSE: &str = "</system-reminder>";

fn derive_stable_conversation_key(request: &CodexResponsesRequest) -> Option<String> {
    let instructions = request
        .instructions()
        .chars()
        .take(2_000)
        .collect::<String>();
    let first_user_text = first_user_text(request.input());
    let normalized = normalize_conversation_anchor_text(&first_user_text);
    let first_user_text = if normalized.is_empty() {
        first_user_text
    } else {
        normalized
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
    let digest = hex::encode(hasher.finalize());
    Some(format!(
        "{}-{}-{}-{}-{}",
        &digest[0..8],
        &digest[8..12],
        &digest[12..16],
        &digest[16..20],
        &digest[20..32]
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
            return text.to_owned();
        }
        if let Some(parts) = content.as_array() {
            return parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("input_text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
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
        rest = rest[close_start + LEADING_SYSTEM_REMINDER_CLOSE.len()..].trim_start();
    }
    rest.to_owned()
}

fn hmac_sha256(key: &[u8], message: &[&[u8]]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;

    let mut key_block = [0_u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = key_block;
    let mut outer_pad = key_block;
    for byte in &mut inner_pad {
        *byte ^= 0x36;
    }
    for byte in &mut outer_pad {
        *byte ^= 0x5c;
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    for part in message {
        inner.update(part);
    }
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

/// 把客户端正文收敛到当前 lease 的账号身份边界。
///
/// 真实 account ID 与 installation ID 由随后构造的 `CodexRequestContext` 注入
/// 请求头；正文只替换客户端原本声明过的 installation 字段，绝不接受客户端
/// 提供的 token、cookie 或账号身份。
pub(crate) fn scope_request_to_account(
    request: &mut CodexResponsesRequest,
    installation_id: &str,
    cross_account: bool,
) {
    for key in CROSS_ACCOUNT_IDENTITY_KEYS {
        request.body_mut().remove(*key);
    }
    for key in UNTRUSTED_CONTINUATION_KEYS {
        request.body_mut().remove(*key);
    }

    let client_metadata_turn_state = metadata_string(request, "x-codex-turn-state");
    let preserve_turn_state =
        !cross_account && (request.turn_state.is_some() || client_metadata_turn_state.is_some());
    let turn_state = preserve_turn_state
        .then(|| request.turn_state.clone())
        .flatten();
    let client_metadata_turn_state = if preserve_turn_state {
        client_metadata_turn_state
    } else {
        None
    };
    let turn_metadata = request
        .turn_metadata
        .as_deref()
        .and_then(|metadata| scope_turn_metadata(metadata, installation_id, cross_account));
    let client_metadata_turn_metadata = metadata_string(request, "x-codex-turn-metadata")
        .and_then(|metadata| scope_turn_metadata(&metadata, installation_id, cross_account));

    if cross_account {
        sanitize_cross_account_input(request);
        for key in ACCOUNT_BOUND_STATE_KEYS {
            request.body_mut().remove(*key);
        }
    }

    for key in INSTALLATION_ID_KEYS {
        request.replace_existing_identity_field(key, Some(installation_id));
    }
    for (key, value) in [
        ("turnState", turn_state.as_deref()),
        ("x-codex-turn-state", turn_state.as_deref()),
        ("turnMetadata", turn_metadata.as_deref()),
        ("x-codex-turn-metadata", turn_metadata.as_deref()),
    ] {
        request.replace_existing_identity_field(key, value);
    }

    if let Some(client_metadata) = request.client_metadata().cloned() {
        let scoped = match client_metadata {
            Value::Object(mut metadata) => {
                let scoped_turn_metadata =
                    ["turnMetadata", "turn_metadata", "x-codex-turn-metadata"].map(|key| {
                        (
                            key,
                            metadata.get(key).and_then(Value::as_str).and_then(|value| {
                                scope_turn_metadata(value, installation_id, cross_account)
                            }),
                        )
                    });
                for key in CROSS_ACCOUNT_IDENTITY_KEYS {
                    metadata.remove(*key);
                }
                if cross_account {
                    for key in ACCOUNT_BOUND_STATE_KEYS {
                        metadata.remove(*key);
                    }
                }
                metadata.insert(
                    "x-codex-installation-id".to_owned(),
                    Value::String(installation_id.to_owned()),
                );
                replace_existing_metadata_field(
                    &mut metadata,
                    "installation_id",
                    Some(installation_id),
                );
                replace_existing_metadata_field(
                    &mut metadata,
                    "installationId",
                    Some(installation_id),
                );
                replace_metadata_field(
                    &mut metadata,
                    "x-codex-turn-state",
                    client_metadata_turn_state.as_deref(),
                );
                replace_metadata_field(
                    &mut metadata,
                    "x-codex-turn-metadata",
                    client_metadata_turn_metadata.as_deref(),
                );
                if !cross_account {
                    for (key, value) in scoped_turn_metadata {
                        replace_existing_metadata_field(&mut metadata, key, value.as_deref());
                    }
                }
                (!metadata.is_empty()).then_some(Value::Object(metadata))
            }
            value if !cross_account => Some(value),
            Value::Null => None,
            _ => None,
        };
        request.set_client_metadata(scoped);
    }

    request.turn_state = turn_state;
    request.turn_metadata = turn_metadata;
    derive_account_scoped_local_conversation_id(request, installation_id);
}

fn sanitize_cross_account_input(request: &mut CodexResponsesRequest) {
    if request.input().is_empty() {
        return;
    }
    let input = request
        .input()
        .iter()
        .cloned()
        .filter_map(sanitize_cross_account_item)
        .collect();
    request.set_input(input);
}

pub(crate) fn sanitize_cross_account_item(mut item: Value) -> Option<Value> {
    if let Value::Object(object) = &mut item {
        if matches!(
            object.get("type").and_then(Value::as_str),
            Some("compaction" | "compaction_summary" | "context_compaction")
        ) {
            return None;
        }
        object.remove("id");
        object.remove("encrypted_content");
    }
    Some(item)
}

fn metadata_string(request: &CodexResponsesRequest, key: &str) -> Option<String> {
    request
        .client_metadata()?
        .as_object()?
        .get(key)?
        .as_str()
        .map(ToOwned::to_owned)
}

fn scope_turn_metadata(raw: &str, installation_id: &str, cross_account: bool) -> Option<String> {
    let Ok(Value::Object(mut metadata)) = serde_json::from_str::<Value>(raw) else {
        return (!cross_account).then(|| raw.to_owned());
    };
    let mut changed = false;
    for key in CROSS_ACCOUNT_IDENTITY_KEYS {
        changed |= metadata.remove(*key).is_some();
    }
    if cross_account {
        for key in ACCOUNT_BOUND_STATE_KEYS {
            changed |= metadata.remove(*key).is_some();
        }
    }
    if !cross_account
        && !changed
        && !INSTALLATION_ID_KEYS
            .iter()
            .any(|key| metadata.contains_key(*key))
    {
        return Some(raw.to_owned());
    }
    for key in INSTALLATION_ID_KEYS {
        if metadata.contains_key(*key) {
            metadata.insert((*key).to_owned(), Value::String(installation_id.to_owned()));
        }
    }
    serde_json::to_string(&metadata).ok()
}

fn replace_existing_metadata_field(
    metadata: &mut Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    if metadata.contains_key(key) {
        replace_metadata_field(metadata, key, value);
    }
}

fn replace_metadata_field(metadata: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    match value.filter(|value| !value.trim().is_empty()) {
        Some(value) => {
            metadata.insert(key.to_owned(), Value::String(value.to_owned()));
        }
        None => {
            metadata.remove(key);
        }
    }
}

fn apply_codex_options(
    request: &mut CodexResponsesRequest,
    options: &Map<String, Value>,
) -> Result<(), CodexRequestEncodeError> {
    if options
        .keys()
        .any(|field| !CODEX_OPTION_FIELDS.contains(&field.as_str()))
    {
        return Err(CodexRequestEncodeError::UnsupportedProviderOption);
    }
    if options.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(CodexRequestEncodeError::InvalidProviderOptions);
    }

    request.turn_state = request
        .turn_state
        .take()
        .or(optional_string(options, "turn_state")?);
    request.turn_metadata = request
        .turn_metadata
        .take()
        .or(optional_string(options, "turn_metadata")?);
    request.beta_features = request
        .beta_features
        .take()
        .or(optional_string(options, "beta_features")?);
    request.version = request
        .version
        .take()
        .or(optional_string(options, "version")?);
    request.include_timing_metrics = request
        .include_timing_metrics
        .take()
        .or(optional_string(options, "include_timing_metrics")?);
    request.codex_window_id = request
        .codex_window_id
        .take()
        .or(optional_string(options, "codex_window_id")?);
    request.parent_thread_id = request
        .parent_thread_id
        .take()
        .or(optional_string(options, "parent_thread_id")?);
    request.client_conversation_id = request
        .client_conversation_id
        .take()
        .or(optional_string(options, "conversation_id")?);
    request.client_session_id = request
        .client_session_id
        .take()
        .or(optional_string(options, "session_id")?);
    request.client_thread_id = request
        .client_thread_id
        .take()
        .or(optional_string(options, "thread_id")?);
    request.client_request_id = request
        .client_request_id
        .take()
        .or(optional_string(options, "client_request_id")?);
    request.client_turn_id = request
        .client_turn_id
        .take()
        .or(optional_string(options, "turn_id")?);
    request.responses_lite =
        optional_string(options, "responses_lite")?.or_else(|| request.responses_lite.take());
    request.memgen_request =
        optional_string(options, "memgen_request")?.or_else(|| request.memgen_request.take());
    match optional_string(options, "transport")?.as_deref() {
        Some("http_sse") => encoded_transport(request, false),
        Some("websocket") => encoded_transport(request, true),
        Some(_) => return Err(CodexRequestEncodeError::InvalidProviderOptions),
        None => {}
    }

    for field in ["prompt_cache_key", "service_tier"] {
        if let Some(value) = optional_string(options, field)? {
            if field == "prompt_cache_key" && request.prompt_cache_key().is_some() {
                return Err(CodexRequestEncodeError::InvalidProviderOptions);
            }
            request
                .body_mut()
                .insert(field.to_owned(), Value::String(value));
        }
    }
    Ok(())
}

fn encoded_transport(request: &mut CodexResponsesRequest, websocket: bool) {
    request.force_http_sse = !websocket;
    request.use_websocket = websocket;
}

fn optional_string(
    options: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, CodexRequestEncodeError> {
    match options.get(field) {
        None => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() && value.len() <= 8_192 => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(CodexRequestEncodeError::InvalidProviderOptions),
    }
}

const fn message_role(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::Developer => "developer",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

const fn reasoning_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::ExtraHigh => "xhigh",
    }
}

const fn reasoning_summary(summary: ReasoningSummary) -> &'static str {
    match summary {
        ReasoningSummary::Auto => "auto",
        ReasoningSummary::Concise => "concise",
        ReasoningSummary::Detailed => "detailed",
        ReasoningSummary::None => "none",
    }
}
