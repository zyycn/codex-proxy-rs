//! 管理端诊断处理器。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_runtime::state::AppState;

use crate::{
    admin_api::{require_admin_session, AdminEnvelope, AdminError, AdminResponse},
    middleware::request_id::RequestId,
    openai_api::diagnostics::diagnostics_data,
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
