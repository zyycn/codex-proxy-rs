//! 管理端认证提取器与会话鉴权。

use axum::{
    extract::FromRequestParts,
    http::{request::Parts, HeaderMap},
};

use crate::api::{admin::response::AdminError, AppState};

pub(super) const ADMIN_SESSION_COOKIE: &str = "cpr_admin_session";

/// 已通过管理员会话或管理员 API Key 鉴权的请求。
pub(crate) struct AdminAuth;

impl FromRequestParts<AppState> for AdminAuth {
    type Rejection = AdminError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        require_admin_auth(state, &parts.headers).await?;
        Ok(Self)
    }
}

/// 要求请求携带有效管理员会话。
pub(crate) async fn require_admin_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), AdminError> {
    match state
        .services
        .admin_sessions
        .validate(admin_session_cookie(headers).as_deref())
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(AdminError::admin_session_required()),
        Err(_) => Err(AdminError::internal("Failed to validate admin session")),
    }
}

/// 要求请求携带有效管理员凭证：会话 Cookie 或管理员 API Key。
pub(crate) async fn require_admin_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), AdminError> {
    if let Some(api_key) = admin_api_key_header(headers) {
        return match state.services.settings.verify_admin_api_key(&api_key).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(AdminError::invalid_admin_api_key()),
            Err(_) => Err(AdminError::internal("Failed to validate admin API key")),
        };
    }

    require_admin_session(state, headers).await
}

pub(super) fn admin_session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get("cookie")?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == ADMIN_SESSION_COOKIE).then(|| value.to_string())
    })
}

fn admin_api_key_header(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("x-api-key")?.to_str().ok()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}
