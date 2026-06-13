use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use chrono::SecondsFormat;
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    codex::accounts::service::{
        AccountQuotaError, AccountQuotaWarning as ServiceAccountQuotaWarning,
        AccountQuotaWarnings as ServiceAccountQuotaWarnings,
    },
    platform::http::middleware::RequestId,
    runtime::state::AppState,
};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::{account_service_error, account_status_value};

pub(crate) enum AccountQuotaHttpError {
    Standard(AdminError),
    UpstreamFetch { error: String, request_id: String },
}

impl From<AdminError> for AccountQuotaHttpError {
    fn from(error: AdminError) -> Self {
        Self::Standard(error)
    }
}

impl IntoResponse for AccountQuotaHttpError {
    fn into_response(self) -> axum::response::Response {
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaData {
    pub quota: Value,
    pub raw: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaWarningsData {
    pub warnings: Vec<AccountQuotaWarningData>,
    pub updated_at: Option<String>,
}

impl From<ServiceAccountQuotaWarnings> for AccountQuotaWarningsData {
    fn from(warnings: ServiceAccountQuotaWarnings) -> Self {
        Self {
            warnings: warnings.warnings.into_iter().map(Into::into).collect(),
            updated_at: warnings
                .updated_at
                .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true)),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaWarningData {
    pub account_id: String,
    pub email: Option<String>,
    pub window: String,
    pub level: String,
    pub used_percent: f64,
    pub reset_at: Option<i64>,
}

impl From<ServiceAccountQuotaWarning> for AccountQuotaWarningData {
    fn from(warning: ServiceAccountQuotaWarning) -> Self {
        Self {
            account_id: warning.account_id,
            email: warning.email,
            window: warning.window.as_str().to_string(),
            level: warning.level.as_str().to_string(),
            used_percent: warning.used_percent,
            reset_at: warning.reset_at,
        }
    }
}

pub(crate) async fn account_quota(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AccountQuotaHttpError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let result = state
        .services
        .accounts
        .account_quota(&account_id, &request_id)
        .await
        .map_err(|error| account_quota_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AccountQuotaData {
                quota: result.quota,
                raw: result.raw,
            },
            request_id,
        ),
    ))
}

pub(crate) async fn quota_warnings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let warnings = state
        .services
        .accounts
        .quota_warnings()
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountQuotaWarningsData::from(warnings), request_id),
    ))
}

fn account_quota_error(error: AccountQuotaError, request_id: &str) -> AccountQuotaHttpError {
    match error {
        AccountQuotaError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        )
        .into(),
        AccountQuotaError::Load => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account",
            request_id,
        )
        .into(),
        AccountQuotaError::NotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        )
        .into(),
        AccountQuotaError::Inactive(status) => AdminError::new(
            StatusCode::CONFLICT,
            40901,
            format!(
                "Account is {}, cannot query quota",
                account_status_value(status)
            ),
            request_id,
        )
        .into(),
        AccountQuotaError::StoreQuota => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to store account quota",
            request_id,
        )
        .into(),
        AccountQuotaError::Fetch(error) => AccountQuotaHttpError::UpstreamFetch {
            error,
            request_id: request_id.to_string(),
        },
    }
}
