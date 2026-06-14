use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};

use crate::{
    codex::serving::http::diagnostics::diagnostics_data, platform::http::request_id::RequestId,
    runtime::state::AppState,
};

use super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};

pub async fn diagnostics(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(diagnostics_data(&state).await, request_id),
    ))
}
