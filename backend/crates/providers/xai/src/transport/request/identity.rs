//! 会话身份、租户隔离与缓存路由。

use super::*;

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
pub(super) fn sanitize_account_identity(body: &mut Map<String, Value>) {
    for field in IDENTITY_FIELDS.iter().chain(ACCOUNT_BOUND_FIELDS) {
        body.remove(*field);
    }
}

pub(super) fn sanitize_client_metadata(body: &mut Map<String, Value>) {
    // Codex 的 client_metadata 是本地 transport envelope，可能包含工作目录、仓库地址、
    // installation/session 标识。会话亲和信息已在调用本函数前提取，整个 envelope 都不能
    // 越过 Grok Build 边界。
    body.remove("client_metadata");
    body.remove("metadata");
}

pub(super) fn enable_grok_prompt_cache_route(
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

pub(super) fn explicit_session_seed(
    request: &GenerateRequest,
    body: &Map<String, Value>,
) -> Option<String> {
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

pub(super) fn resolve_session_identity(
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
