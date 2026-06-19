//! 账号 Cookie 处理器。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use codex_proxy_runtime::state::AppState;

use crate::{
    admin_api::{
        accounts::{account_error, account_not_found},
        require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 设置账号 Cookie 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAccountCookiesRequest {
    /// Cookie 请求头字符串，或 name/value 对象。
    pub cookies: Value,
}

/// 账号 Cookie 响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCookiesData {
    /// 标准 HTTP Cookie 请求头。
    pub cookies: Option<String>,
}

/// 删除账号 Cookie 响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAccountCookiesData {
    /// 是否完成删除。
    pub deleted: bool,
}

/// `GET /api/admin/accounts/{account_id}/cookies`
pub async fn get_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state.services.admin_accounts.cookies(&account_id).await {
        Ok(cookies) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
        )),
        Err(codex_proxy_runtime::services::AdminAccountError::NotFound) => {
            Err(account_not_found(request_id))
        }
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/{account_id}/cookies`
pub async fn set_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<SetAccountCookiesRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let cookie_header = admin_cookie_header(&payload.cookies).map_err(|message| {
        AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id.clone())
    })?;
    require_admin_session(&state, &headers, &request_id).await?;

    match state
        .services
        .admin_accounts
        .set_cookies(&account_id, &cookie_header)
        .await
    {
        Ok(cookies) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
        )),
        Err(codex_proxy_runtime::services::AdminAccountError::NotFound) => {
            Err(account_not_found(request_id))
        }
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `DELETE /api/admin/accounts/{account_id}/cookies`
pub async fn delete_account_cookies(
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
        .delete_cookies(&account_id)
        .await
    {
        Ok(()) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(DeleteAccountCookiesData { deleted: true }, request_id),
        )),
        Err(codex_proxy_runtime::services::AdminAccountError::NotFound) => {
            Err(account_not_found(request_id))
        }
        Err(error) => Err(account_error(error, request_id)),
    }
}

fn admin_cookie_header(value: &Value) -> Result<String, &'static str> {
    if let Some(cookies) = value.as_str() {
        let cookies = cookies.trim();
        if cookies.is_empty() {
            return Err("cookies field is required");
        }
        return Ok(cookies.to_string());
    }

    let Some(object) = value.as_object() else {
        return Err("cookies must be a string or object");
    };
    if object.is_empty() {
        return Err("cookies field is required");
    }

    let pairs = object
        .iter()
        .filter_map(|(name, value)| {
            let value = value.as_str()?.trim();
            (!name.trim().is_empty() && !value.is_empty())
                .then(|| format!("{}={value}", name.trim()))
        })
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        Err("No valid cookies found")
    } else {
        Ok(pairs.join("; "))
    }
}
