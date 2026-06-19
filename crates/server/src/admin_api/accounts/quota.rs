//! 账号配额处理器。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension,
};
use codex_proxy_runtime::{
    services::{
        AdminAccountError, AdminAccountQuota, AdminAccountQuotaWarning, AdminAccountQuotaWarnings,
        AdminQuotaWarningLevel, AdminQuotaWarningWindow,
    },
    state::AppState,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    admin_api::{
        accounts::{account_error, account_not_found, account_status_value},
        require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 账号配额响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaData {
    /// 归一化后的配额快照。
    pub quota: Value,
    /// Codex usage 原始响应。
    pub raw: Value,
}

/// 账号配额 HTTP 错误。
pub enum AccountQuotaHttpError {
    Standard(AdminError),
    UpstreamFetch { error: String, request_id: String },
}

impl From<AdminError> for AccountQuotaHttpError {
    fn from(error: AdminError) -> Self {
        Self::Standard(error)
    }
}

impl IntoResponse for AccountQuotaHttpError {
    fn into_response(self) -> Response {
        match self {
            Self::Standard(error) => error.into_response(),
            Self::UpstreamFetch { error, request_id } => AdminResponse::new(
                StatusCode::BAD_GATEWAY,
                AdminEnvelope::new(
                    50201,
                    "Failed to fetch quota from Codex API",
                    json!({ "error": error }),
                    request_id,
                ),
            )
            .into_response(),
        }
    }
}

/// 账号配额预警响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaWarningsData {
    /// 预警列表。
    pub warnings: Vec<AccountQuotaWarningData>,
    /// 产生预警的快照中最新的拉取时间。
    pub updated_at: Option<String>,
}

/// 单条账号配额预警响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaWarningData {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 配额窗口。
    pub window: String,
    /// 预警级别。
    pub level: String,
    /// 已使用百分比。
    pub used_percent: f64,
    /// 重置时间戳。
    pub reset_at: Option<i64>,
}

/// `GET /api/admin/accounts/{account_id}/quota`
pub async fn account_quota(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AccountQuotaHttpError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state
        .services
        .admin_accounts
        .account_quota(&account_id, &request_id)
        .await
    {
        Ok(quota) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountQuotaData::from(quota), request_id),
        )),
        Err(error) => Err(account_quota_error(error, request_id)),
    }
}

/// `GET /api/admin/accounts/quota-warnings`
pub async fn quota_warnings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state.services.admin_accounts.quota_warnings().await {
        Ok(warnings) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountQuotaWarningsData::from(warnings), request_id),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

impl From<AdminAccountQuota> for AccountQuotaData {
    fn from(quota: AdminAccountQuota) -> Self {
        Self {
            quota: quota.quota,
            raw: quota.raw,
        }
    }
}

impl From<AdminAccountQuotaWarnings> for AccountQuotaWarningsData {
    fn from(warnings: AdminAccountQuotaWarnings) -> Self {
        Self {
            warnings: warnings.warnings.into_iter().map(Into::into).collect(),
            updated_at: warnings.updated_at.map(|value| value.to_rfc3339()),
        }
    }
}

impl From<AdminAccountQuotaWarning> for AccountQuotaWarningData {
    fn from(warning: AdminAccountQuotaWarning) -> Self {
        Self {
            account_id: warning.account_id,
            email: warning.email,
            window: quota_window_value(warning.window).to_string(),
            level: quota_level_value(warning.level).to_string(),
            used_percent: warning.used_percent,
            reset_at: warning.reset_at,
        }
    }
}

fn quota_window_value(window: AdminQuotaWarningWindow) -> &'static str {
    window.as_str()
}

fn quota_level_value(level: AdminQuotaWarningLevel) -> &'static str {
    level.as_str()
}

fn account_quota_error(error: AdminAccountError, request_id: String) -> AccountQuotaHttpError {
    match error {
        AdminAccountError::NotFound => account_not_found(request_id).into(),
        AdminAccountError::Inactive(status) => AdminError::new(
            StatusCode::CONFLICT,
            40901,
            format!(
                "Account is {}, cannot query quota",
                account_status_value(status)
            ),
            request_id,
        )
        .into(),
        AdminAccountError::FetchQuota(error) => {
            AccountQuotaHttpError::UpstreamFetch { error, request_id }
        }
        AdminAccountError::StoreQuota => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to store account quota",
            request_id,
        )
        .into(),
        error => account_error(error, request_id).into(),
    }
}
