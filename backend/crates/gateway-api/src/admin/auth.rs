//! 管理员登录、会话、principal 与认证服务端口。

use std::{fmt, sync::Arc};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use async_trait::async_trait;
use axum::{
    Extension, Json, Router,
    extract::{FromRequestParts, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;

use super::{AdminEnvelope, AdminError, AdminResponse};

/// 管理端服务错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminServiceErrorKind {
    Invalid,
    NotFound,
    Conflict,
    Unavailable,
    Internal,
}

/// 管理端服务错误。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AdminServiceError {
    kind: AdminServiceErrorKind,
    message: String,
}

impl AdminServiceError {
    #[must_use]
    pub fn new(kind: AdminServiceErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(AdminServiceErrorKind::Invalid, message)
    }

    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(AdminServiceErrorKind::NotFound, message)
    }

    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(AdminServiceErrorKind::Conflict, message)
    }

    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(AdminServiceErrorKind::Unavailable, message)
    }

    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AdminServiceErrorKind::Internal, message)
    }

    #[must_use]
    pub fn kind(&self) -> AdminServiceErrorKind {
        self.kind
    }
}

/// 已认证的管理端主体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminPrincipal {
    Session { admin_user_id: String },
    ApiKey,
}

/// 传给应用服务的安全请求上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminRequestContext {
    principal: AdminPrincipal,
    request_id: String,
}

impl AdminRequestContext {
    #[must_use]
    pub fn new(principal: AdminPrincipal, request_id: impl Into<String>) -> Self {
        Self {
            principal,
            request_id: request_id.into(),
        }
    }

    #[must_use]
    pub fn principal(&self) -> &AdminPrincipal {
        &self.principal
    }

    #[must_use]
    pub fn admin_user_id(&self) -> Option<&str> {
        match &self.principal {
            AdminPrincipal::Session { admin_user_id } => Some(admin_user_id),
            AdminPrincipal::ApiKey => None,
        }
    }

    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub fn is_api_key(&self) -> bool {
        matches!(self.principal, AdminPrincipal::ApiKey)
    }
}

const REQUEST_ID_HEADER: &str = "x-request-id";

#[async_trait]
pub trait AdminSessionResolver: Send + Sync {
    async fn resolve_admin_user_id(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<String>, AdminServiceError>;

    async fn verify_admin_api_key(&self, _key: &str) -> Result<bool, AdminServiceError> {
        Ok(false)
    }
}

pub trait AdminSessionState {
    fn admin_session_resolver(&self) -> &dyn AdminSessionResolver;
}

/// 已通过管理员会话或部署级管理 API Key 鉴权的请求。
pub struct AdminAuth {
    context: AdminRequestContext,
}

impl AdminAuth {
    #[must_use]
    pub fn context(&self) -> &AdminRequestContext {
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
            context: AdminRequestContext::new(principal, request_id),
        })
    }
}

pub async fn require_admin_session<S>(state: &S, headers: &HeaderMap) -> Result<String, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    match state
        .admin_session_resolver()
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
            .admin_session_resolver()
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

const ADMIN_SESSION_COOKIE: &str = "cpr_admin_session";
const ADMIN_SESSION_COOKIE_ATTRS: &str = "Path=/; Secure; HttpOnly; SameSite=Lax";

/// 管理员登录请求；密码不得进入 `Debug`。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminLoginRequest {
    username: Option<String>,
    password: String,
}

impl AdminLoginRequest {
    /// 取出认证服务所需字段。
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
    /// 构造登录成功响应。
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
    /// 构造会话状态响应。
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
    /// 构造稳定登出响应。
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

/// 登录成功后由应用层返回的 session 事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLoginSession {
    pub session_id: String,
    pub expires_at: String,
}

/// 登录特有的低基数失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminLoginError {
    InvalidCredentials,
    Throttled,
    Unavailable,
}

/// 管理员认证应用端口。
#[async_trait]
pub trait AdminAuthService: Send + Sync {
    async fn login(
        &self,
        source: &str,
        username: Option<String>,
        password: String,
    ) -> Result<AdminLoginSession, AdminLoginError>;

    async fn validate(&self, session_id: Option<String>) -> Result<bool, AdminServiceError>;

    async fn logout(&self, session_id: String) -> Result<(), AdminServiceError>;
}

/// 认证 HTTP module 的最小 state。
pub trait AdminAuthState {
    fn admin_auth_service(&self) -> &dyn AdminAuthService;
}

/// 构造固定 GET/POST 管理员认证路由。
pub fn router<S>() -> Router<S>
where
    S: AdminAuthState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/login", post(login::<S>))
        .route("/api/admin/auth/status", get(session_status::<S>))
        .route("/api/admin/logout", post(logout::<S>))
}

async fn login<S>(
    State(state): State<S>,
    source: Option<Extension<AdminLoginSource>>,
    Json(payload): Json<AdminLoginRequest>,
) -> Result<Response, AdminError>
where
    S: AdminAuthState + Send + Sync,
{
    let source = source
        .as_ref()
        .map_or("unknown", |Extension(source)| source.as_str());
    let (username, password) = payload.into_parts();
    let session = state
        .admin_auth_service()
        .login(source, username, password)
        .await
        .map_err(|error| match error {
            AdminLoginError::InvalidCredentials => AdminError::invalid_admin_credentials(),
            AdminLoginError::Throttled => AdminError::too_many_login_attempts(),
            AdminLoginError::Unavailable => AdminError::internal("Failed to create admin session"),
        })?;
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminLoginData::new(session.expires_at)),
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
    S: AdminAuthState + Send + Sync,
{
    let authenticated = state
        .admin_auth_service()
        .validate(admin_session_cookie(&headers))
        .await
        .map_err(|_| AdminError::internal("Failed to validate admin session"))?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSessionStatusData::new(authenticated)),
    ))
}

async fn logout<S>(State(state): State<S>, headers: HeaderMap) -> Result<Response, AdminError>
where
    S: AdminAuthState + Send + Sync,
{
    if let Some(session_id) = admin_session_cookie(&headers) {
        let _ = state.admin_auth_service().logout(session_id).await;
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

const LOGIN_FAILURE_LIMIT: u32 = 5;
const LOGIN_FAILURE_WINDOW_SECONDS: u64 = 15 * 60;

/// 认证领域需要的外层持久端口；API crate 不知道 PostgreSQL 或 runtime settings。
#[async_trait]
pub trait AdminAuthBackend: Send + Sync {
    async fn password_hash(&self, admin_user_id: &str)
    -> Result<Option<String>, AdminServiceError>;
    async fn store_password_hash(
        &self,
        admin_user_id: &str,
        password_hash: &str,
    ) -> Result<(), AdminServiceError>;
    async fn admin_api_key(&self) -> Result<Option<String>, AdminServiceError>;
    async fn load_admin_session(
        &self,
        session_id: &str,
    ) -> Result<Option<AdminBackendSession>, AdminServiceError>;
    async fn store_admin_session(
        &self,
        session_id: &str,
        session: &AdminBackendSession,
    ) -> Result<(), AdminServiceError>;
    async fn delete_admin_session(
        &self,
        session_id: &str,
    ) -> Result<Option<AdminBackendSession>, AdminServiceError>;
    async fn login_source_is_throttled(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> Result<bool, AdminServiceError>;
    async fn record_login_failure(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> Result<bool, AdminServiceError>;
    async fn clear_login_failures(&self, source: &str) -> Result<(), AdminServiceError>;
    async fn append_auth_audit(&self, event: AdminAuthAuditEvent) -> Result<(), AdminServiceError>;
}

/// 登录与登出的最小安全审计事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAuthAuditEvent {
    pub admin_user_id: String,
    pub action: &'static str,
    pub occurred_at: DateTime<Utc>,
}

/// Redis session owner 与认证领域交换的非秘密会话事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminBackendSession {
    pub admin_user_id: String,
    pub expires_at: DateTime<Utc>,
}

/// 冻结管理端使用的默认认证领域实现。
pub struct DefaultAdminAuthService {
    default_admin_user_id: String,
    session_ttl: Duration,
    backend: Arc<dyn AdminAuthBackend>,
}

impl DefaultAdminAuthService {
    #[must_use]
    pub fn new(
        default_admin_user_id: String,
        session_ttl_minutes: u64,
        backend: Arc<dyn AdminAuthBackend>,
    ) -> Self {
        let minutes = i64::try_from(session_ttl_minutes).unwrap_or(i64::MAX);
        Self {
            default_admin_user_id,
            session_ttl: Duration::minutes(minutes.max(1)),
            backend,
        }
    }

    /// 空库第一次启动时创建唯一默认管理员；已有用户绝不覆盖密码。
    pub async fn ensure_default_admin(&self, password: &str) -> Result<bool, AdminServiceError> {
        if self
            .backend
            .password_hash(&self.default_admin_user_id)
            .await?
            .is_some()
        {
            return Ok(false);
        }
        let hash = hash_admin_password(password)?;
        self.backend
            .store_password_hash(&self.default_admin_user_id, &hash)
            .await?;
        Ok(true)
    }
}

#[async_trait]
impl AdminSessionResolver for DefaultAdminAuthService {
    async fn resolve_admin_user_id(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<String>, AdminServiceError> {
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        Ok(self
            .backend
            .load_admin_session(session_id)
            .await?
            .filter(|session| session.expires_at > Utc::now())
            .map(|session| session.admin_user_id))
    }

    async fn verify_admin_api_key(&self, key: &str) -> Result<bool, AdminServiceError> {
        if !valid_admin_api_key_shape(key) {
            return Ok(false);
        }
        let stored = self.backend.admin_api_key().await?;
        Ok(stored.as_deref().is_some_and(|stored| {
            key.len() == stored.len() && bool::from(key.as_bytes().ct_eq(stored.as_bytes()))
        }))
    }
}

#[async_trait]
impl AdminAuthService for DefaultAdminAuthService {
    async fn login(
        &self,
        source: &str,
        username: Option<String>,
        password: String,
    ) -> Result<AdminLoginSession, AdminLoginError> {
        let source = normalized_login_source(source);
        if self
            .backend
            .login_source_is_throttled(source, LOGIN_FAILURE_LIMIT, LOGIN_FAILURE_WINDOW_SECONDS)
            .await
            .map_err(|_| AdminLoginError::Unavailable)?
        {
            return Err(AdminLoginError::Throttled);
        }
        if username.as_deref().unwrap_or(&self.default_admin_user_id) != self.default_admin_user_id
        {
            let throttled = self
                .backend
                .record_login_failure(source, LOGIN_FAILURE_LIMIT, LOGIN_FAILURE_WINDOW_SECONDS)
                .await
                .map_err(|_| AdminLoginError::Unavailable)?;
            return Err(if throttled {
                AdminLoginError::Throttled
            } else {
                AdminLoginError::InvalidCredentials
            });
        }
        let hash = self
            .backend
            .password_hash(&self.default_admin_user_id)
            .await
            .map_err(|_| AdminLoginError::Unavailable)?
            .ok_or(AdminLoginError::InvalidCredentials)?;
        if !verify_admin_password(&password, &hash).map_err(|_| AdminLoginError::Unavailable)? {
            let throttled = self
                .backend
                .record_login_failure(source, LOGIN_FAILURE_LIMIT, LOGIN_FAILURE_WINDOW_SECONDS)
                .await
                .map_err(|_| AdminLoginError::Unavailable)?;
            return Err(if throttled {
                AdminLoginError::Throttled
            } else {
                AdminLoginError::InvalidCredentials
            });
        }

        self.backend
            .clear_login_failures(source)
            .await
            .map_err(|_| AdminLoginError::Unavailable)?;
        let session_id = random_session_token();
        let expires_at = Utc::now() + self.session_ttl;
        self.backend
            .store_admin_session(
                &session_id,
                &AdminBackendSession {
                    admin_user_id: self.default_admin_user_id.clone(),
                    expires_at,
                },
            )
            .await
            .map_err(|_| AdminLoginError::Unavailable)?;
        if self
            .backend
            .append_auth_audit(AdminAuthAuditEvent {
                admin_user_id: self.default_admin_user_id.clone(),
                action: "admin.login",
                occurred_at: Utc::now(),
            })
            .await
            .is_err()
        {
            let _ = self.backend.delete_admin_session(&session_id).await;
            return Err(AdminLoginError::Unavailable);
        }
        Ok(AdminLoginSession {
            session_id,
            expires_at: expires_at.to_rfc3339(),
        })
    }

    async fn validate(&self, session_id: Option<String>) -> Result<bool, AdminServiceError> {
        let Some(session_id) = session_id else {
            return Ok(false);
        };
        Ok(self
            .backend
            .load_admin_session(&session_id)
            .await?
            .is_some_and(|session| session.expires_at > Utc::now()))
    }

    async fn logout(&self, session_id: String) -> Result<(), AdminServiceError> {
        let session = self.backend.delete_admin_session(&session_id).await?;
        if let Some(session) = session {
            self.backend
                .append_auth_audit(AdminAuthAuditEvent {
                    admin_user_id: session.admin_user_id,
                    action: "admin.logout",
                    occurred_at: Utc::now(),
                })
                .await?;
        }
        Ok(())
    }
}

fn hash_admin_password(password: &str) -> Result<String, AdminServiceError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AdminServiceError::internal("Failed to hash admin password"))
}

fn verify_admin_password(password: &str, encoded: &str) -> Result<bool, AdminServiceError> {
    let hash = PasswordHash::new(encoded)
        .map_err(|_| AdminServiceError::internal("Stored admin password hash is invalid"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .is_ok())
}

fn random_session_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("session_{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn valid_admin_api_key_shape(value: &str) -> bool {
    value.len() == 70
        && value.starts_with("admin-")
        && value[6..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn normalized_login_source(source: &str) -> &str {
    let source = source.trim();
    if source.is_empty() { "unknown" } else { source }
}
