use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    admin::auth::api_key::{ApiKeyServiceError, ImportedClientApiKey},
    platform::{http::middleware::RequestId, identity::api_key_repository::StoredClientApiKey},
    runtime::state::AppState,
    utils::pagination::{clamp_limit, Page},
};

use super::{
    account_export_ids, require_admin_session, AdminEnvelope, AdminError, AdminPageEnvelope,
    AdminResponse,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeysQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyExportQuery {
    pub ids: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyRequest {
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyData {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedClientApiKeyData {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub plaintext: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyStatusRequest {
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyLabelRequest {
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyStatusData {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysData {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteClientApiKeyData {
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyExportData {
    pub source_format: &'static str,
    pub rotation_required: bool,
    pub api_keys: Vec<ClientApiKeyExportEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyExportEntry {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyImportData {
    pub imported: u32,
    pub skipped: u32,
    pub rotated: bool,
    pub keys: Vec<ImportedClientApiKeyData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedClientApiKeyData {
    pub source_id: Option<String>,
    pub source_prefix: Option<String>,
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub plaintext: String,
}

impl From<StoredClientApiKey> for ClientApiKeyData {
    fn from(key: StoredClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

impl From<StoredClientApiKey> for ClientApiKeyExportEntry {
    fn from(key: StoredClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

impl CreatedClientApiKeyData {
    fn new(key: StoredClientApiKey, plaintext: String) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
            plaintext,
        }
    }
}

impl ImportedClientApiKeyData {
    fn new(imported: ImportedClientApiKey) -> Self {
        let ImportedClientApiKey {
            key,
            plaintext,
            source_id,
            source_prefix,
        } = imported;
        Self {
            source_id,
            source_prefix,
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
            plaintext,
        }
    }
}

fn api_key_service_error(error: ApiKeyServiceError, request_id: String) -> AdminError {
    match error {
        ApiKeyServiceError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "API key repository is not initialized",
            request_id,
        ),
        ApiKeyServiceError::HasherUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "API key hasher is not initialized",
            request_id,
        ),
        ApiKeyServiceError::List => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list API keys",
            request_id,
        ),
        ApiKeyServiceError::Export => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to export API keys",
            request_id,
        ),
        ApiKeyServiceError::Import => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to import API key",
            request_id,
        ),
        ApiKeyServiceError::Create => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to create API key",
            request_id,
        ),
        ApiKeyServiceError::Delete => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to delete API key",
            request_id,
        ),
        ApiKeyServiceError::UpdateLabel => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update API key label",
            request_id,
        ),
        ApiKeyServiceError::UpdateStatus => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update API key status",
            request_id,
        ),
        ApiKeyServiceError::Verify => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to verify API key",
            request_id,
        ),
        ApiKeyServiceError::InvalidStatus(message) => {
            AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id)
        }
        ApiKeyServiceError::EmptyName => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "API key name is required",
            request_id,
        ),
        ApiKeyServiceError::EmptyIds => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "API key ids are required",
            request_id,
        ),
        ApiKeyServiceError::LabelTooLong => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "API key label must be 64 characters or fewer",
            request_id,
        ),
        ApiKeyServiceError::NoImportableKeys => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "No importable API keys found",
            request_id,
        ),
    }
}

fn api_key_not_found(request_id: String) -> AdminError {
    AdminError::new(
        StatusCode::NOT_FOUND,
        40401,
        "API key not found",
        request_id,
    )
}

pub async fn api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeysQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let limit = clamp_limit(query.limit.unwrap_or(50));
    match state.services.api_keys.list(query.cursor, limit).await {
        Ok(page) => {
            let Page { items, next_cursor } = page;
            let page = Page {
                items: items.into_iter().map(ClientApiKeyData::from).collect(),
                next_cursor,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            ))
        }
        Err(error) => Err(api_key_service_error(error, request_id)),
    }
}

pub async fn export_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeyExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let ids = account_export_ids(query.ids.as_deref());
    let keys = match state.services.api_keys.export(ids).await {
        Ok(keys) => keys,
        Err(error) => return Err(api_key_service_error(error, request_id)),
    };

    // 安全边界：本地 cpr_ key 只导出可展示元数据，绝不导出 plaintext、key_hash 或 pepper。
    let data = ClientApiKeyExportData {
        source_format: "rustLocalClientApiKeys",
        rotation_required: true,
        api_keys: keys
            .into_iter()
            .map(ClientApiKeyExportEntry::from)
            .collect(),
    };
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(data, request_id),
    ))
}

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
