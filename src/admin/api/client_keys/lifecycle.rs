use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};

use crate::{platform::http::request_id::RequestId, runtime::state::AppState};

use super::{
    api_key_not_found, api_key_service_error, BatchDeleteClientApiKeysData,
    BatchDeleteClientApiKeysRequest, ClientApiKeyData, DeleteClientApiKeyData,
    UpdateClientApiKeyLabelRequest, UpdateClientApiKeyStatusData, UpdateClientApiKeyStatusRequest,
};
use crate::admin::api::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};

pub async fn batch_delete_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchDeleteClientApiKeysRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let deleted = match state.services.api_keys.batch_delete(payload.ids).await {
        Ok(deleted) => deleted,
        Err(error) => return Err(api_key_service_error(error, request_id)),
    };

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            BatchDeleteClientApiKeysData {
                deleted: deleted.deleted,
                not_found: deleted.not_found,
            },
            request_id,
        ),
    ))
}

pub async fn update_api_key_label(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
    Json(payload): Json<UpdateClientApiKeyLabelRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    // label 是后台显示备注，不能影响 cpr_ key 的 hash、prefix 或启用状态。
    match state
        .services
        .api_keys
        .update_label(&key_id, payload.label)
        .await
    {
        Ok(Some(key)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(ClientApiKeyData::from(key), request_id),
        )),
        Ok(None) => Err(api_key_not_found(request_id)),
        Err(error) => Err(api_key_service_error(error, request_id)),
    }
}

pub async fn update_api_key_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
    Json(payload): Json<UpdateClientApiKeyStatusRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state
        .services
        .api_keys
        .update_status(key_id, &payload.status)
        .await
    {
        Ok(Some(updated)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                UpdateClientApiKeyStatusData {
                    id: updated.id,
                    enabled: updated.enabled,
                },
                request_id,
            ),
        )),
        Ok(None) => Err(api_key_not_found(request_id)),
        Err(error) => Err(api_key_service_error(error, request_id)),
    }
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state.services.api_keys.delete(&key_id).await {
        Ok(true) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(DeleteClientApiKeyData { deleted: true }, request_id),
        )),
        Ok(false) => Err(api_key_not_found(request_id)),
        Err(error) => Err(api_key_service_error(error, request_id)),
    }
}
