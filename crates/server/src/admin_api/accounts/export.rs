//! 账号导出处理器。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_runtime::{services::AdminStoredAccount, state::AppState};
use serde::{Deserialize, Serialize};

use crate::{
    admin_api::{
        accounts::{account_error, account_status_value},
        require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 账号导出查询。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountExportQuery {
    /// 逗号分隔的账号 ID 列表；空表示导出全部。
    pub ids: Option<String>,
    /// 导出格式；当前仅支持 `native`。
    pub format: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountExportData {
    source_format: &'static str,
    accounts: Vec<AccountExportEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountExportEntry {
    id: String,
    email: Option<String>,
    account_id: Option<String>,
    user_id: Option<String>,
    label: Option<String>,
    plan_type: Option<String>,
    token: String,
    refresh_token: Option<String>,
    status: String,
    access_token_expires_at: Option<String>,
    added_at: String,
    updated_at: String,
}

/// `GET /api/admin/accounts/export`
pub async fn export_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    validate_account_export_format(query.format.as_deref()).map_err(|message| {
        AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id.clone())
    })?;
    let ids = account_export_ids(query.ids.as_deref());

    match state.services.admin_accounts.export(ids).await {
        Ok(accounts) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                AccountExportData {
                    source_format: "native",
                    accounts: accounts.into_iter().map(AccountExportEntry::from).collect(),
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

fn validate_account_export_format(value: Option<&str>) -> Result<(), &'static str> {
    match value.unwrap_or("native").trim() {
        "" | "native" => Ok(()),
        _ => Err("Unsupported account export format"),
    }
}

fn account_export_ids(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|ids| ids.split(','))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect()
}

impl From<AdminStoredAccount> for AccountExportEntry {
    fn from(account: AdminStoredAccount) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            token: account.access_token,
            refresh_token: account.refresh_token,
            status: account_status_value(account.status).to_string(),
            access_token_expires_at: account
                .access_token_expires_at
                .map(|value| value.to_rfc3339()),
            added_at: account.added_at.to_rfc3339(),
            updated_at: account.updated_at.to_rfc3339(),
        }
    }
}
