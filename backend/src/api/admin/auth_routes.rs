//! 管理端认证路由。

use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::{
    api::{
        AppState,
        admin::{
            response::{AdminEnvelope, AdminError, AdminResponse},
            session::{ADMIN_SESSION_COOKIE, admin_session_cookie},
        },
        middleware::request_id::ClientIp,
    },
    auth::types::SessionError,
    infra::time::china_rfc3339,
};

const ADMIN_SESSION_COOKIE_ATTRS: &str = "Path=/; Secure; HttpOnly; SameSite=Lax";

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
        Err(SessionError::LoginThrottled) => return Err(AdminError::too_many_login_attempts()),
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
    if let Some(session_id) = admin_session_cookie(&headers)
        && let Err(error) = state
            .services
            .admin_sessions
            .delete_session(&session_id)
            .await
    {
        tracing::warn!(error = %error, "Failed to revoke admin session during logout");
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

fn optional_client_ip(client_ip: Option<Extension<ClientIp>>) -> Option<String> {
    let Extension(client_ip) = client_ip?;
    Some(client_ip.as_str().to_string())
}

fn admin_login_source(fallback_client_ip: Option<String>) -> String {
    fallback_client_ip.unwrap_or_else(|| "unknown".to_string())
}
