//! OpenAI API 认证。
//!
//! 提供 Bearer token 提取与客户端 API key 校验。

use axum::http::{HeaderMap, header::AUTHORIZATION};

use crate::api::AppState;

/// client API key 鉴权失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientApiKeyAuthError {
    /// 缺失 Authorization 头。
    MissingAuthorization,
    /// Authorization 头不是合法的 Bearer token。
    MalformedAuthorization,
    /// Bearer token 不是 client API key 格式。
    InvalidKeyFormat,
    /// client API key 不存在或已禁用。
    InvalidKey,
    /// 鉴权存储失败。
    StoreUnavailable { message: String },
}

impl ClientApiKeyAuthError {
    /// 返回可用于日志和指标的稳定失败原因。
    pub fn reason(&self) -> &'static str {
        match self {
            Self::MissingAuthorization => "missing_authorization",
            Self::MalformedAuthorization => "malformed_authorization",
            Self::InvalidKeyFormat => "invalid_key_format",
            Self::InvalidKey => "invalid_key",
            Self::StoreUnavailable { .. } => "store_unavailable",
        }
    }
}

/// 从请求头提取 Bearer 客户端 API key。
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

/// 校验客户端 API key。
pub async fn authorize_client_api_key(state: &AppState, headers: &HeaderMap) -> bool {
    authorized_client_api_key_id(state, headers).await.is_some()
}

/// 校验客户端 API key，成功时返回用于事实归因的稳定 ID。
pub async fn authorized_client_api_key_id(state: &AppState, headers: &HeaderMap) -> Option<String> {
    match authorize_client_api_key_result(state, headers).await {
        Ok(key_id) => Some(key_id),
        Err(error) => {
            log_client_api_key_auth_failure(&error);
            None
        }
    }
}

/// 校验客户端 API key，返回可观测的失败原因。
pub async fn authorize_client_api_key_result(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, ClientApiKeyAuthError> {
    let api_key = bearer_client_api_key(headers)?;
    let verified = state
        .services
        .client_keys
        .verify(api_key)
        .await
        .map_err(|error| ClientApiKeyAuthError::StoreUnavailable {
            message: error.to_string(),
        })?;
    verified.ok_or(ClientApiKeyAuthError::InvalidKey)
}

fn log_client_api_key_auth_failure(error: &ClientApiKeyAuthError) {
    match error {
        ClientApiKeyAuthError::StoreUnavailable { message } => {
            tracing::warn!(
                auth_failure = error.reason(),
                error = %message,
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
