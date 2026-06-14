use axum::{
    extract::{Path, Query, State},
    http::{
        header::{HeaderValue, HOST, LOCATION},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Extension, Json,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::{
    admin::session::service::{AdminAuthOAuthError, AdminAuthPkceExchange},
    codex::gateway::oauth::{DeviceCode, OAuthError},
    config::AppConfig,
    platform::http::request_id::RequestId,
    runtime::state::AppState,
};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::validated_account_import_error;

// OAuth 成功后会导入或更新账号资产，因此 handler 归在 accounts 域；密码登录仍留在 auth。
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
