//! 管理端诊断处理器。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};

use crate::{
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    admin::session::require_admin_session,
    app::state::AppState,
    gateway::openai::diagnostics_data,
    http::middleware::request_id::RequestId,
};

/// `GET /api/admin/diagnostics`
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
