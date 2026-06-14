use axum::{
    extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse, Extension, Json,
};

use crate::{platform::http::request_id::RequestId, runtime::state::AppState};

use super::{api_key_service_error, CreateApiKeyRequest, CreatedClientApiKeyData};
use crate::admin::api::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};

pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.api_keys.create(&payload.name).await {
        Ok(created) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                CreatedClientApiKeyData::new(created.key, created.plaintext),
                request_id,
            ),
        )),
        Err(error) => Err(api_key_service_error(error, request_id)),
    }
}
