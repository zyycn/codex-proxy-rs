//! 控制面统一登录、会话恢复与退出；身份由认证用例返回

use std::{fmt, net::SocketAddr};

use axum::{
    Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use gateway_admin::{
    AdminServices,
    model::auth::{AuthSession, ChangePassword, LoginCommand, LoginError, SessionSubject},
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::{AdminEnvelope, AdminError, AdminJson, AdminResponse, wire::map_admin_service_error},
    session_cookie,
};

/// 控制面 HTTP adapter 消费同一组用例；权限由各入口服务端校验
pub trait SessionState {
    fn admin_services(&self) -> &AdminServices;
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
pub enum LoginRequest {
    Admin {
        username: Option<String>,
        password: String,
    },
    Key {
        #[serde(rename = "apiKey")]
        api_key: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
}

impl fmt::Debug for ChangePasswordRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChangePasswordRequest([REDACTED])")
    }
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Admin { username, .. } => formatter
                .debug_struct("AdminLogin")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::Key { .. } => formatter
                .debug_struct("KeyLogin")
                .field("api_key", &"[REDACTED]")
                .finish(),
        }
    }
}

impl From<LoginRequest> for LoginCommand {
    fn from(request: LoginRequest) -> Self {
        match request {
            LoginRequest::Admin { username, password } => Self::Admin { username, password },
            LoginRequest::Key { api_key } => Self::Key { api_key },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionData {
    role: &'static str,
    expires_at: String,
}

impl From<&AuthSession> for SessionData {
    fn from(session: &AuthSession) -> Self {
        Self {
            role: match session.subject {
                SessionSubject::Admin { .. } => "admin",
                SessionSubject::Key { .. } => "key",
            },
            expires_at: session.expires_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
struct SessionStatusData {
    authenticated: bool,
    session: Option<SessionData>,
}

#[derive(Debug, Serialize)]
struct LogoutData {
    message: &'static str,
}

pub(crate) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/auth/login", post(login::<S>))
        .route("/api/auth/status", get(session_status::<S>))
        .route("/api/auth/refresh", post(refresh_session::<S>))
        .route("/api/auth/logout", post(logout::<S>))
        .route("/api/auth/password", post(change_password::<S>))
        .route("/api/auth", any(not_found))
        .route("/api/auth/{*path}", any(not_found))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::map_response(no_store))
}

async fn login<S>(
    State(state): State<S>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AdminJson(payload): AdminJson<LoginRequest>,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .auth()
        .login(
            payload.into(),
            peer.ip(),
            session_cookie::refresh_value(&headers)
                .or_else(|| session_cookie::value(&headers))
                .as_deref(),
        )
        .await
        .map_err(map_login_error)?;
    set_session_cookie(
        AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(SessionData::from(&result.session)),
        )
        .into_response(),
        &headers,
        &result,
    )
}

async fn session_status<S>(
    State(state): State<S>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let session = state
        .admin_services()
        .auth()
        .session(session_cookie::value(&headers).as_deref())
        .await
        .map_err(map_admin_service_error)?;
    Ok(session_response(session.as_ref()))
}

async fn refresh_session<S>(
    State(state): State<S>,
    headers: HeaderMap,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    let grant = state
        .admin_services()
        .auth()
        .renew_session(
            session_cookie::refresh_value(&headers).as_deref(),
            session_cookie::value(&headers).as_deref(),
        )
        .await
        .map_err(map_admin_service_error)?;
    let response = session_response(grant.as_ref().map(|grant| &grant.session));
    if let Some(grant) = grant {
        return set_session_cookie(response, &headers, &grant);
    }
    // 失效响应不清 Cookie，避免晚到的请求覆盖另一个标签页刚建立的会话
    Ok(response)
}

fn session_response(session: Option<&AuthSession>) -> Response {
    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(SessionStatusData {
            authenticated: session.is_some(),
            session: session.map(SessionData::from),
        }),
    )
    .into_response()
}

fn set_session_cookie(
    mut response: Response,
    headers: &HeaderMap,
    grant: &gateway_admin::model::auth::SessionGrant,
) -> Result<Response, AdminError> {
    append_cookie(
        &mut response,
        headers,
        session_cookie::NAME,
        &grant.session_id,
        Some(grant.session.expires_at),
    )?;
    append_cookie(
        &mut response,
        headers,
        session_cookie::REFRESH_NAME,
        grant.refresh_token.as_deref().unwrap_or_default(),
        grant
            .session
            .renewal
            .as_ref()
            .map(|renewal| renewal.expires_at),
    )?;
    Ok(response)
}

fn append_cookie(
    response: &mut Response,
    headers: &HeaderMap,
    name: &str,
    value: &str,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(), AdminError> {
    let attributes = session_cookie::attributes(headers);
    let path = if name == session_cookie::REFRESH_NAME {
        "/api/auth"
    } else {
        "/"
    };
    let lifetime = expires_at.map_or_else(
        || "Max-Age=0".to_owned(),
        |expiry| {
            format!(
                "Max-Age={}; Expires={}",
                expiry
                    .signed_duration_since(chrono::Utc::now())
                    .num_seconds()
                    .max(1),
                expiry.format("%a, %d %b %Y %H:%M:%S GMT")
            )
        },
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{name}={value}; Path={path}; {attributes}; {lifetime}"
        ))
        .map_err(|_| AdminError::internal())?,
    );
    Ok(())
}

async fn logout<S>(State(state): State<S>, headers: HeaderMap) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    if let Some(session_id) =
        session_cookie::refresh_value(&headers).or_else(|| session_cookie::value(&headers))
    {
        state
            .admin_services()
            .auth()
            .logout(&session_id)
            .await
            .map_err(map_admin_service_error)?;
    }
    let response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(LogoutData {
            message: "Logged out successfully",
        }),
    )
    .into_response();
    clear_session_cookie(response, &headers)
}

async fn change_password<S>(
    State(state): State<S>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AdminJson(payload): AdminJson<ChangePasswordRequest>,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    state
        .admin_services()
        .auth()
        .change_password(
            session_cookie::value(&headers).as_deref(),
            ChangePassword {
                current_password: payload.current_password,
                new_password: payload.new_password,
            },
            peer.ip(),
        )
        .await
        .map_err(map_admin_service_error)?;
    let response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(LogoutData {
            message: "密码已修改，请重新登录",
        }),
    )
    .into_response();
    clear_session_cookie(response, &headers)
}

fn clear_session_cookie(
    mut response: Response,
    headers: &HeaderMap,
) -> Result<Response, AdminError> {
    for name in [session_cookie::NAME, session_cookie::REFRESH_NAME] {
        append_cookie(&mut response, headers, name, "", None)?;
    }
    Ok(response)
}

fn map_login_error(error: LoginError) -> AdminError {
    match error {
        LoginError::InvalidCredentials => AdminError::invalid_credentials(),
        LoginError::TooManyAttempts {
            retry_after_seconds,
        } => AdminError::too_many_login_attempts().with_retry_after(retry_after_seconds),
        LoginError::Unavailable => AdminError::service_unavailable(),
    }
}

async fn method_not_allowed() -> AdminError {
    AdminError::method_not_allowed()
}
async fn not_found() -> AdminError {
    AdminError::not_found("认证接口不存在")
}
async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
