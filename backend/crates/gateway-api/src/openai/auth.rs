//! 数据面客户端 Bearer 与 x-api-key 认证

use axum::{
    http::{HeaderMap, header::AUTHORIZATION},
    response::{IntoResponse, Response},
};
use gateway_core::engine::{
    authentication::ClientAuthenticationRequest, execution::AuthenticatedClient,
};
use gateway_core::policy::{ClientVersionRejection, CodexClientKind, CodexClientVersion};

use super::{
    error::{
        client_version_rejection_response, missing_client_api_key_response,
        runtime_unavailable_response,
    },
    service::OpenAiService,
};

/// Client API key 鉴权失败原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientApiKeyAuthError {
    /// 缺失 Authorization 头
    MissingAuthorization,
    /// Authorization 头不是合法的 Bearer token
    MalformedAuthorization,
    /// Bearer token 不是 client API key 格式
    InvalidKeyFormat,
    /// Key 不存在、已禁用或 wire 格式无效
    InvalidKey,
    /// RuntimeSnapshot 一致性保护暂停接收新请求
    RuntimeUnavailable,
}

/// Client API Key 或最低版本准入失败
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientAccessError {
    Authentication(ClientApiKeyAuthError),
    Version(ClientVersionRejection),
}

impl From<ClientApiKeyAuthError> for ClientAccessError {
    fn from(error: ClientApiKeyAuthError) -> Self {
        Self::Authentication(error)
    }
}

impl From<ClientVersionRejection> for ClientAccessError {
    fn from(error: ClientVersionRejection) -> Self {
        Self::Version(error)
    }
}

/// 从有界请求头中识别出的客户端及其可选合法版本
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifiedCodexClient {
    kind: CodexClientKind,
    version: Option<CodexClientVersion>,
}

impl IdentifiedCodexClient {
    #[must_use]
    pub const fn kind(&self) -> CodexClientKind {
        self.kind
    }

    #[must_use]
    pub const fn version(&self) -> Option<&CodexClientVersion> {
        self.version.as_ref()
    }
}

impl ClientApiKeyAuthError {
    /// 返回可用于日志和指标的稳定失败原因
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::MissingAuthorization => "missing_authorization",
            Self::MalformedAuthorization => "malformed_authorization",
            Self::InvalidKeyFormat => "invalid_key_format",
            Self::InvalidKey => "invalid_key",
            Self::RuntimeUnavailable => "runtime_unavailable",
        }
    }
}

pub(crate) async fn authenticate_client(
    service: &OpenAiService,
    headers: &HeaderMap,
    accepted: ClientKeyHeaders,
) -> Result<AuthenticatedClient, ClientAccessError> {
    let request = client_authentication_request(headers, accepted)?;
    let client = service.authenticate(request).await?;
    if let Some(identified) = identify_codex_client(headers) {
        client
            .snapshot()
            .min_codex_client_versions()
            .enforce(identified.kind(), identified.version())?;
    }
    Ok(client)
}

/// 复用数据面入口认证计划，但不记录 Key 使用事实或执行推理准入
pub(crate) async fn verify_client(
    service: &OpenAiService,
    headers: &HeaderMap,
) -> Result<AuthenticatedClient, ClientApiKeyAuthError> {
    let request = client_authentication_request(headers, ClientKeyHeaders::Bearer)?;
    service.verify(request).await
}

/// 入口声明其认证头合同，所有形式仍进入同一 Client Key 认证链路
#[derive(Clone, Copy)]
pub(crate) enum ClientKeyHeaders {
    Bearer,
    Anthropic,
}

fn client_authentication_request(
    headers: &HeaderMap,
    accepted: ClientKeyHeaders,
) -> Result<ClientAuthenticationRequest, ClientApiKeyAuthError> {
    let raw = if let Some(authorization) = headers.get(AUTHORIZATION) {
        authorization
            .to_str()
            .map_err(|_| ClientApiKeyAuthError::MalformedAuthorization)?
            .to_owned()
    } else if matches!(accepted, ClientKeyHeaders::Anthropic)
        && let Some(key) = headers.get("x-api-key")
    {
        format!(
            "Bearer {}",
            key.to_str()
                .map_err(|_| ClientApiKeyAuthError::MalformedAuthorization)?
        )
    } else {
        return Err(ClientApiKeyAuthError::MissingAuthorization);
    };
    let request = ClientAuthenticationRequest::new(raw)
        .map_err(|_| ClientApiKeyAuthError::MalformedAuthorization)?;
    let request = request.with_request_context(
        crate::middleware::current().and_then(|context| context.extensions.shared_request()),
    );
    Ok(match crate::middleware::current_settings() {
        Some(settings) => request.with_settings(settings),
        None => request,
    })
}

pub(crate) fn client_access_error_response(error: ClientAccessError) -> Response {
    let code = match &error {
        ClientAccessError::Authentication(error) => error.reason(),
        ClientAccessError::Version(error) if error.current().is_some() => "client_version_too_old",
        ClientAccessError::Version(_) => "client_version_unavailable",
    };
    let mut response = match error {
        ClientAccessError::Authentication(error) => {
            log_client_api_key_auth_failure(error);
            if error == ClientApiKeyAuthError::RuntimeUnavailable {
                runtime_unavailable_response().into_response()
            } else {
                missing_client_api_key_response().into_response()
            }
        }
        ClientAccessError::Version(error) => {
            tracing::info!(
                client = error.kind().as_str(),
                current_version = error.current().map(ToString::to_string),
                min_version = %error.min(),
                "Codex client version requirement rejected request"
            );
            client_version_rejection_response(&error).into_response()
        }
    };
    response
        .extensions_mut()
        .insert(crate::client_error::AccessRejection(code));
    response
}

/// 识别 Desktop 应用或 CLI/Core；未知客户端及未提供应用版本的手机远程客户端返回 `None`
#[must_use]
pub fn identify_codex_client(headers: &HeaderMap) -> Option<IdentifiedCodexClient> {
    const MAXIMUM_HEADER_LENGTH: usize = 4096;
    // 官方 CLI、IDE、SDK 与 App Server 的 UA 产品版本均来自所运行的 Codex Core
    const CORE_CLIENT_PRODUCTS: &[&str] = &[
        "codex_cli_rs",
        "codex-cli",
        "codex-tui",
        "codex_exec",
        "codex_vscode",
        "codex_sdk_ts",
        "codex_python_sdk",
        "codex-app-server",
        "codex-app-server-sdk",
        "codex_mcp_server",
    ];

    let ascii_header = |name: &'static str| headers.get(name).and_then(|value| value.to_str().ok());
    let raw_user_agent = ascii_header("user-agent");
    // HeaderValue::to_str 只接受 ASCII，可以按字节截取搜索范围
    let user_agent = raw_user_agent.map(|ua| &ua[..ua.len().min(MAXIMUM_HEADER_LENGTH)]);
    let is_desktop = ascii_header("originator")
        .is_some_and(|value| value.eq_ignore_ascii_case("Codex Desktop"))
        || user_agent.is_some_and(|value| contains_ascii_case_insensitive(value, "Codex Desktop"));
    if is_desktop {
        let version_header = headers.get("version");
        let user_agent_version = user_agent.and_then(desktop_version_from_user_agent);
        if user_agent_version.is_none()
            && let Some(ua) = raw_user_agent.filter(|ua| ua.len() <= MAXIMUM_HEADER_LENGTH)
            && let Some(client) = user_agent_client_name(ua)
        {
            // 共享 App Server 可保留 Desktop 进程名，TUI 请求的 version 头也可能重复 Core 版本
            if client.eq_ignore_ascii_case("codex-tui")
                && let Some(core) = desktop_core_version(ua)
                && version_header.is_none_or(|value| value.as_bytes() == core.as_bytes())
            {
                return Some(IdentifiedCodexClient {
                    kind: CodexClientKind::Cli,
                    version: CodexClientVersion::parse(core).ok(),
                });
            }
            if version_header.is_none() && is_chatgpt_remote_client(client) {
                return None;
            }
        }
        let version = match version_header.and_then(|value| value.to_str().ok()) {
            Some(value) => CodexClientVersion::parse(value).ok(),
            None => user_agent_version.and_then(|value| CodexClientVersion::parse(value).ok()),
        };
        return Some(IdentifiedCodexClient {
            kind: CodexClientKind::Desktop,
            version,
        });
    }

    let user_agent = user_agent?;
    for product in user_agent.split(|character: char| {
        character.is_ascii_whitespace() || matches!(character, '(' | ')' | ';' | ',' | '[' | ']')
    }) {
        let (name, version) = product
            .split_once('/')
            .map_or((product, None), |(name, version)| (name, Some(version)));
        if CORE_CLIENT_PRODUCTS
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
        {
            return Some(IdentifiedCodexClient {
                kind: CodexClientKind::Cli,
                version: version.and_then(|value| CodexClientVersion::parse(value).ok()),
            });
        }
    }
    None
}

fn desktop_version_from_user_agent(user_agent: &str) -> Option<&str> {
    let marker = "Codex Desktop;";
    let start = find_ascii_case_insensitive(user_agent, marker)? + marker.len();
    // 空候选也代表已提供应用版本，不能被手机远程的缺失版本规则放行
    user_agent[start..]
        .trim_start()
        .split([')', ' ', ';', ','])
        .next()
}

fn desktop_core_version(user_agent: &str) -> Option<&str> {
    let (product, rest) = user_agent.split_once('/')?;
    let (version, _) = rest.split_once(' ')?;
    product
        .eq_ignore_ascii_case("Codex Desktop")
        .then_some(version)
}

fn user_agent_client_name(user_agent: &str) -> Option<&str> {
    let (_, suffix) = user_agent.strip_suffix(')')?.rsplit_once(" (")?;
    let (name, version) = suffix.split_once("; ")?;
    let valid_token = |value: &str| {
        !value.is_empty()
            && !value.contains(['(', ')', ';'])
            && !value.bytes().any(|byte| byte.is_ascii_whitespace())
    };
    (valid_token(name) && valid_token(version)).then_some(name)
}

fn is_chatgpt_remote_client(name: &str) -> bool {
    name.strip_prefix("codex_chatgpt_")
        .and_then(|value| value.strip_suffix("_remote"))
        .is_some_and(|platform| !platform.is_empty())
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    find_ascii_case_insensitive(haystack, needle).is_some()
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn log_client_api_key_auth_failure(error: ClientApiKeyAuthError) {
    match error {
        ClientApiKeyAuthError::RuntimeUnavailable => {
            tracing::warn!(
                auth_failure = error.reason(),
                "Client API key authorization failed"
            );
        }
        _ => {
            tracing::info!(
                auth_failure = error.reason(),
                "Client API key authorization failed"
            );
        }
    }
}
