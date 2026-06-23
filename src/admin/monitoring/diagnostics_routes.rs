//! 管理端诊断处理器。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};

use crate::{
    admin::auth::session::require_admin_session,
    admin::monitoring::diagnostics::{diagnostics_data, DiagnosticsInput},
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    http::middleware::request_id::RequestId,
    runtime::state::AppState,
};

/// `GET /api/admin/diagnostics`
pub async fn diagnostics(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let config = state.services.settings.current();
    let accounts = state
        .services
        .accounts
        .list_pool_accounts()
        .await
        .unwrap_or_default();
    let capacity = state.services.account_pool.capacity_summary_now().await;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            diagnostics_data(DiagnosticsInput {
                config: config.as_ref(),
                accounts: &accounts,
                capacity,
                fingerprint: &state.services.fingerprint,
            }),
            request_id,
        ),
    ))
}
