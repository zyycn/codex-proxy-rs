//! 管理端账号认证状态 HTTP 处理器。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::Serialize;

use crate::{
    admin::accounts::service::{AdminAccountError, AdminAuthStatus},
    admin::{
        accounts::routes::AdminAccountData,
        auth::session::require_admin_session,
        response::{AdminEnvelope, AdminError, AdminResponse},
    },
    http::middleware::request_id::RequestId,
    runtime::state::AppState,
};

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

/// `GET /api/admin/accounts/auth-status`
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

fn account_error(error: AdminAccountError, request_id: String) -> AdminError {
    match error {
        AdminAccountError::InvalidStatus(_)
        | AdminAccountError::LabelTooLong
        | AdminAccountError::EmptyIds
        | AdminAccountError::NoImportableAccounts
        | AdminAccountError::InvalidAccessTokenExpiresAt
        | AdminAccountError::TokenRequired
        | AdminAccountError::InvalidToken(_)
        | AdminAccountError::RefreshTokenExchange(_)
        | AdminAccountError::NoValidCookies => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        ),
        AdminAccountError::NotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ),
        AdminAccountError::Inactive(_) => {
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

impl From<AdminAuthStatus> for AdminAuthStatusData {
    fn from(status: AdminAuthStatus) -> Self {
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
