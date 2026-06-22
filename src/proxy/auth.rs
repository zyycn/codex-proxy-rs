//! OpenAI API 认证。
//!
//! 提供 Bearer token 提取与客户端 API key 校验。

use axum::http::HeaderMap;

use crate::runtime::state::AppState;

/// 从请求头提取 Bearer 客户端 API key。
pub fn bearer_client_api_key(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = raw.strip_prefix("Bearer ")?.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// 校验客户端 API key。
pub async fn authorize_client_api_key(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(api_key) = bearer_client_api_key(headers) else {
        return false;
    };

    state
        .services
        .client_keys
        .verify(&api_key)
        .await
        .unwrap_or(false)
}
