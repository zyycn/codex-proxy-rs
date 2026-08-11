//! 上游响应元数据提取辅助。

use std::fmt;

use bytes::Bytes;
use gateway_protocol::openai::events::{is_codex_quota_header_name, is_rate_limit_header_name};
use reqwest::header::{HeaderMap, SET_COOKIE};

use super::diagnostics::CodexUpstreamDiagnostics;

/// Codex Responses 上游响应元数据。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct CodexResponseMetadata {
    /// 上游实际选用的模型。
    pub effective_model: Option<String>,
    /// 模型目录版本。
    pub models_etag: Option<String>,
    /// 上游是否声明响应包含 reasoning。
    pub reasoning_included: bool,
    /// 允许交给 Core 的普通响应头；名称和值保持 transport 观察到的顺序与字节。
    pub client_headers: Vec<(String, Bytes)>,
}

impl fmt::Debug for CodexResponseMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexResponseMetadata")
            .field("effective_model", &self.effective_model)
            .field("models_etag", &self.models_etag)
            .field("reasoning_included", &self.reasoning_included)
            .field("client_header_count", &self.client_headers.len())
            .finish()
    }
}

pub(super) fn diagnostics(
    status_code: Option<u16>,
    headers: &HeaderMap,
) -> CodexUpstreamDiagnostics {
    CodexUpstreamDiagnostics::from_headers(status_code, headers)
}

pub(super) fn turn_state(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-codex-turn-state")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

pub(super) fn set_cookie_headers(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToString::to_string))
        .collect()
}

pub(super) fn rate_limit_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| is_rate_limit_header_name(name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

pub(super) fn response_metadata(headers: &HeaderMap) -> CodexResponseMetadata {
    response_metadata_from_client_headers(client_headers(headers))
}

/// 提取响应中可交给客户端 adapter 的普通头。
///
/// 账号身份、凭据、cookie、逐跳头和已经由 adapter 重建的 framing 在进入 Core 前剔除；
/// 其余名称和值保持 HeaderMap 的多值顺序和原始字节。
pub(super) fn client_headers(headers: &HeaderMap) -> Vec<(String, Bytes)> {
    filter_client_headers(headers.iter().map(|(name, value)| {
        (
            name.as_str().to_owned(),
            Bytes::copy_from_slice(value.as_bytes()),
        )
    }))
}

pub(super) fn merge_response_metadata(
    metadata: &mut CodexResponseMetadata,
    headers: impl IntoIterator<Item = (String, String)>,
) {
    for (name, value) in filter_client_headers(
        headers
            .into_iter()
            .map(|(name, value)| (name, Bytes::from(value))),
    ) {
        observe_typed_response_header(metadata, &name, &value);
        metadata.client_headers.push((name, value));
    }
}

fn response_metadata_from_client_headers(
    client_headers: Vec<(String, Bytes)>,
) -> CodexResponseMetadata {
    let mut metadata = CodexResponseMetadata::default();
    for (name, value) in &client_headers {
        observe_typed_response_header(&mut metadata, name, value);
    }
    metadata.client_headers = client_headers;
    metadata
}

fn observe_typed_response_header(metadata: &mut CodexResponseMetadata, name: &str, value: &[u8]) {
    let Ok(value) = std::str::from_utf8(value) else {
        return;
    };
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    match name.trim().to_ascii_lowercase().as_str() {
        "openai-model" => metadata.effective_model = Some(value.to_string()),
        "x-models-etag" => metadata.models_etag = Some(value.to_string()),
        "x-reasoning-included" => metadata.reasoning_included = true,
        _ => {}
    }
}

fn filter_client_headers(
    headers: impl IntoIterator<Item = (String, Bytes)>,
) -> Vec<(String, Bytes)> {
    let headers = headers.into_iter().collect::<Vec<_>>();
    let connection_options = headers
        .iter()
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("connection"))
        .filter_map(|(_, value)| std::str::from_utf8(value).ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();

    headers
        .into_iter()
        .filter(|(name, _)| client_response_header_is_forwardable(name, &connection_options))
        .collect()
}

fn client_response_header_is_forwardable(name: &str, connection_options: &[String]) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if connection_options
        .iter()
        .any(|option| option.eq_ignore_ascii_case(&name))
        || name.starts_with("sec-websocket-")
    {
        return false;
    }
    if is_codex_quota_header_name(&name) {
        return false;
    }

    !matches!(
        name.as_str(),
        // 逐跳和 framing 由下游 adapter 针对实际 JSON/SSE/WebSocket 载体重建。
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "content-type"
            | "content-encoding"
            // 上游账号、凭据和 cookie 不能跨越换号边界。
            | "authorization"
            | "x-api-key"
            | "www-authenticate"
            | "authentication-info"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-authentication-info"
            | "cookie"
            | "cookie2"
            | "set-cookie"
            | "set-cookie2"
            | "chatgpt-account-id"
            | "chatgpt-organization-id"
            | "chatgpt-org-id"
            | "chatgpt-project-id"
            | "openai-organization"
            | "openai-project"
            | "x-openai-organization"
            | "x-openai-project"
            | "x-codex-installation-id"
            | "x-codex-turn-state"
            | "x-codex-turn-metadata"
    )
}
