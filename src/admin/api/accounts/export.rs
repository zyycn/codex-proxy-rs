use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    codex::accounts::repository::StoredAccount, platform::http::request_id::RequestId,
    runtime::state::AppState,
};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::{account_export_ids, account_service_error, account_status_value};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountExportQuery {
    pub ids: Option<String>,
    pub format: Option<String>,
}

pub async fn export_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    validate_account_export_format(query.format.as_deref())
        .map_err(|message| AdminError::new(StatusCode::BAD_REQUEST, 40001, message, &request_id))?;
    require_admin_session(&state, &headers, &request_id).await?;

    let ids = account_export_ids(query.ids.as_deref());
    let accounts = state
        .services
        .accounts
        .export(ids)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    // 账号导出会返回可重新导入的 OAuth token；只允许 admin session 访问，不写入日志。
    let data = native_account_export(accounts);
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(data, request_id),
    ))
}

fn validate_account_export_format(value: Option<&str>) -> Result<(), &'static str> {
    match value.unwrap_or("native") {
        "" | "native" => Ok(()),
        _ => Err("Unsupported account export format"),
    }
}

fn native_account_export(accounts: Vec<StoredAccount>) -> Value {
    json!({
        "sourceFormat": "native",
        "accounts": accounts.into_iter().map(native_export_account).collect::<Vec<_>>(),
    })
}

fn native_export_account(account: StoredAccount) -> Value {
    json!({
        "id": account.id,
        "email": account.email,
        "accountId": account.account_id,
        "userId": account.user_id,
        "label": account.label,
        "planType": account.plan_type,
        "token": account.access_token.expose_secret(),
        "refreshToken": account
            .refresh_token
            .as_ref()
            .map(|token| token.expose_secret().to_string()),
        "status": account_status_value(account.status),
        "accessTokenExpiresAt": account.access_token_expires_at.map(|value| value.to_rfc3339()),
        "addedAt": account.added_at.to_rfc3339(),
        "updatedAt": account.updated_at.to_rfc3339(),
    })
}
