//! 管理端会话处理器。

use axum::{
    extract::State,
    http::{header::SET_COOKIE, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    infra::time::china_rfc3339,
    runtime::state::AppState,
};

const ADMIN_SESSION_COOKIE: &str = "cpr_admin_session";

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
    Json(payload): Json<AdminLoginRequest>,
) -> Result<Response, AdminError> {
    let session = state
        .services
        .admin_sessions
        .login(payload.username.as_deref(), payload.password.as_str())
        .await
        .map_err(|_| {
            AdminError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                50001,
                "Failed to create admin session",
            )
        })?
        .ok_or_else(|| {
            AdminError::new(StatusCode::UNAUTHORIZED, 40102, "Invalid admin credentials")
        })?;

    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminLoginData {
            expires_at: china_rfc3339(&session.expires_at),
        }),
    )
    .into_response();
    let cookie = format!(
        "{ADMIN_SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Lax",
        session.session_id
    );
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| {
            AdminError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                50001,
                "Failed to create admin session cookie",
            )
        })?,
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
        .map_err(|_| {
            AdminError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                50001,
                "Failed to validate admin session",
            )
        })?;

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

    let cookie = format!("{ADMIN_SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| {
            AdminError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                50001,
                "Failed to clear admin session cookie",
            )
        })?,
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
        Ok(false) => Err(AdminError::new(
            StatusCode::UNAUTHORIZED,
            40101,
            "Admin session required",
        )),
        Err(_) => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to validate admin session",
        )),
    }
}

fn admin_session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get("cookie")?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == ADMIN_SESSION_COOKIE).then(|| value.to_string())
    })
}
