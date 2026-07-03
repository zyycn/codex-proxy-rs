//! 管理端会话处理器。

use axum::{
    extract::{Extension, FromRequestParts, State},
    http::{header::SET_COOKIE, request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    admin::{
        auth::service::AdminSessionError,
        monitoring::usage_record_model::{UsageRecord, UsageRecordLevel},
        response::{AdminEnvelope, AdminError, AdminResponse},
    },
    http::middleware::request_id::RequestId,
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
    request_id: Option<Extension<RequestId>>,
    headers: HeaderMap,
    Json(payload): Json<AdminLoginRequest>,
) -> Result<Response, AdminError> {
    let source = admin_login_source(&headers);
    let request_id = request_id.map(|Extension(request_id)| request_id.as_str().to_string());
    let username_provided = payload
        .username
        .as_deref()
        .is_some_and(|username| !username.trim().is_empty());
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
        Ok(Some(session)) => {
            record_admin_login_audit(
                &state,
                request_id.as_deref(),
                &source,
                username_provided,
                AdminLoginAuditOutcome::Success,
            )
            .await;
            session
        }
        Ok(None) => {
            record_admin_login_audit(
                &state,
                request_id.as_deref(),
                &source,
                username_provided,
                AdminLoginAuditOutcome::InvalidCredentials,
            )
            .await;
            return Err(AdminError::invalid_admin_credentials());
        }
        Err(AdminSessionError::LoginThrottled) => {
            record_admin_login_audit(
                &state,
                request_id.as_deref(),
                &source,
                username_provided,
                AdminLoginAuditOutcome::Throttled,
            )
            .await;
            return Err(AdminError::too_many_login_attempts());
        }
        Err(_) => {
            record_admin_login_audit(
                &state,
                request_id.as_deref(),
                &source,
                username_provided,
                AdminLoginAuditOutcome::InternalError,
            )
            .await;
            return Err(AdminError::internal("Failed to create admin session"));
        }
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

enum AdminLoginAuditOutcome {
    Success,
    InvalidCredentials,
    Throttled,
    InternalError,
}

impl AdminLoginAuditOutcome {
    fn level(&self) -> UsageRecordLevel {
        match self {
            Self::Success => UsageRecordLevel::Info,
            Self::InvalidCredentials | Self::Throttled => UsageRecordLevel::Warn,
            Self::InternalError => UsageRecordLevel::Error,
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::Success => "Admin login succeeded",
            Self::InvalidCredentials => "Admin login failed",
            Self::Throttled => "Admin login throttled",
            Self::InternalError => "Admin login failed internally",
        }
    }

    fn status_code(&self) -> i64 {
        match self {
            Self::Success => i64::from(StatusCode::OK.as_u16()),
            Self::InvalidCredentials => i64::from(StatusCode::UNAUTHORIZED.as_u16()),
            Self::Throttled => i64::from(StatusCode::TOO_MANY_REQUESTS.as_u16()),
            Self::InternalError => i64::from(StatusCode::INTERNAL_SERVER_ERROR.as_u16()),
        }
    }

    fn failure_class(&self) -> Option<&'static str> {
        match self {
            Self::Success => None,
            Self::InvalidCredentials => Some("invalid_credentials"),
            Self::Throttled => Some("login_throttled"),
            Self::InternalError => Some("session_error"),
        }
    }
}

async fn record_admin_login_audit(
    state: &AppState,
    request_id: Option<&str>,
    source: &str,
    username_provided: bool,
    outcome: AdminLoginAuditOutcome,
) {
    let mut record = UsageRecord::new("admin_auth", outcome.level(), outcome.message());
    record.request_id = request_id.map(ToString::to_string);
    record.route = Some("/api/admin/login".to_string());
    record.status_code = Some(outcome.status_code());
    record.failure_class = outcome.failure_class().map(ToString::to_string);
    record.metadata = json!({
        "source": source,
        "usernameProvided": username_provided,
    });

    if let Err(error) = state.services.usage_records.record_audit(record).await {
        tracing::warn!(%error, "failed to record admin login audit event");
    }
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

fn admin_login_source(headers: &HeaderMap) -> String {
    headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string()
}
