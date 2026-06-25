//! 管理端 v1 接口访问 Key 处理器（列表、创建、生命周期、导出）。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    admin::auth::session::require_admin_session,
    admin::keys::service::{
        AdminClientKeyError, AdminCreatedClientApiKey, AdminStoredClientApiKey,
    },
    admin::response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse},
    http::middleware::request_id::RequestId,
    infra::{
        json::{clamp_limit, Page},
        time::{china_relative_time_str, china_rfc3339_str},
    },
    runtime::state::AppState,
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
    pub created_at_display: String,
    pub last_used_at: Option<String>,
    pub last_used_at_display: String,
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
    pub created_at_display: String,
    pub last_used_at: Option<String>,
    pub last_used_at_display: String,
    pub plaintext: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysData {
    pub deleted: u32,
    pub not_found: Vec<String>,
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

// ---- Conversions ----

impl From<AdminStoredClientApiKey> for ClientApiKeyData {
    fn from(k: AdminStoredClientApiKey) -> Self {
        Self {
            id: k.id,
            name: k.name,
            label: k.label,
            prefix: k.prefix,
            enabled: k.enabled,
            created_at_display: china_relative_time_str(Some(&k.created_at)),
            created_at: china_rfc3339_str(&k.created_at),
            last_used_at_display: china_relative_time_str(k.last_used_at.as_deref()),
            last_used_at: k.last_used_at.as_deref().map(china_rfc3339_str),
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
            created_at: china_rfc3339_str(&k.created_at),
            last_used_at: k.last_used_at.as_deref().map(china_rfc3339_str),
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
            created_at_display: china_relative_time_str(Some(&k.created_at)),
            created_at: china_rfc3339_str(&k.created_at),
            last_used_at_display: china_relative_time_str(k.last_used_at.as_deref()),
            last_used_at: k.last_used_at.as_deref().map(china_rfc3339_str),
            plaintext: k.plaintext,
        }
    }
}

fn client_key_error(error: AdminClientKeyError, request_id: String) -> AdminError {
    match error {
        AdminClientKeyError::InvalidStatus(_)
        | AdminClientKeyError::EmptyName
        | AdminClientKeyError::EmptyIds
        | AdminClientKeyError::LabelTooLong => AdminError::new(
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

struct ParsedClientApiKeyUpdate {
    id: String,
    label: Option<Option<String>>,
    status: Option<String>,
}

fn parse_client_api_key_update(
    payload: Value,
    request_id: &str,
) -> Result<ParsedClientApiKeyUpdate, AdminError> {
    let object = payload.as_object().ok_or_else(|| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Client API key update request must be an object",
            request_id,
        )
    })?;
    let id = required_string_field(object, "id", request_id)?;
    let label = object
        .get("label")
        .map(|value| optional_string_field(value, "label", request_id))
        .transpose()?;
    let status = object
        .get("status")
        .map(|value| required_string_value(value, "status", request_id))
        .transpose()?;
    if label.is_none() && status.is_none() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Client API key update request must include label or status",
            request_id,
        ));
    }
    Ok(ParsedClientApiKeyUpdate { id, label, status })
}

fn required_string_field(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    request_id: &str,
) -> Result<String, AdminError> {
    let Some(value) = object.get(field) else {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("{field} is required"),
            request_id,
        ));
    };
    required_string_value(value, field, request_id)
}

fn required_string_value(
    value: &Value,
    field: &'static str,
    request_id: &str,
) -> Result<String, AdminError> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                format!("{field} must be a non-empty string"),
                request_id,
            )
        })
}

fn optional_string_field(
    value: &Value,
    field: &'static str,
    request_id: &str,
) -> Result<Option<String>, AdminError> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(ToString::to_string)
        .map(Some)
        .ok_or_else(|| {
            AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                format!("{field} must be a string or null"),
                request_id,
            )
        })
}

// ---- Handlers ----

/// `GET /api/admin/keys`
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

/// `POST /api/admin/keys`
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

/// `POST /api/admin/keys/update`
pub async fn update_api_key(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let update = parse_client_api_key_update(payload, &request_id)?;
    if let Some(label) = update.label {
        match state
            .services
            .admin_client_keys
            .update_label(&update.id, label)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return Err(client_key_not_found(request_id)),
            Err(error) => return Err(client_key_error(error, request_id)),
        }
    }
    if let Some(status) = update.status {
        match state
            .services
            .admin_client_keys
            .update_status(&update.id, &status)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return Err(client_key_not_found(request_id)),
            Err(error) => return Err(client_key_error(error, request_id)),
        }
    }

    match state.services.admin_client_keys.get(&update.id).await {
        Ok(Some(key)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(ClientApiKeyData::from(key), request_id),
        )),
        Ok(None) => Err(client_key_not_found(request_id)),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}

/// `POST /api/admin/keys/delete`
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

/// `GET /api/admin/keys/export`
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
