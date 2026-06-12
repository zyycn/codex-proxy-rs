use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::Deserialize;

use crate::{
    app::state::AppState, http::middleware::RequestId, service::log::LogServiceError,
    utils::pagination::clamp_limit,
};

use super::{require_admin_session, AdminError, AdminPageEnvelope, AdminResponse};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
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
    match state.services.logs.list(query.cursor, limit).await {
        Ok(page) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminPageEnvelope::ok(page, limit, request_id),
        )),
        Err(error) => Err(log_service_error(error, request_id)),
    }
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
    }
}
