use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::Serialize;

use crate::{
    app::state::AppState,
    codex::models::service::{ModelRefreshResult, ModelServiceError},
    http::middleware::RequestId,
};

use super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshModelsData {
    pub refreshed_plans: usize,
    pub model_count: usize,
    pub failed_plans: usize,
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

pub async fn refresh_models(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state
        .services
        .models
        .refresh_backend_models(&request_id)
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(RefreshModelsData::from(result), request_id),
        )),
        Err(error) => model_service_error_response(error, request_id),
    }
}

fn model_service_error_response(
    error: ModelServiceError,
    request_id: String,
) -> Result<AdminResponse<AdminEnvelope<RefreshModelsData>>, AdminError> {
    match error {
        ModelServiceError::AccountRepositoryUnavailable => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        )),
        ModelServiceError::ModelRepositoryUnavailable => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Model repository is not initialized",
            request_id,
        )),
        ModelServiceError::ListAccounts => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list accounts",
            request_id,
        )),
        ModelServiceError::NoAccounts => Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "No accounts available for model refresh",
            request_id,
        )),
        ModelServiceError::BuildClient => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to build Codex client",
            request_id,
        )),
        ModelServiceError::StoreSnapshot => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to store model snapshot",
            request_id,
        )),
        ModelServiceError::AllPlansFailed(result) => Ok(AdminResponse::new(
            StatusCode::BAD_GATEWAY,
            AdminEnvelope::new(
                50201,
                "Failed to refresh backend models",
                RefreshModelsData::from(result),
                request_id,
            ),
        )),
        ModelServiceError::LoadSnapshots => Err(AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load model snapshots",
            request_id,
        )),
    }
}
