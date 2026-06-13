use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    codex::accounts::{model::AccountStatus, service::RefreshAccountError},
    platform::http::middleware::RequestId,
    runtime::state::AppState,
};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::{account_probe_data_from_service, account_service_error, account_status_value};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetAccountUsageData {
    pub id: String,
    pub reset: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountLabelRequest {
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountLabelData {
    pub id: String,
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountStatusRequest {
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountStatusData {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateAccountStatusRequest {
    pub ids: Vec<String>,
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateAccountStatusData {
    pub updated: u32,
    pub not_found: Vec<String>,
}

pub async fn refresh_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let result = state
        .services
        .accounts
        .refresh_account(&account_id)
        .await
        .map_err(|error| refresh_account_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(account_probe_data_from_service(result), request_id),
    ))
}

pub async fn reset_account_usage(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    if !state
        .services
        .accounts
        .reset_usage(&account_id)
        .await
        .map_err(|error| account_service_error(error, &request_id))?
    {
        return Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ));
    }

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            ResetAccountUsageData {
                id: account_id,
                reset: true,
            },
            request_id,
        ),
    ))
}

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
    let updated = state
        .services
        .accounts
        .update_label(&account_id, label.clone())
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    if !updated {
        return Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ));
    }

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            UpdateAccountLabelData {
                id: account_id,
                label,
            },
            request_id,
        ),
    ))
}

pub async fn update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<UpdateAccountStatusRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let updated = state
        .services
        .accounts
        .update_status(&account_id, &payload.status)
        .await
        .map_err(|error| account_service_error(error, &request_id))?
        .ok_or_else(|| {
            AdminError::new(
                StatusCode::NOT_FOUND,
                40401,
                "Account not found",
                request_id.as_str(),
            )
        })?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            UpdateAccountStatusData {
                id: updated.id,
                status: account_status_value(updated.status).to_string(),
            },
            request_id,
        ),
    ))
}

pub async fn batch_update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchUpdateAccountStatusRequest>,
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
    parse_admin_account_status(&payload.status).map_err(|message| {
        AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id.as_str())
    })?;
    require_admin_session(&state, &headers, &request_id).await?;

    let updated = state
        .services
        .accounts
        .batch_update_status(payload.ids, &payload.status)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            BatchUpdateAccountStatusData {
                updated: updated.updated,
                not_found: updated.not_found,
            },
            request_id,
        ),
    ))
}

fn refresh_account_error(error: RefreshAccountError, request_id: &str) -> AdminError {
    match error {
        RefreshAccountError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        RefreshAccountError::Load => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account",
            request_id,
        ),
        RefreshAccountError::NotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ),
        RefreshAccountError::LeaseAcquire => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to acquire refresh lease",
            request_id,
        ),
        RefreshAccountError::TokenRefresherUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Token refresher is not initialized",
            request_id,
        ),
        RefreshAccountError::StoreRefreshed => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to store refreshed account",
            request_id,
        ),
    }
}

fn parse_admin_account_status(status: &str) -> Result<AccountStatus, String> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(AccountStatus::Active),
        "disabled" => Ok(AccountStatus::Disabled),
        other => Err(format!("Unsupported account status: {other}")),
    }
}
