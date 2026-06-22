//! 管理端 API Key 处理器（列表、创建、生命周期、导出、导入）。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    access::client_keys::{
        AdminClientKeyError, AdminCreatedClientApiKey, AdminStoredClientApiKey,
        ImportedClientApiKey,
    },
    admin::response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse},
    admin::session::require_admin_session,
    app::state::AppState,
    http::middleware::request_id::RequestId,
    infra::json::{clamp_limit, Page},
};

// ---- Query types ----

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysRequest {
    pub ids: Vec<String>,
}

// ---- Response types ----

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyStatusData {
    pub id: String,
    pub enabled: bool,
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

// ---- Conversions ----

impl From<AdminStoredClientApiKey> for ClientApiKeyData {
    fn from(k: AdminStoredClientApiKey) -> Self {
        Self {
            id: k.id,
            name: k.name,
            label: k.label,
            prefix: k.prefix,
            enabled: k.enabled,
            created_at: k.created_at,
            last_used_at: k.last_used_at,
        }
    }
}

impl From<AdminStoredClientApiKey> for ClientApiKeyExportEntry {
    fn from(k: AdminStoredClientApiKey) -> Self {
        Self {
            id: k.id,
            name: k.name,
            label: k.label,
            prefix: k.prefix,
            enabled: k.enabled,
            created_at: k.created_at,
            last_used_at: k.last_used_at,
        }
    }
}

impl From<AdminCreatedClientApiKey> for CreatedClientApiKeyData {
    fn from(k: AdminCreatedClientApiKey) -> Self {
        Self {
            id: k.id,
            name: k.name,
            label: k.label,
            prefix: k.prefix,
            enabled: k.enabled,
            created_at: k.created_at,
            last_used_at: k.last_used_at,
            plaintext: k.plaintext,
        }
    }
}

fn imported_client_key_data(imported: ImportedClientApiKey) -> ImportedClientApiKeyData {
    ImportedClientApiKeyData {
        source_id: imported.source_id,
        source_prefix: imported.source_prefix,
        id: imported.key.id,
        name: imported.key.name,
        label: imported.key.label,
        prefix: imported.key.prefix,
        enabled: imported.key.enabled,
        created_at: imported.key.created_at,
        last_used_at: imported.key.last_used_at,
        plaintext: imported.key.plaintext,
    }
}

fn client_key_error(error: AdminClientKeyError, request_id: String) -> AdminError {
    match error {
        AdminClientKeyError::InvalidStatus(_)
        | AdminClientKeyError::EmptyName
        | AdminClientKeyError::EmptyIds
        | AdminClientKeyError::LabelTooLong
        | AdminClientKeyError::NoImportableKeys => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        ),
        _ => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            error.to_string(),
            request_id,
        ),
    }
}

fn client_key_not_found(request_id: String) -> AdminError {
    AdminError::new(
        StatusCode::NOT_FOUND,
        40401,
        "Client API key not found",
        request_id,
    )
}

// ---- Handlers ----

/// `GET /api/admin/api-keys`
pub async fn api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeysQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let limit = clamp_limit(query.limit.unwrap_or(50));
    match state
        .services
        .admin_client_keys
        .list(query.cursor, limit)
        .await
    {
        Ok(page) => {
            let page = Page {
                items: page.items.into_iter().map(ClientApiKeyData::from).collect(),
                next_cursor: page.next_cursor,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            ))
        }
        Err(error) => Err(client_key_error(error, request_id)),
    }
}

/// `POST /api/admin/api-keys`
pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_client_keys.create(&payload.name).await {
        Ok(created) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(CreatedClientApiKeyData::from(created), request_id),
        )),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}

/// `DELETE /api/admin/api-keys/{key_id}`
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

/// `PATCH /api/admin/api-keys/{key_id}/label`
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

/// `PATCH /api/admin/api-keys/{key_id}/status`
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

/// `POST /api/admin/api-keys/batch-delete`
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

/// `GET /api/admin/api-keys/export`
pub async fn export_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeyExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let ids = query
        .ids
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect();
    match state.services.admin_client_keys.export(ids).await {
        Ok(keys) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                ClientApiKeyExportData {
                    source_format: "rustLocalClientApiKeys",
                    rotation_required: true,
                    api_keys: keys
                        .into_iter()
                        .map(ClientApiKeyExportEntry::from)
                        .collect(),
                },
                request_id,
            ),
        )),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}

/// `POST /api/admin/api-keys/import`
pub async fn import_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_client_keys.import(&payload).await {
        Ok(imported) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                ClientApiKeyImportData {
                    imported: imported.imported,
                    skipped: imported.skipped,
                    rotated: true,
                    keys: imported
                        .keys
                        .into_iter()
                        .map(imported_client_key_data)
                        .collect(),
                },
                request_id,
            ),
        )),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}
