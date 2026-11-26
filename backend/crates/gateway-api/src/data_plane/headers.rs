//! 数据面入站业务头与上游响应头的公共传输边界

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gateway_core::event::ProviderResponseHeader;
use gateway_protocol::http::is_transport_managed_request_header;
use serde_json::{Map, Value};

/// 出口统一剥离已知账号身份，过滤范围不随客户端选择的协议变化
pub(crate) fn response_header_is_forwardable(name: &str, connection_options: &[String]) -> bool {
    gateway_protocol::http::response_header_is_forwardable(name, connection_options)
        && !gateway_protocol::openai::is_private_response_header(name)
}

pub(crate) fn response_connection_options(headers: &[ProviderResponseHeader]) -> Vec<String> {
    headers
        .iter()
        .filter(|header| header.name().trim().eq_ignore_ascii_case("connection"))
        .filter_map(|header| std::str::from_utf8(header.value()).ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

pub(crate) fn passthrough_headers(headers: &HeaderMap) -> Vec<Value> {
    passthrough_request_headers(headers)
        .map(|(name, value)| {
            Value::Array(vec![
                Value::String(name.as_str().to_owned()),
                Value::String(STANDARD.encode(value.as_bytes())),
            ])
        })
        .collect()
}

/// HTTP 与 Live 入站共用的身份、传输边界；保留业务头原始多值字节
pub(crate) fn passthrough_request_headers(
    headers: &HeaderMap,
) -> impl Iterator<Item = (&HeaderName, &HeaderValue)> {
    let connection_headers = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();

    headers
        .iter()
        .filter(move |(name, _)| passthrough_header_name(name.as_str(), &connection_headers))
}

fn passthrough_header_name(name: &str, connection_headers: &[String]) -> bool {
    if connection_headers
        .iter()
        .any(|connection_header| connection_header.eq_ignore_ascii_case(name))
        || is_transport_managed_request_header(name)
    {
        return false;
    }

    !matches!(
        name,
        // 下游鉴权和账号 cookie 绝不能成为上游账号身份
        "authorization"
            | "x-api-key"
            | "x-goog-api-key"
            // Codex 的服务端托管认证标记只用于客户端能力判断，不代表上游身份
            | "x-openai-actor-authorization"
            | "cookie"
            | "cookie2"
            | "chatgpt-account-id"
            | "chatgpt-project-id"
            | "openai-organization"
            | "openai-project"
            // 上游指纹必须由运行时画像统一生成，客户端 originator/User-Agent/version
            // 不能作为不透明头透传覆盖，避免不同下游客户端暴露不一致的设备指纹
            | "originator"
            | "user-agent"
            | "version"
            // 设备 attestation/integrity 头只能由官方客户端或网关自身生成，
            // 客户端注入的 x-oai-attestation / X-OAI-IS 不得透传上游
            | "x-oai-attestation"
            | "x-oai-is"
            | "x-oai-is-update"
    )
}

pub(crate) fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

/// 共享会话头与不透明业务头，不继承 Responses 的续接和连接状态
pub(crate) fn session_context(headers: &HeaderMap) -> Map<String, Value> {
    let mut context = Map::new();
    for (key, value) in [
        (
            "session_id",
            header_string(headers, "session-id").or_else(|| header_string(headers, "session_id")),
        ),
        ("thread_id", header_string(headers, "thread-id")),
        (
            "turn_metadata",
            header_string(headers, "x-codex-turn-metadata"),
        ),
    ] {
        if let Some(value) = value {
            context.insert(key.into(), Value::String(value));
        }
    }
    let passthrough = passthrough_headers(headers);
    if !passthrough.is_empty() {
        context.insert("opaque_request_headers".into(), Value::Array(passthrough));
    }
    context
}
