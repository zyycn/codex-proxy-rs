use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    codex::events::service::{ClearLogs, LogState, LogStateUpdate},
    platform::http::request_id::RequestId,
    runtime::state::AppState,
};

use super::log_service_error;
use crate::admin::api::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};

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
