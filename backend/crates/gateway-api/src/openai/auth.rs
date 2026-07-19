//! OpenAI 客户端协议的 Bearer API key 认证。

use axum::{
    http::{HeaderMap, header::AUTHORIZATION},
    response::{IntoResponse, Response},
};

use super::{
    error::{missing_client_api_key_response, runtime_unavailable_response},
    service::OpenAiClientService,
};

/// Client API key 鉴权失败原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientApiKeyAuthError {
    /// 缺失 Authorization 头。
    MissingAuthorization,
    /// Authorization 头不是合法的 Bearer token。
    MalformedAuthorization,
    /// Bearer token 不是 client API key 格式。
    InvalidKeyFormat,
    /// Key 不存在、已禁用或 wire 格式无效。
    InvalidKey,
    /// RuntimeSnapshot 一致性保护暂停接收新请求。
    RuntimeUnavailable,
}

impl ClientApiKeyAuthError {
    /// 返回可用于日志和指标的稳定失败原因。
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

/// 从请求头提取 Bearer Client API key。
///
/// # Errors
///
/// Header 缺失、Bearer 语法错误或不是网关 Client Key 前缀时返回稳定错误。
pub fn bearer_client_api_key(headers: &HeaderMap) -> Result<&str, ClientApiKeyAuthError> {
    let raw = headers
        .get(AUTHORIZATION)
        .ok_or(ClientApiKeyAuthError::MissingAuthorization)?
        .to_str()
        .map_err(|_| ClientApiKeyAuthError::MalformedAuthorization)?;
    let token = raw
        .strip_prefix("Bearer ")
        .ok_or(ClientApiKeyAuthError::MalformedAuthorization)?
        .trim();
    if token.is_empty() {
        return Err(ClientApiKeyAuthError::MalformedAuthorization);
    }
    if !token.starts_with("sk_") {
        return Err(ClientApiKeyAuthError::InvalidKeyFormat);
    }
    Ok(token)
}

pub(crate) fn authenticate_client<S>(
    service: &S,
    headers: &HeaderMap,
) -> Result<S::Client, ClientApiKeyAuthError>
where
    S: OpenAiClientService,
{
    bearer_client_api_key(headers).and_then(|key| service.authenticate(key))
}

pub(crate) fn authentication_error_response(error: ClientApiKeyAuthError) -> Response {
    log_client_api_key_auth_failure(error);
    if error == ClientApiKeyAuthError::RuntimeUnavailable {
        runtime_unavailable_response().into_response()
    } else {
        missing_client_api_key_response().into_response()
    }
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
