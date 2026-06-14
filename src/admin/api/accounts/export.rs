use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use chrono::{SecondsFormat, Utc};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::{json, Map, Value};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountExportFormat {
    Native,
    Sub2Api,
}

pub async fn export_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let format = match parse_account_export_format(query.format.as_deref()) {
        Ok(format) => format,
        Err(message) => {
            return Err(AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                message,
                request_id,
            ));
        }
    };
    require_admin_session(&state, &headers, &request_id).await?;

    let ids = account_export_ids(query.ids.as_deref());
    let accounts = state
        .services
        .accounts
        .export(ids)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    // 账号导出会返回可重新导入的 OAuth token；只允许 admin session 访问，不写入日志。
    let data = match format {
        AccountExportFormat::Native => native_account_export(accounts),
        AccountExportFormat::Sub2Api => sub2api_account_export(accounts),
    };
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(data, request_id),
    ))
}

fn parse_account_export_format(value: Option<&str>) -> Result<AccountExportFormat, &'static str> {
    match value.unwrap_or("native") {
        "" | "native" | "full" => Ok(AccountExportFormat::Native),
        "sub2api" => Ok(AccountExportFormat::Sub2Api),
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

fn sub2api_account_export(accounts: Vec<StoredAccount>) -> Value {
    json!({
        "exported_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "proxies": [],
        "accounts": accounts.into_iter().map(sub2api_export_account).collect::<Vec<_>>(),
        "type": "sub2api-data",
        "version": 1,
    })
}

fn sub2api_export_account(account: StoredAccount) -> Value {
    let mut credentials = Map::new();
    credentials.insert(
        "access_token".to_string(),
        Value::String(account.access_token.expose_secret().to_string()),
    );
    insert_optional_string(
        &mut credentials,
        "refresh_token",
        account
            .refresh_token
            .as_ref()
            .map(|token| token.expose_secret().to_string()),
    );
    insert_optional_string(&mut credentials, "email", account.email.clone());
    insert_optional_string(&mut credentials, "chatgpt_account_id", account.account_id);
    insert_optional_string(&mut credentials, "chatgpt_user_id", account.user_id);
    insert_optional_string(&mut credentials, "plan_type", account.plan_type);
    insert_optional_string(
        &mut credentials,
        "expires_at",
        account
            .access_token_expires_at
            .map(|value| value.to_rfc3339()),
    );

    json!({
        "name": account
            .label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .map(str::to_string)
            .or_else(|| account.email.clone())
            .unwrap_or_else(|| account.id.clone()),
        "platform": "openai",
        "type": "oauth",
        "credentials": credentials,
        "concurrency": 0,
        "priority": 0,
    })
}

fn insert_optional_string(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        map.insert(key.to_string(), Value::String(value));
    }
}
