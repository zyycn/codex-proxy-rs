use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crate::{platform::http::middleware::RequestId, runtime::state::AppState};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::account_service_error;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAccountData {
    pub deleted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteAccountsRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteAccountsData {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

pub async fn delete_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let deleted = state
        .services
        .accounts
        .delete(&account_id)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    if !deleted {
        return Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ));
    }

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DeleteAccountData { deleted: true }, request_id),
    ))
}

pub async fn batch_delete_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchDeleteAccountsRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    if payload.ids.is_empty() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account ids are required",
            request_id,
        ));
    }
    require_admin_session(&state, &headers, &request_id).await?;

    let deleted = state
        .services
        .accounts
        .batch_delete(payload.ids)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            BatchDeleteAccountsData {
                deleted: deleted.deleted,
                not_found: deleted.not_found,
            },
            request_id,
        ),
    ))
}
