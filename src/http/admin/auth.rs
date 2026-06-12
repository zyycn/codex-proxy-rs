use axum::{
    extract::{Path, Query, State},
    http::{
        header::{HeaderValue, HOST, LOCATION, SET_COOKIE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Extension, Json,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::{
    app::state::AppState,
    codex::oauth::{DeviceCode, OAuthError},
    config::AppConfig,
    http::auth::admin_session_id,
    http::middleware::RequestId,
    service::admin_auth::{
        AdminAuthOAuthError, AdminAuthPkceExchange,
        AdminAuthPoolSummary as ServiceAdminAuthPoolSummary, AdminAuthServiceError,
        AdminAuthStatus as ServiceAdminAuthStatus, AdminAuthUser as ServiceAdminAuthUser,
        AdminSessionValidationError,
    },
};

use super::{
    account_status_value, validated_account_import_error, AdminEnvelope, AdminError, AdminResponse,
};

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthDeviceLoginData {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub device_code: String,
    pub expires_in: u64,
    pub interval: u64,
}

impl From<DeviceCode> for AdminAuthDeviceLoginData {
    fn from(device: DeviceCode) -> Self {
        Self {
            user_code: device.user_code,
            verification_uri: device.verification_uri,
            verification_uri_complete: device.verification_uri_complete,
            device_code: device.device_code,
            expires_in: device.expires_in,
            interval: device.interval,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthDevicePollData {
    pub success: bool,
    pub pending: bool,
    pub code: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthLoginStartData {
    pub auth_url: String,
    pub state: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthCodeRelayRequest {
    pub callback_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthCodeRelayData {
    pub success: bool,
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

fn oauth_error(error: OAuthError, request_id: &str, message: &'static str) -> AdminError {
    let status = match error {
        OAuthError::Rejected(_) => StatusCode::BAD_REQUEST,
        OAuthError::AuthorizationPending | OAuthError::SlowDown | OAuthError::Transport => {
            StatusCode::BAD_GATEWAY
        }
    };
    let body_code = if status == StatusCode::BAD_REQUEST {
        40001
    } else {
        50201
    };
    AdminError::new(status, body_code, message, request_id)
}

fn admin_oauth_error(error: AdminAuthOAuthError, request_id: &str) -> AdminError {
    match error {
        AdminAuthOAuthError::OAuthClientUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "OAuth client is not initialized",
            request_id,
        ),
        AdminAuthOAuthError::AccountRepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        AdminAuthOAuthError::DeviceCodeRequired => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Device code is required",
            request_id,
        ),
        AdminAuthOAuthError::InvalidOrExpiredSession => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Invalid or expired OAuth session",
            request_id,
        ),
        AdminAuthOAuthError::DeviceCodeRequest(error) => {
            oauth_error(error, request_id, "Device code request failed")
        }
        AdminAuthOAuthError::DeviceAuthorization(error) => {
            oauth_error(error, request_id, "Device authorization failed")
        }
        AdminAuthOAuthError::TokenExchange(error) => {
            oauth_error(error, request_id, "Token exchange failed")
        }
        AdminAuthOAuthError::Import(error) => validated_account_import_error(error, request_id),
    }
}

fn request_host(headers: &HeaderMap, config: &AppConfig) -> String {
    headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("localhost:{}", config.server.port))
}

fn parse_callback_url(callback_url: &str) -> Result<(String, String), ()> {
    let callback_url = callback_url.trim();
    if callback_url.is_empty() {
        return Err(());
    }
    let url = Url::parse(callback_url).map_err(|_| ())?;
    if url.query_pairs().any(|(key, _)| key == "error") {
        return Err(());
    }
    let code = url
        .query_pairs()
        .find_map(|(key, value)| (key == "code").then(|| value.into_owned()))
        .filter(|value| !value.trim().is_empty())
        .ok_or(())?;
    let state = url
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .filter(|value| !value.trim().is_empty())
        .ok_or(())?;
    Ok((code, state))
}

fn redirect_to_host(return_host: &str, request_id: &str) -> Result<Response, AdminError> {
    let location = format!("http://{return_host}/");
    let Ok(location) = HeaderValue::from_str(&location) else {
        return Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Invalid OAuth return host",
            request_id,
        ));
    };
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(LOCATION, location);
    Ok(response)
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

pub async fn auth_device_login(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let device = state
        .services
        .admin_auth
        .request_device_code()
        .await
        .map_err(|error| admin_oauth_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminAuthDeviceLoginData::from(device), request_id),
    ))
}

pub async fn auth_device_poll(
    State(state): State<AppState>,
    Path(device_code): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let result = state
        .services
        .admin_auth
        .poll_device_token(&device_code)
        .await
        .map_err(|error| admin_oauth_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AdminAuthDevicePollData {
                success: result.success,
                pending: result.pending,
                code: result.code,
            },
            request_id,
        ),
    ))
}

pub async fn auth_login_start(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let return_host = request_host(&headers, state.config());
    let login = state
        .services
        .admin_auth
        .start_pkce_login(&return_host)
        .await;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AdminAuthLoginStartData {
                auth_url: login.auth_url,
                state: login.state,
            },
            request_id,
        ),
    ))
}

pub async fn auth_code_relay(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<AdminAuthCodeRelayRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let Ok((code, oauth_state)) = parse_callback_url(&payload.callback_url) else {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Invalid OAuth callback URL",
            request_id,
        ));
    };
    let exchange = state
        .services
        .admin_auth
        .exchange_pkce_code(&oauth_state, &code)
        .await
        .map_err(|error| admin_oauth_error(error, &request_id))?;

    match exchange {
        AdminAuthPkceExchange::Imported { .. } | AdminAuthPkceExchange::AlreadyCompleted => {
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(AdminAuthCodeRelayData { success: true }, request_id),
            ))
        }
    }
}

pub async fn auth_callback(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AdminAuthCallbackQuery>,
) -> Result<Response, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    if query.error.is_some() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            query
                .error_description
                .unwrap_or_else(|| "OAuth error".to_string()),
            request_id,
        ));
    }
    let (Some(code), Some(oauth_state)) = (query.code.as_deref(), query.state.as_deref()) else {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Missing OAuth code or state",
            request_id,
        ));
    };
    let exchange = state
        .services
        .admin_auth
        .exchange_pkce_code(oauth_state, code)
        .await
        .map_err(|error| admin_oauth_error(error, &request_id))?;

    match exchange {
        AdminAuthPkceExchange::Imported { return_host } => {
            redirect_to_host(&return_host, &request_id)
        }
        AdminAuthPkceExchange::AlreadyCompleted => {
            redirect_to_host(&request_host(&headers, state.config()), &request_id)
        }
    }
}
