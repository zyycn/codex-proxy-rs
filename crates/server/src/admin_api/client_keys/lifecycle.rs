//! 客户端 key 生命周期处理器。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use codex_proxy_runtime::state::AppState;

use crate::{
    admin_api::{
        client_keys::{
            client_key_error, client_key_not_found, BatchDeleteClientApiKeysData,
            BatchDeleteClientApiKeysRequest, ClientApiKeyData, DeleteClientApiKeyData,
            UpdateClientApiKeyLabelRequest, UpdateClientApiKeyStatusData,
            UpdateClientApiKeyStatusRequest,
        },
        require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 批量删除客户端 API Key。
pub async fn batch_delete_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchDeleteClientApiKeysRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_client_keys
        .batch_delete(payload.ids)
        .await
    {
        Ok(deleted) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                BatchDeleteClientApiKeysData {
                    deleted: deleted.deleted,
                    not_found: deleted.not_found,
                },
                request_id,
            ),
        )),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}

/// 更新客户端 API Key 标签。
pub async fn update_api_key_label(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
    Json(payload): Json<UpdateClientApiKeyLabelRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_client_keys
        .update_label(&key_id, payload.label)
        .await
    {
        Ok(Some(key)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(ClientApiKeyData::from(key), request_id),
        )),
        Ok(None) => Err(client_key_not_found(request_id)),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}

/// 更新客户端 API Key 启用状态。
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
        .admin_client_keys
        .update_status(&key_id, &payload.status)
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
        Ok(None) => Err(client_key_not_found(request_id)),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}

/// 删除客户端 API Key。
pub async fn delete_api_key(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_client_keys.delete(&key_id).await {
        Ok(true) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(DeleteClientApiKeyData { deleted: true }, request_id),
        )),
        Ok(false) => Err(client_key_not_found(request_id)),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}
