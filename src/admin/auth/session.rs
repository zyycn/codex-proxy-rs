//! 管理端会话处理器。

use axum::{
    extract::State,
    http::{header::SET_COOKIE, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    http::middleware::request_id::RequestId,
    runtime::state::AppState,
};

const ADMIN_SESSION_COOKIE: &str = "cpr_admin_session";

/// 管理员登录请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLoginRequest {
    /// 管理员用户名；缺省时使用配置中的默认管理员。
    pub username: Option<String>,
    /// 管理员密码。
    pub password: String,
}

/// 管理员登录响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLoginData {
    /// 会话过期时间。
    pub expires_at: String,
}

/// 管理员会话状态响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSessionStatusData {
    /// 当前请求是否携带有效管理端会话。
    pub authenticated: bool,
}

/// 会话登录是否成功。
pub fn session_login_allowed() -> bool {
    true
}

/// `POST /api/admin/login`
pub async fn login(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<AdminLoginRequest>,
) -> Result<Response, AdminError> {
    let request_id = request_id.as_str().to_string();
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
                request_id.clone(),
            )
        })?
        .ok_or_else(|| {
            AdminError::new(
                StatusCode::UNAUTHORIZED,
                40102,
                "Invalid admin credentials",
                request_id.clone(),
            )
        })?;

    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AdminLoginData {
                expires_at: session.expires_at.to_rfc3339(),
            },
            request_id.clone(),
        ),
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
                request_id.clone(),
            )
        })?,
    );
    Ok(response)
}

/// `GET /api/admin/auth/status`
pub async fn session_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
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
                request_id.clone(),
            )
        })?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSessionStatusData { authenticated }, request_id),
    ))
}

/// `POST /api/admin/logout`
pub async fn logout(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<Response, AdminError> {
    let request_id = request_id.as_str().to_string();

    // 获取并删除服务器端 session
    if let Some(session_id) = admin_session_cookie(&headers) {
        let _ = state
            .services
            .admin_sessions
            .delete_session(&session_id)
            .await;
    }

    // 返回 JSON 响应并清除 cookie
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            serde_json::json!({
                "message": "Logged out successfully"
            }),
            request_id.clone(),
        ),
    )
    .into_response();

    // 清除 cookie
    let cookie = format!("{ADMIN_SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| {
            AdminError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                50001,
                "Failed to clear admin session cookie",
                request_id.clone(),
            )
        })?,
    );

    Ok(response)
}

/// 要求请求携带有效管理员会话。
pub async fn require_admin_session(
    state: &AppState,
    headers: &HeaderMap,
    request_id: &str,
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
            request_id,
        )),
        Err(_) => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to validate admin session",
            request_id,
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
