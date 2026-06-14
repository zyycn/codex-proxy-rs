use axum::{
    extract::State,
    http::{
        header::{HeaderValue, SET_COOKIE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::session::service::{
        AdminAuthPoolSummary as ServiceAdminAuthPoolSummary, AdminAuthServiceError,
        AdminAuthStatus as ServiceAdminAuthStatus, AdminAuthUser as ServiceAdminAuthUser,
        AdminSessionValidationError,
    },
    platform::http::{auth::admin_session_id, request_id::RequestId},
    runtime::state::AppState,
};

use super::{account_status_value, AdminEnvelope, AdminError, AdminResponse};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginData {
    pub expires_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthStatusData {
    pub authenticated: bool,
    pub user: Option<AdminAuthUserData>,
    pub pool: AdminAuthPoolSummaryData,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthUserData {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub status: String,
    pub access_token_expires_at: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthPoolSummaryData {
    pub total: usize,
    pub active: usize,
    pub expired: usize,
    pub quota_exhausted: usize,
    pub refreshing: usize,
    pub disabled: usize,
    pub banned: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthLogoutData {
    pub success: bool,
    pub deleted: u64,
}

fn auth_status_data(status: ServiceAdminAuthStatus) -> AdminAuthStatusData {
    AdminAuthStatusData {
        authenticated: status.authenticated,
        user: status.user.map(auth_user_data),
        pool: auth_pool_summary_data(status.pool),
    }
}

fn auth_user_data(user: ServiceAdminAuthUser) -> AdminAuthUserData {
    AdminAuthUserData {
        id: user.id,
        email: user.email,
        account_id: user.account_id,
        user_id: user.user_id,
        label: user.label,
        plan_type: user.plan_type,
        status: account_status_value(user.status).to_string(),
        access_token_expires_at: user.access_token_expires_at.map(|value| value.to_rfc3339()),
    }
}

fn auth_pool_summary_data(summary: ServiceAdminAuthPoolSummary) -> AdminAuthPoolSummaryData {
    AdminAuthPoolSummaryData {
        total: summary.total,
        active: summary.active,
        expired: summary.expired,
        quota_exhausted: summary.quota_exhausted,
        refreshing: summary.refreshing,
        disabled: summary.disabled,
        banned: summary.banned,
    }
}

fn admin_session_set_cookie(session_id: &str, ttl_minutes: u64) -> Option<HeaderValue> {
    let max_age = ttl_minutes.checked_mul(60)?;
    let cookie = format!(
        "cpr_admin_session={session_id}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}"
    );
    HeaderValue::from_str(&cookie).ok()
}

pub(super) async fn require_admin_session(
    state: &AppState,
    headers: &HeaderMap,
    request_id: &str,
) -> Result<(), AdminError> {
    match state
        .services
        .admin_auth
        .validate_session(admin_session_id(headers))
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(AdminError::new(
            StatusCode::UNAUTHORIZED,
            40101,
            "Admin session required",
            request_id,
        )),
        Err(AdminSessionValidationError::DatabaseUnavailable) => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Database is not initialized",
            request_id,
        )),
        Err(AdminSessionValidationError::ValidateSession) => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to validate admin session",
            request_id,
        )),
    }
}

fn login_error(error: AdminAuthServiceError, request_id: &str) -> AdminError {
    match error {
        AdminAuthServiceError::DatabaseUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Database is not initialized",
            request_id,
        ),
        AdminAuthServiceError::AdminPasswordInvalid => AdminError::new(
            StatusCode::UNAUTHORIZED,
            40102,
            "Admin password invalid",
            request_id,
        ),
        AdminAuthServiceError::LoadAdminUser => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load admin user",
            request_id,
        ),
        AdminAuthServiceError::VerifyAdminPassword => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to verify admin password",
            request_id,
        ),
        AdminAuthServiceError::InvalidSessionTtl => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Admin session ttl is invalid",
            request_id,
        ),
        AdminAuthServiceError::CreateSession => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to create admin session",
            request_id,
        ),
        AdminAuthServiceError::AccountRepositoryUnavailable
        | AdminAuthServiceError::InspectAccountAuthStatus
        | AdminAuthServiceError::ClearAccounts => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to create admin session",
            request_id,
        ),
    }
}

fn auth_status_error(error: AdminAuthServiceError, request_id: &str) -> AdminError {
    match error {
        AdminAuthServiceError::AccountRepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        AdminAuthServiceError::InspectAccountAuthStatus => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to inspect account auth status",
            request_id,
        ),
        AdminAuthServiceError::DatabaseUnavailable
        | AdminAuthServiceError::LoadAdminUser
        | AdminAuthServiceError::AdminPasswordInvalid
        | AdminAuthServiceError::VerifyAdminPassword
        | AdminAuthServiceError::InvalidSessionTtl
        | AdminAuthServiceError::CreateSession
        | AdminAuthServiceError::ClearAccounts => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to inspect account auth status",
            request_id,
        ),
    }
}

fn logout_error(error: AdminAuthServiceError, request_id: &str) -> AdminError {
    match error {
        AdminAuthServiceError::AccountRepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        AdminAuthServiceError::ClearAccounts => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to clear accounts",
            request_id,
        ),
        AdminAuthServiceError::DatabaseUnavailable
        | AdminAuthServiceError::LoadAdminUser
        | AdminAuthServiceError::AdminPasswordInvalid
        | AdminAuthServiceError::VerifyAdminPassword
        | AdminAuthServiceError::InvalidSessionTtl
        | AdminAuthServiceError::CreateSession
        | AdminAuthServiceError::InspectAccountAuthStatus => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to clear accounts",
            request_id,
        ),
    }
}

pub async fn login(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<LoginRequest>,
) -> Result<Response, AdminError> {
    let request_id = request_id.as_str().to_string();
    let login = state
        .services
        .admin_auth
        .login(&payload.password)
        .await
        .map_err(|error| login_error(error, &request_id))?;

    let Some(cookie) = admin_session_set_cookie(&login.session_id, login.ttl_minutes) else {
        return Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to create admin session cookie",
            request_id,
        ));
    };
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            LoginData {
                expires_at: login.expires_at.to_rfc3339(),
            },
            request_id,
        ),
    )
    .into_response();
    response.headers_mut().insert(SET_COOKIE, cookie);
    Ok(response)
}

pub async fn auth_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let status = state
        .services
        .admin_auth
        .status()
        .await
        .map_err(|error| auth_status_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(auth_status_data(status), request_id),
    ))
}

pub async fn auth_logout(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let logout = state
        .services
        .admin_auth
        .logout()
        .await
        .map_err(|error| logout_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AdminAuthLogoutData {
                success: true,
                deleted: logout.deleted,
            },
            request_id,
        ),
    ))
}
