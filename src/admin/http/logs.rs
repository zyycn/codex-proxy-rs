use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    codex::logs::service::{ClearLogs, LogListFilter, LogServiceError, LogState, LogStateUpdate},
    platform::http::middleware::RequestId,
    runtime::state::AppState,
    utils::pagination::clamp_limit,
};

use super::{require_admin_session, AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub kind: Option<String>,
    pub level: Option<String>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub search: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogStateData {
    pub enabled: bool,
    pub capacity: u32,
    pub capture_body: bool,
    pub stored_count: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearLogsData {
    pub cleared: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateLogStateRequest {
    pub enabled: Option<bool>,
    pub capacity: Option<u32>,
    pub capture_body: Option<bool>,
}

pub async fn logs(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<LogsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let limit = clamp_limit(query.limit.unwrap_or(50));
    let cursor = query.cursor.clone();
    match state.services.logs.list(cursor, limit, query.into()).await {
        Ok(page) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminPageEnvelope::ok(page, limit, request_id),
        )),
        Err(error) => Err(log_service_error(error, request_id)),
    }
}

pub async fn logs_state(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state.services.logs.state().await {
        Ok(state) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(LogStateData::from(state), request_id),
        )),
        Err(error) => Err(log_service_error(error, request_id)),
    }
}

pub async fn update_logs_state(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<UpdateLogStateRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state.services.logs.update_state(payload.into()).await {
        Ok(state) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(LogStateData::from(state), request_id),
        )),
        Err(error) => Err(log_service_error(error, request_id)),
    }
}

pub async fn log_detail(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(log_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state.services.logs.get(&log_id).await {
        Ok(Some(log)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(log, request_id),
        )),
        Ok(None) => Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Log event not found",
            request_id,
        )),
        Err(error) => Err(log_service_error(error, request_id)),
    }
}

pub async fn clear_logs(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state.services.logs.clear().await {
        Ok(cleared) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(ClearLogsData::from(cleared), request_id),
        )),
        Err(error) => Err(log_service_error(error, request_id)),
    }
}

impl From<LogState> for LogStateData {
    fn from(state: LogState) -> Self {
        Self {
            enabled: state.enabled,
            capacity: state.capacity,
            capture_body: state.capture_body,
            stored_count: state.stored_count,
        }
    }
}

impl From<ClearLogs> for ClearLogsData {
    fn from(cleared: ClearLogs) -> Self {
        Self {
            cleared: cleared.cleared,
        }
    }
}

impl From<UpdateLogStateRequest> for LogStateUpdate {
    fn from(request: UpdateLogStateRequest) -> Self {
        Self {
            enabled: request.enabled,
            capacity: request.capacity,
            capture_body: request.capture_body,
        }
    }
}

impl From<LogsQuery> for LogListFilter {
    fn from(query: LogsQuery) -> Self {
        Self {
            kind: non_empty(query.kind),
            level: non_empty(query.level),
            request_id: non_empty(query.request_id),
            account_id: non_empty(query.account_id),
            route: non_empty(query.route),
            model: non_empty(query.model),
            status_code: query.status_code,
            search: non_empty(query.search),
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn log_service_error(error: LogServiceError, request_id: String) -> AdminError {
    match error {
        LogServiceError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Event log repository is not initialized",
            request_id,
        ),
        LogServiceError::List => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list event logs",
            request_id,
        ),
        LogServiceError::Get => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load event log",
            request_id,
        ),
        LogServiceError::Count => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to count event logs",
            request_id,
        ),
        LogServiceError::Clear => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to clear event logs",
            request_id,
        ),
        LogServiceError::InvalidCapacity => AdminError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            42201,
            "logsCapacity must be greater than 0",
            request_id,
        ),
    }
}
