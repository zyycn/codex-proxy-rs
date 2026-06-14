use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::{Deserialize, Serialize};

use crate::{
    codex::accounts::service::{AccountProbeResult, HealthCheckError},
    platform::http::request_id::RequestId,
    runtime::state::AppState,
};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::account_status_value;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthCheckRequest {
    pub ids: Option<Vec<String>>,
    pub stagger_ms: Option<u64>,
    pub concurrency: Option<u8>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckData {
    pub summary: HealthCheckSummary,
    pub results: Vec<AccountProbeData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckSummary {
    pub total: usize,
    pub alive: usize,
    pub dead: usize,
    pub skipped: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProbeData {
    pub id: String,
    pub email: Option<String>,
    pub previous_status: String,
    pub result: String,
    pub status: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u128>,
}

pub async fn health_check_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let payload = parse_health_check_request(&body, &request_id)?;
    if payload.ids.as_ref().is_some_and(Vec::is_empty) {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account ids must not be empty",
            request_id,
        ));
    }
    if payload
        .stagger_ms
        .is_some_and(|value| !(500..=30_000).contains(&value))
    {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "staggerMs must be between 500 and 30000",
            request_id,
        ));
    }
    if payload
        .concurrency
        .is_some_and(|value| !(1..=10).contains(&value))
    {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "concurrency must be between 1 and 10",
            request_id,
        ));
    }
    require_admin_session(&state, &headers, &request_id).await?;

    let concurrency = usize::from(payload.concurrency.unwrap_or(2));
    let stagger_ms = payload.stagger_ms.unwrap_or(3_000);
    let results = state
        .services
        .accounts
        .health_check_accounts(payload.ids, concurrency, stagger_ms, &request_id)
        .await
        .map_err(|error| health_check_error(error, &request_id))?
        .into_iter()
        .map(account_probe_data_from_service)
        .collect::<Vec<_>>();
    let summary = HealthCheckSummary {
        total: results.len(),
        alive: results
            .iter()
            .filter(|result| result.result == "alive")
            .count(),
        dead: results
            .iter()
            .filter(|result| result.result == "dead")
            .count(),
        skipped: results
            .iter()
            .filter(|result| result.result == "skipped")
            .count(),
    };

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(HealthCheckData { summary, results }, request_id),
    ))
}

fn parse_health_check_request(
    body: &Bytes,
    request_id: &str,
) -> Result<HealthCheckRequest, AdminError> {
    if body.is_empty() {
        return Ok(HealthCheckRequest::default());
    }
    serde_json::from_slice(body).map_err(|_| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Invalid health check request",
            request_id,
        )
    })
}

pub(super) fn health_check_error(error: HealthCheckError, request_id: &str) -> AdminError {
    match error {
        HealthCheckError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        HealthCheckError::List => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list accounts",
            request_id,
        ),
    }
}

pub(crate) fn account_probe_data_from_service(result: AccountProbeResult) -> AccountProbeData {
    AccountProbeData {
        id: result.id,
        email: result.email,
        previous_status: account_status_value(result.previous_status).to_string(),
        result: result.outcome.as_str().to_string(),
        status: result
            .status
            .map(account_status_value)
            .map(ToString::to_string),
        error: result.error,
        duration_ms: result.duration_ms,
    }
}
