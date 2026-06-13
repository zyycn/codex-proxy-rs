use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{platform::http::middleware::RequestId, runtime::state::AppState};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::account_service_error;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAccountCookiesRequest {
    pub cookies: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCookiesData {
    pub cookies: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAccountCookiesData {
    pub deleted: bool,
}

pub async fn get_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let cookies = state
        .services
        .accounts
        .get_cookies(&account_id)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
    ))
}

pub async fn set_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<SetAccountCookiesRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let cookie_header = match admin_cookie_header(&payload.cookies) {
        Ok(cookie_header) => cookie_header,
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

    let cookies = state
        .services
        .accounts
        .set_cookies(&account_id, &cookie_header)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
    ))
}

pub async fn delete_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    state
        .services
        .accounts
        .delete_cookies(&account_id)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DeleteAccountCookiesData { deleted: true }, request_id),
    ))
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
