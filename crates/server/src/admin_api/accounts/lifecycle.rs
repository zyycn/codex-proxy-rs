//! 账号生命周期处理器。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use codex_proxy_runtime::{
    services::{AdminAccountProbeOutcome, AdminAccountRefresh},
    state::AppState,
};

use crate::{
    admin_api::{
        accounts::{account_error, account_not_found, account_status_value},
        require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 更新账号标签请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountLabelRequest {
    /// 新标签；`null` 表示清空。
    pub label: Option<String>,
}

/// 更新账号标签响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountLabelData {
    /// 账号 ID。
    pub id: String,
    /// 新标签。
    pub label: Option<String>,
}

/// 更新账号状态请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountStatusRequest {
    /// 新状态。
    pub status: String,
}

/// 更新账号状态响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountStatusData {
    /// 账号 ID。
    pub id: String,
    /// 新状态。
    pub status: String,
}

/// 批量删除账号请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteAccountsRequest {
    /// 账号 ID 列表。
    pub ids: Vec<String>,
}

/// 批量删除账号响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteAccountsData {
    /// 成功删除数量。
    pub deleted: u32,
    /// 未找到的账号 ID。
    pub not_found: Vec<String>,
}

/// 批量更新账号状态请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateAccountStatusRequest {
    /// 账号 ID 列表。
    pub ids: Vec<String>,
    /// 新状态。
    pub status: String,
}

/// 批量更新账号状态响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateAccountStatusData {
    /// 成功更新数量。
    pub updated: u32,
    /// 未找到的账号 ID。
    pub not_found: Vec<String>,
}

/// 删除账号响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAccountData {
    /// 是否已删除。
    pub deleted: bool,
}

/// 刷新账号响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshAccountData {
    /// 账号 ID。
    pub id: String,
    /// 刷新结果。
    pub result: String,
    /// 刷新前状态。
    pub previous_status: String,
    /// 刷新后状态。
    pub status: Option<String>,
    /// 错误信息。
    pub error: Option<String>,
}

/// 重置账号用量响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetAccountUsageData {
    /// 账号 ID。
    pub id: String,
    /// 是否已处理。
    pub reset: bool,
}

/// `PATCH /api/admin/accounts/{account_id}/label`
pub async fn update_account_label(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<UpdateAccountLabelRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let label = payload.label;
    match state
        .services
        .admin_accounts
        .update_label(&account_id, label.clone())
        .await
    {
        Ok(true) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                UpdateAccountLabelData {
                    id: account_id,
                    label,
                },
                request_id,
            ),
        )),
        Ok(false) => Err(account_not_found(request_id)),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `PATCH /api/admin/accounts/{account_id}/status`
pub async fn update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<UpdateAccountStatusRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .update_status(&account_id, &payload.status)
        .await
    {
        Ok(Some(updated)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                UpdateAccountStatusData {
                    id: updated.id,
                    status: account_status_value(updated.status).to_string(),
                },
                request_id,
            ),
        )),
        Ok(None) => Err(account_not_found(request_id)),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `DELETE /api/admin/accounts/{account_id}`
pub async fn delete_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_accounts.delete(&account_id).await {
        Ok(true) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(DeleteAccountData { deleted: true }, request_id),
        )),
        Ok(false) => Err(account_not_found(request_id)),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/batch-delete`
pub async fn batch_delete_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchDeleteAccountsRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .batch_delete(payload.ids)
        .await
    {
        Ok(deleted) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                BatchDeleteAccountsData {
                    deleted: deleted.deleted,
                    not_found: deleted.not_found,
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/batch-status`
pub async fn batch_update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchUpdateAccountStatusRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .batch_update_status(payload.ids, &payload.status)
        .await
    {
        Ok(updated) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                BatchUpdateAccountStatusData {
                    updated: updated.updated,
                    not_found: updated.not_found,
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/{account_id}/refresh`
pub async fn refresh_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .refresh_account(&account_id)
        .await
    {
        Ok(refreshed) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(RefreshAccountData::from(refreshed), request_id),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/{account_id}/reset-usage`
pub async fn reset_account_usage(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_accounts.reset_usage(&account_id).await {
        Ok(reset) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                ResetAccountUsageData {
                    id: reset.id,
                    reset: reset.reset,
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

impl From<AdminAccountRefresh> for RefreshAccountData {
    fn from(refreshed: AdminAccountRefresh) -> Self {
        Self {
            id: refreshed.id,
            result: refresh_outcome_value(refreshed.outcome).to_string(),
            previous_status: account_status_value(refreshed.previous_status).to_string(),
            status: refreshed
                .status
                .map(account_status_value)
                .map(ToString::to_string),
            error: refreshed.error,
        }
    }
}

fn refresh_outcome_value(outcome: AdminAccountProbeOutcome) -> &'static str {
    outcome.as_str()
}
