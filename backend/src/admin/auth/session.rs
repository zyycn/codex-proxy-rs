//! 管理端会话处理器。

use axum::{
    extract::{FromRequestParts, State},
    http::{header::SET_COOKIE, request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::{
        auth::service::AdminSessionError,
        response::{AdminEnvelope, AdminError, AdminResponse},
    },
    http::middleware::request_id::ClientIp,
    infra::time::china_rfc3339,
    runtime::state::AppState,
};

const ADMIN_SESSION_COOKIE: &str = "cpr_admin_session";
const ADMIN_SESSION_COOKIE_ATTRS: &str = "Path=/; Secure; HttpOnly; SameSite=Lax";

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminLoginRequest {
    username: Option<String>,
    password: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminLoginData {
    expires_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminSessionStatusData {
    authenticated: bool,
}

/// `POST /api/admin/login`
pub(crate) async fn login(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    Json(payload): Json<AdminLoginRequest>,
) -> Result<Response, AdminError> {
    let source = admin_login_source(optional_client_ip(client_ip));
    let login_result = state
        .services
        .admin_sessions
        .login(
            &source,
            payload.username.as_deref(),
            payload.password.as_str(),
        )
        .await;
    let session = match login_result {
        Ok(Some(session)) => session,
        Ok(None) => return Err(AdminError::invalid_admin_credentials()),
        Err(AdminSessionError::LoginThrottled) => return Err(AdminError::too_many_login_attempts()),
        Err(_) => return Err(AdminError::internal("Failed to create admin session")),
    };

    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminLoginData {
            expires_at: china_rfc3339(&session.expires_at),
        }),
    )
    .into_response();
    let cookie = format!(
        "{ADMIN_SESSION_COOKIE}={}; {ADMIN_SESSION_COOKIE_ATTRS}",
        session.session_id
    );
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|_| AdminError::internal("Failed to create admin session cookie"))?,
    );
    Ok(response)
}

/// `GET /api/admin/auth/status`
pub(crate) async fn session_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let authenticated = state
        .services
        .admin_sessions
        .validate(admin_session_cookie(&headers).as_deref())
        .await
        .map_err(|_| AdminError::internal("Failed to validate admin session"))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSessionStatusData { authenticated }),
    ))
}

/// `POST /api/admin/logout`
pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AdminError> {
    if let Some(session_id) = admin_session_cookie(&headers) {
        let _ = state
            .services
            .admin_sessions
            .delete_session(&session_id)
            .await;
    }

    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "message": "Logged out successfully"
        })),
    )
    .into_response();

    let cookie = format!("{ADMIN_SESSION_COOKIE}=; {ADMIN_SESSION_COOKIE_ATTRS}; Max-Age=0");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|_| AdminError::internal("Failed to clear admin session cookie"))?,
    );

    Ok(response)
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

fn admin_session_cookie(headers: &HeaderMap) -> Option<String> {
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

fn optional_client_ip(client_ip: Option<Extension<ClientIp>>) -> Option<String> {
    let Extension(client_ip) = client_ip?;
    Some(client_ip.as_str().to_string())
}

fn admin_login_source(fallback_client_ip: Option<String>) -> String {
    fallback_client_ip.unwrap_or_else(|| "unknown".to_string())
}
