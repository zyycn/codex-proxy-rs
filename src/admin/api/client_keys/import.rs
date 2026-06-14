use axum::{
    extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse, Extension, Json,
};
use serde_json::Value;

use crate::{platform::http::request_id::RequestId, runtime::state::AppState};

use super::{api_key_service_error, ClientApiKeyImportData, ImportedClientApiKeyData};
use crate::admin::api::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};

pub async fn import_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let imported = match state.services.api_keys.import(&payload).await {
        Ok(imported) => imported,
        Err(error) => return Err(api_key_service_error(error, request_id)),
    };

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            ClientApiKeyImportData {
                imported: imported.imported,
                skipped: imported.skipped,
                rotated: true,
                keys: imported
                    .keys
                    .into_iter()
                    .map(ImportedClientApiKeyData::new)
                    .collect(),
            },
            request_id,
        ),
    ))
}
