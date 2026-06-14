use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};

use crate::{platform::http::request_id::RequestId, runtime::state::AppState};

use super::log_service_error;
use crate::admin::api::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};

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
