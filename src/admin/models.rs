//! 管理端模型处理器。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::Serialize;

use crate::{
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    admin::session::require_admin_session,
    app::state::AppState,
    codex::models::ModelRefreshResult,
    http::middleware::request_id::RequestId,
};

use crate::app::services::AdminModelError;

/// 模型刷新响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshModelsData {
    pub refreshed_plans: usize,
    pub model_count: usize,
    pub failed_plans: usize,
}

/// `POST /api/admin/refresh-models`
pub async fn refresh_models(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state
        .services
        .admin_models
        .refresh_backend_models(&request_id)
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(RefreshModelsData::from(result), request_id),
        )),
        Err(error) => model_error(error, request_id),
    }
}

fn model_error(
    error: AdminModelError,
    request_id: String,
) -> Result<AdminResponse<AdminEnvelope<RefreshModelsData>>, AdminError> {
    match error {
        AdminModelError::NoAccounts => Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        )),
        AdminModelError::AllPlansFailed(result) => Ok(AdminResponse::new(
            StatusCode::BAD_GATEWAY,
            AdminEnvelope::new(
                50201,
                "Failed to refresh backend models",
                RefreshModelsData::from(result),
                request_id,
            ),
        )),
        AdminModelError::ListAccounts
        | AdminModelError::SnapshotStoreUnavailable
        | AdminModelError::UpstreamClientUnavailable
        | AdminModelError::StoreSnapshot
        | AdminModelError::LoadSnapshots => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            error.to_string(),
            request_id,
        )),
    }
}

impl From<ModelRefreshResult> for RefreshModelsData {
    fn from(result: ModelRefreshResult) -> Self {
        Self {
            refreshed_plans: result.refreshed_plans,
            model_count: result.model_count,
            failed_plans: result.failed_plans,
        }
    }
}
