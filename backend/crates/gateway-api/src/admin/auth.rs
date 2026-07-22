//! 管理员登录、会话与管理请求鉴权接线。

use std::fmt;

use axum::{
    Extension, Json, Router,
    extract::{FromRequestParts, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use gateway_admin::{
    AdminServices,
    model::auth::{AdminPrincipal, AdminRequestContext, LoginCommand, LoginError},
};
use serde::{Deserialize, Serialize};

use super::{AdminEnvelope, AdminError, AdminResponse};

const REQUEST_ID_HEADER: &str = "x-request-id";
const ADMIN_SESSION_COOKIE: &str = "cpr_admin_session";
const ADMIN_SESSION_COOKIE_ATTRS: &str = "Path=/; Secure; HttpOnly; SameSite=Lax";

/// 所有管理 HTTP 模块从 state 消费同一个认证用例端口。
pub trait AdminSessionState {
    fn admin_services(&self) -> &AdminServices;
}

/// 已通过管理员会话或部署级管理 API Key 鉴权的请求。
pub struct AdminAuth {
    context: AdminRequestContext,
}

impl AdminAuth {
    #[must_use]
    pub const fn context(&self) -> &AdminRequestContext {
        &self.context
    }

    #[must_use]
    pub fn into_context(self) -> AdminRequestContext {
        self.context
    }
}

impl<S> FromRequestParts<S> for AdminAuth
where
    S: AdminSessionState + Send + Sync,
{
    type Rejection = AdminError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let principal = require_admin_auth(state, &parts.headers).await?;
        let request_id = parts
            .headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AdminError::internal("Missing admin request context"))?;
        Ok(Self {
            context: AdminRequestContext {
                principal,
                request_id: request_id.to_owned(),
            },
        })
    }
}

pub async fn require_admin_session<S>(state: &S, headers: &HeaderMap) -> Result<String, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    match state
        .admin_services()
        .auth()
        .resolve_admin_user_id(admin_session_cookie(headers).as_deref())
        .await
    {
        Ok(Some(admin_user_id)) => Ok(admin_user_id),
        Ok(None) => Err(AdminError::admin_session_required()),
        Err(_) => Err(AdminError::internal("Failed to validate admin session")),
    }
}

async fn require_admin_auth<S>(state: &S, headers: &HeaderMap) -> Result<AdminPrincipal, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    if let Some(api_key) = admin_api_key_header(headers) {
        return match state
            .admin_services()
            .auth()
            .verify_admin_api_key(&api_key)
            .await
        {
            Ok(true) => Ok(AdminPrincipal::ApiKey),
            Ok(false) => Err(AdminError::invalid_admin_api_key()),
            Err(_) => Err(AdminError::internal("Failed to validate admin API key")),
        };
    }

    require_admin_session(state, headers)
        .await
        .map(|admin_user_id| AdminPrincipal::Session { admin_user_id })
}

fn admin_api_key_header(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("x-api-key")?.to_str().ok()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// 管理员登录请求；密码不得进入 Debug。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminLoginRequest {
    username: Option<String>,
    password: String,
}

impl AdminLoginRequest {
    /// 取出认证用例所需字段。
    #[must_use]
    pub fn into_parts(self) -> (Option<String>, String) {
        (self.username, self.password)
    }
}

impl fmt::Debug for AdminLoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdminLoginRequest")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// 登录成功响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLoginData {
    expires_at: String,
}

impl AdminLoginData {
    #[must_use]
    pub fn new(expires_at: String) -> Self {
        Self { expires_at }
    }
}

/// 管理员会话状态响应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSessionStatusData {
    authenticated: bool,
}

impl AdminSessionStatusData {
    #[must_use]
    pub const fn new(authenticated: bool) -> Self {
        Self { authenticated }
    }
}

/// 登出成功响应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AdminLogoutData {
    message: &'static str,
}

impl AdminLogoutData {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            message: "Logged out successfully",
        }
    }
}

impl Default for AdminLogoutData {
    fn default() -> Self {
        Self::new()
    }
}

/// 请求上下文解析出的客户端登录来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLoginSource(String);

impl AdminLoginSource {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 构造固定 GET/POST 管理员认证路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/auth/login", post(login::<S>))
        .route("/api/admin/auth/status", get(session_status::<S>))
        .route("/api/admin/auth/logout", post(logout::<S>))
}

async fn login<S>(
    State(state): State<S>,
    source: Option<Extension<AdminLoginSource>>,
    Json(payload): Json<AdminLoginRequest>,
) -> Result<Response, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let source = source
        .as_ref()
        .map_or("unknown", |Extension(source)| source.as_str());
    let (username, password) = payload.into_parts();
    let session = state
        .admin_services()
        .auth()
        .login(LoginCommand {
            username,
            password,
            source: source.to_owned(),
        })
        .await
        .map_err(map_login_error)?;
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminLoginData::new(session.expires_at.to_rfc3339())),
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

async fn session_status<S>(
    State(state): State<S>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let authenticated = state
        .admin_services()
        .auth()
        .validate_session(admin_session_cookie(&headers).as_deref())
        .await
        .map_err(|_| AdminError::internal("Failed to validate admin session"))?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSessionStatusData::new(authenticated)),
    ))
}

async fn logout<S>(State(state): State<S>, headers: HeaderMap) -> Result<Response, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    if let Some(session_id) = admin_session_cookie(&headers) {
        let _ = state.admin_services().auth().logout(&session_id).await;
    }
    let mut response =
        AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(AdminLogoutData::new()))
            .into_response();
    let cookie = format!("{ADMIN_SESSION_COOKIE}=; {ADMIN_SESSION_COOKIE_ATTRS}; Max-Age=0");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|_| AdminError::internal("Failed to clear admin session cookie"))?,
    );
    Ok(response)
}

fn admin_session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get("cookie")?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == ADMIN_SESSION_COOKIE).then(|| value.to_owned())
    })
}

fn map_login_error(error: LoginError) -> AdminError {
    match error {
        LoginError::InvalidCredentials => AdminError::invalid_admin_credentials(),
        LoginError::Throttled => AdminError::too_many_login_attempts(),
        LoginError::Unavailable => AdminError::internal("Failed to create admin session"),
    }
}
