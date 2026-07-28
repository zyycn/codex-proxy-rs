//! 核心 Generate operation 到 Codex Responses wire request 的严格编码。

use base64::{Engine as _, engine::general_purpose::STANDARD};
use gateway_core::operation::GenerateRequest;
use gateway_protocol::openai::WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::transport::protocol::responses::CodexResponsesRequest;

const PASSTHROUGH_HEADERS_CONTEXT_KEY: &str = "opaque_request_headers";

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
    "previous_response_id",
    "previousResponseId",
    "response_id",
    "responseId",
    "conversation",
];

const TURN_METADATA_KEYS: &[&str] = &["turnMetadata", "turn_metadata", "x-codex-turn-metadata"];

const INSTALLATION_ID_KEYS: &[&str] = &[
    "installation_id",
    "installationId",
    "x-codex-installation-id",
];

/// Provider 专属编码错误；不保存 prompt、schema 或 option 值。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodexRequestEncodeError {
    #[error("Codex request is missing its OpenAI protocol payload")]
    InvalidProtocolPayload,
}

pub fn encode_generate_request(
    request: &GenerateRequest,
    upstream_model: &str,
) -> Result<CodexResponsesRequest, CodexRequestEncodeError> {
    let payload = request.protocol_payload();
    if payload.protocol() != "openai" {
        return Err(CodexRequestEncodeError::InvalidProtocolPayload);
    }
    let mut body = payload.body().clone();
    body.insert("model".to_owned(), Value::String(upstream_model.to_owned()));

    let mut encoded = CodexResponsesRequest::from_body(body);
    encoded.explicit_prompt_cache_key = encoded.prompt_cache_key().is_some();
    extract_request_context(&mut encoded);
    apply_protocol_context(&mut encoded, payload.context());
    Ok(encoded)
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
            turn_state: body_string(body, "turnState"),
            turn_metadata: body_string(body, "turnMetadata"),
            beta_features: body_string(body, "betaFeatures"),
            version: body_string(body, "version"),
            include_timing_metrics: body_string(body, "includeTimingMetrics"),
            codex_window_id: body_string(body, "codexWindowId"),
            parent_thread_id: body_string(body, "parentThreadId"),
            conversation_id: body_string(body, "conversation_id"),
            session_id: body_string(body, "session_id"),
            thread_id: body_string(body, "thread_id"),
            client_request_id: body_string(body, "x-client-request-id"),
            turn_id: body_string(body, "turn_id"),
            // Responses Lite 在 WebSocket body 中的这个键是官方 header 投影，
            // 不是普通上下文字段的 metadata 回退。
            responses_lite: body
                .get("client_metadata")
                .and_then(Value::as_object)
                .and_then(|metadata| {
                    string_value(metadata.get(WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY))
                }),
            // Memory consolidation 只由官方请求头提供；body 中同名字段不参与
            // transport 事实提取。
            memgen_request: None,
        }
    }
}

fn body_string(body: &Map<String, Value>, key: &str) -> Option<String> {
    string_value(body.get(key))
}

fn string_value(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(ToOwned::to_owned)
}

pub(crate) fn derive_conversation_anchor(
    request: &CodexResponsesRequest,
) -> Option<(&'static str, String)> {
    request
        .prompt_cache_key()
        .map(|value| ("prompt-cache", value.to_owned()))
        .or_else(|| {
            request
                .client_session_id
                .as_deref()
                .map(|value| ("session", value.to_owned()))
        })
        .or_else(|| {
            request
                .client_thread_id
                .as_deref()
                .map(|value| ("thread", value.to_owned()))
        })
        .or_else(|| {
            request
                .client_conversation_id
                .as_deref()
                .map(|value| ("conversation", value.to_owned()))
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
        request.passthrough_headers.remove("x-codex-turn-state");
        request.passthrough_headers.remove("x-codex-turn-metadata");
        sanitize_cross_account_input(request);
        for key in CROSS_ACCOUNT_IDENTITY_KEYS
            .iter()
            .chain(ACCOUNT_BOUND_STATE_KEYS)
        {
            request.body_mut().remove(*key);
        }
    }

    for key in INSTALLATION_ID_KEYS {
        request.replace_existing_identity_field(key, Some(installation_id));
    }
    for key in TURN_METADATA_KEYS {
        let scoped = request
            .body()
            .get(*key)
            .and_then(Value::as_str)
            .and_then(|value| scope_turn_metadata(value, installation_id, cross_account));
        replace_existing_body_string(request, key, scoped.as_deref());
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
                if cross_account {
                    for key in CROSS_ACCOUNT_IDENTITY_KEYS
                        .iter()
                        .chain(ACCOUNT_BOUND_STATE_KEYS)
                    {
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
                for (key, value) in scoped_turn_metadata {
                    replace_existing_metadata_field(&mut metadata, key, value.as_deref());
                }
                (!metadata.is_empty()).then_some(Value::Object(metadata))
            }
            value => Some(value),
        };
        request.set_client_metadata(scoped);
    }

    request.turn_state = turn_state;
    request.turn_metadata = turn_metadata;
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
    if cross_account {
        for key in CROSS_ACCOUNT_IDENTITY_KEYS
            .iter()
            .chain(ACCOUNT_BOUND_STATE_KEYS)
            .chain(TURN_METADATA_KEYS)
        {
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

fn replace_existing_body_string(
    request: &mut CodexResponsesRequest,
    key: &str,
    value: Option<&str>,
) {
    if request.body().get(key).is_some_and(Value::is_string)
        && let Some(value) = value
    {
        request
            .body_mut()
            .insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn replace_existing_metadata_field(
    metadata: &mut Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    if metadata.contains_key(key)
        && let Some(value) = value
    {
        metadata.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn replace_metadata_field(metadata: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value
        && metadata.get(key).is_none_or(Value::is_string)
    {
        metadata.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

// 迁移契约是 header-authoritative:连接边界解析出的协议上下文(官方请求头)
// 优先;body 顶层别名只在 header 缺失时兜底(例如 WebSocket 帧无法逐轮携带
// 请求头的场景)。
fn apply_protocol_context(request: &mut CodexResponsesRequest, context: &Map<String, Value>) {
    request.passthrough_headers = decode_passthrough_headers(context);
    request.turn_state =
        context_string(context, "turn_state").or_else(|| request.turn_state.take());
    request.turn_metadata =
        context_string(context, "turn_metadata").or_else(|| request.turn_metadata.take());
    request.beta_features =
        context_string(context, "beta_features").or_else(|| request.beta_features.take());
    request.version = context_string(context, "version").or_else(|| request.version.take());
    request.include_timing_metrics = context_string(context, "include_timing_metrics")
        .or_else(|| request.include_timing_metrics.take());
    request.codex_window_id =
        context_string(context, "codex_window_id").or_else(|| request.codex_window_id.take());
    request.parent_thread_id =
        context_string(context, "parent_thread_id").or_else(|| request.parent_thread_id.take());
    let prompt_cache_key = request.prompt_cache_key().map(ToOwned::to_owned);
    request.client_conversation_id = context_string(context, "conversation_id")
        .or_else(|| request.client_conversation_id.take())
        .or(prompt_cache_key);
    request.client_session_id =
        context_string(context, "session_id").or_else(|| request.client_session_id.take());
    request.client_thread_id =
        context_string(context, "thread_id").or_else(|| request.client_thread_id.take());
    request.client_request_id =
        context_string(context, "client_request_id").or_else(|| request.client_request_id.take());
    request.client_turn_id =
        context_string(context, "turn_id").or_else(|| request.client_turn_id.take());
    request.responses_lite =
        context_string(context, "responses_lite").or_else(|| request.responses_lite.take());
    request.memgen_request =
        context_string(context, "memgen_request").or_else(|| request.memgen_request.take());
    match context.get("use_websocket").and_then(Value::as_bool) {
        Some(true) => {
            request.use_websocket = true;
            request.force_http_sse = false;
        }
        Some(false) => {
            request.use_websocket = false;
            request.force_http_sse = true;
        }
        None => {}
    }
}

fn decode_passthrough_headers(context: &Map<String, Value>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let Some(entries) = context
        .get(PASSTHROUGH_HEADERS_CONTEXT_KEY)
        .and_then(Value::as_array)
    else {
        return headers;
    };

    for entry in entries {
        let Some(entry) = entry.as_array().filter(|entry| entry.len() == 2) else {
            continue;
        };
        let Some(name) = entry.first().and_then(Value::as_str) else {
            continue;
        };
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        if provider_managed_header(name.as_str()) {
            continue;
        }
        let Some(encoded) = entry.get(1).and_then(Value::as_str) else {
            continue;
        };
        let Ok(bytes) = STANDARD.decode(encoded) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_bytes(&bytes) else {
            continue;
        };
        headers.append(name, value);
    }
    headers
}

fn provider_managed_header(name: &str) -> bool {
    name.starts_with("sec-websocket-")
        || name.starts_with("x-grok-")
        || name.starts_with("x-xai-")
        || matches!(
            name,
            "connection"
                | "keep-alive"
                | "proxy-connection"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "host"
                | "content-length"
                | "forwarded"
                | "x-forwarded-for"
                | "x-forwarded-host"
                | "x-forwarded-proto"
                | "x-forwarded-port"
                | "x-real-ip"
                | "true-client-ip"
                | "cf-connecting-ip"
                | "x-request-id"
                | "authorization"
                | "x-api-key"
                | "cookie"
                | "cookie2"
                | "chatgpt-account-id"
                | "chatgpt-organization-id"
                | "chatgpt-org-id"
                | "chatgpt-project-id"
                | "openai-organization"
                | "openai-project"
                | "x-openai-organization"
                | "x-openai-project"
                | "x-codex-installation-id"
        )
}

fn context_string(context: &Map<String, Value>, field: &str) -> Option<String> {
    context
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}
