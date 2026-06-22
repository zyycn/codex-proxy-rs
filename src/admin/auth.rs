//! OAuth 认证 HTTP 处理器。

use axum::{
    extract::{Path, Query, State},
    http::{header::HOST, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect},
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::{
        accounts::AdminAccountData,
        response::{AdminEnvelope, AdminError, AdminResponse},
        session::require_admin_session,
    },
    app::{
        services::{AdminDevicePoll, AdminOAuthError},
        state::AppState,
    },
    http::middleware::request_id::RequestId,
};

/// OAuth PKCE 登录开始响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthLoginStartData {
    /// 授权 URL。
    pub auth_url: String,
    /// OAuth state。
    pub state: String,
}

/// 管理端认证状态响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthStatusData {
    authenticated: bool,
    user: Option<AdminAccountData>,
    pool: AdminAuthPoolData,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthPoolData {
    total: u32,
    active: u32,
    expired: u32,
    quota_exhausted: u32,
    refreshing: u32,
    disabled: u32,
    banned: u32,
}

/// 管理端登出响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthLogoutData {
    success: bool,
    deleted: u64,
}

/// 设备码登录响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthDeviceLoginData {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

/// 设备码轮询响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthDevicePollData {
    success: bool,
    pending: bool,
    code: Option<&'static str>,
}

/// OAuth code relay 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthCodeRelayRequest {
    callback_url: String,
}

/// OAuth 登录完成响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthSuccessData {
    success: bool,
}

/// OAuth callback query。
#[derive(Debug, Deserialize)]
pub struct AdminAuthCallbackQuery {
    code: String,
    state: String,
}

/// `GET /api/admin/auth/status`
pub async fn auth_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let status = state
        .services
        .admin_accounts
        .auth_status()
        .await
        .map_err(|error| account_error(error, request_id.clone()))?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminAuthStatusData::from(status), request_id),
    ))
}

/// `POST /api/admin/auth/logout`
pub async fn auth_logout(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let result = state
        .services
        .admin_accounts
        .logout()
        .await
        .map_err(|error| account_error(error, request_id.clone()))?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AdminAuthLogoutData {
                success: result.success,
                deleted: result.deleted,
            },
            request_id,
        ),
    ))
}

/// `POST /api/admin/auth/login-start`
pub async fn auth_login_start(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let login = state
        .services
        .admin_oauth
        .start_pkce_login(&request_host(&headers))
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

/// `POST /api/admin/auth/device-login`
pub async fn auth_device_login(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let device = state
        .services
        .admin_oauth
        .request_device_code()
        .await
        .map_err(|error| oauth_error(error, request_id.clone()))?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AdminAuthDeviceLoginData {
                device_code: device.device_code,
                user_code: device.user_code,
                verification_uri: device.verification_uri,
                verification_uri_complete: device.verification_uri_complete,
                expires_in: device.expires_in,
                interval: device.interval,
            },
            request_id,
        ),
    ))
}

/// `GET /api/admin/auth/device-poll/{device_code}`
pub async fn auth_device_poll(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(device_code): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state
        .services
        .admin_oauth
        .poll_device_token(&device_code)
        .await
        .map_err(|error| oauth_error(error, request_id.clone()))?
    {
        AdminDevicePoll::Pending { code } => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                AdminAuthDevicePollData {
                    success: false,
                    pending: true,
                    code: Some(code),
                },
                request_id,
            ),
        )),
        AdminDevicePoll::Authorized(tokens) => {
            state
                .services
                .admin_accounts
                .create(Some(tokens.access_token), tokens.refresh_token)
                .await
                .map_err(|error| account_error(error, request_id.clone()))?;
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(
                    AdminAuthDevicePollData {
                        success: true,
                        pending: false,
                        code: None,
                    },
                    request_id,
                ),
            ))
        }
    }
}

/// `POST /api/admin/auth/code-relay`
pub async fn auth_code_relay(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<AdminAuthCodeRelayRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let (code, oauth_state) = callback_params(&payload.callback_url)
        .ok_or_else(|| oauth_error(AdminOAuthError::InvalidCallback, request_id.clone()))?;
    let callback = state
        .services
        .admin_oauth
        .exchange_callback(&code, &oauth_state)
        .await
        .map_err(|error| oauth_error(error, request_id.clone()))?;
    state
        .services
        .admin_accounts
        .create(
            Some(callback.tokens.access_token),
            callback.tokens.refresh_token,
        )
        .await
        .map_err(|error| account_error(error, request_id.clone()))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminAuthSuccessData { success: true }, request_id),
    ))
}

/// `GET /auth/callback`
pub async fn auth_callback(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AdminAuthCallbackQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let callback = state
        .services
        .admin_oauth
        .exchange_callback(&query.code, &query.state)
        .await
        .map_err(|error| oauth_error(error, request_id.clone()))?;
    state
        .services
        .admin_accounts
        .create(
            Some(callback.tokens.access_token),
            callback.tokens.refresh_token,
        )
        .await
        .map_err(|error| account_error(error, request_id))?;

    Ok(Redirect::to(&format!("http://{}/", callback.return_host)))
}

fn request_host(headers: &HeaderMap) -> String {
    headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "localhost".to_string())
}

fn callback_params(callback_url: &str) -> Option<(String, String)> {
    let (_base, query) = callback_url.split_once('?')?;
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "code" if !value.is_empty() => code = Some(value.to_string()),
            "state" if !value.is_empty() => state = Some(value.to_string()),
            _ => {}
        }
    }
    Some((code?, state?))
}

fn oauth_error(error: AdminOAuthError, request_id: String) -> AdminError {
    match error {
        AdminOAuthError::InvalidCallback | AdminOAuthError::InvalidState => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        ),
        AdminOAuthError::OAuth(_) => AdminError::new(
            StatusCode::BAD_GATEWAY,
            50201,
            error.to_string(),
            request_id,
        ),
    }
}

fn account_error(error: crate::app::services::AdminAccountError, request_id: String) -> AdminError {
    match error {
        crate::app::services::AdminAccountError::InvalidStatus(_)
        | crate::app::services::AdminAccountError::LabelTooLong
        | crate::app::services::AdminAccountError::EmptyIds
        | crate::app::services::AdminAccountError::NoImportableAccounts
        | crate::app::services::AdminAccountError::InvalidAccessTokenExpiresAt
        | crate::app::services::AdminAccountError::TokenRequired
        | crate::app::services::AdminAccountError::InvalidToken(_)
        | crate::app::services::AdminAccountError::RefreshTokenExchange(_)
        | crate::app::services::AdminAccountError::NoValidCookies => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        ),
        crate::app::services::AdminAccountError::NotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ),
        crate::app::services::AdminAccountError::Inactive(_) => {
            AdminError::new(StatusCode::CONFLICT, 40901, error.to_string(), request_id)
        }
        _ => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            error.to_string(),
            request_id,
        ),
    }
}

impl From<crate::app::services::AdminAuthStatus> for AdminAuthStatusData {
    fn from(status: crate::app::services::AdminAuthStatus) -> Self {
        Self {
            authenticated: status.authenticated,
            user: status.user.map(AdminAccountData::from),
            pool: AdminAuthPoolData {
                total: status.pool.total,
                active: status.pool.active,
                expired: status.pool.expired,
                quota_exhausted: status.pool.quota_exhausted,
                refreshing: status.pool.refreshing,
                disabled: status.pool.disabled,
                banned: status.pool.banned,
            },
        }
    }
}
