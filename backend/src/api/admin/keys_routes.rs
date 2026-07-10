//! 管理端 v1 接口访问 Key 处理器（列表、创建、生命周期）。

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api::admin::response::{
        parse_editable_update, AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse,
        BatchDeleteData, EditableUpdateMessages,
    },
    api::admin::session::AdminAuth,
    api::AppState,
    infra::{
        json::{clamp_limit, Page},
        time::{china_relative_time_str, china_rfc3339_str},
    },
    keys::types::{KeyManageError, ManagedClientApiKey},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiKeysQuery {
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateApiKeyRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchDeleteClientApiKeysRequest {
    ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientApiKeyData {
    id: String,
    name: String,
    label: Option<String>,
    prefix: String,
    key: String,
    enabled: bool,
    created_at: String,
    created_at_display: String,
    last_used_at: Option<String>,
    last_used_at_display: String,
}

impl From<ManagedClientApiKey> for ClientApiKeyData {
    fn from(k: ManagedClientApiKey) -> Self {
        Self {
            id: k.id,
            name: k.name,
            label: k.label,
            prefix: k.prefix,
            key: k.key,
            enabled: k.enabled,
            created_at_display: china_relative_time_str(Some(&k.created_at)),
            created_at: china_rfc3339_str(&k.created_at),
            last_used_at_display: china_relative_time_str(k.last_used_at.as_deref()),
            last_used_at: k.last_used_at.as_deref().map(china_rfc3339_str),
        }
    }
}

fn client_key_error(error: &KeyManageError) -> AdminError {
    match error {
        KeyManageError::InvalidStatus(_)
        | KeyManageError::EmptyName
        | KeyManageError::EmptyIds
        | KeyManageError::LabelTooLong => AdminError::bad_request(error.to_string()),
        _ => AdminError::internal(error.to_string()),
    }
}

fn client_key_not_found() -> AdminError {
    AdminError::not_found("Client API key not found")
}

struct ParsedClientApiKeyUpdate {
    id: String,
    label: Option<Option<String>>,
    status: Option<String>,
}

fn parse_client_api_key_update(payload: &Value) -> Result<ParsedClientApiKeyUpdate, AdminError> {
    let update = parse_editable_update(
        payload,
        EditableUpdateMessages {
            object_required: "Client API key update request must be an object",
            invalid: "Invalid client API key update request",
            empty_update: "Client API key update request must include label or status",
            unknown_field_editable: false,
        },
    )?;
    Ok(ParsedClientApiKeyUpdate {
        id: update.id,
        label: update.label,
        status: update.status,
    })
}

/// `GET /api/admin/keys`
pub(crate) async fn api_keys(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<ApiKeysQuery>,
) -> Result<impl IntoResponse, AdminError> {
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
                AdminPageEnvelope::ok(page, limit),
            ))
        }
        Err(error) => Err(client_key_error(&error)),
    }
}

/// `POST /api/admin/keys`
pub(crate) async fn create_api_key(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<impl IntoResponse, AdminError> {
    match state.services.admin_client_keys.create(&payload.name).await {
        Ok(created) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(ClientApiKeyData::from(created)),
        )),
        Err(error) => Err(client_key_error(&error)),
    }
}

/// `POST /api/admin/keys/update`
pub(crate) async fn update_api_key(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let update = parse_client_api_key_update(&payload)?;
    if let Some(label) = update.label {
        match state
            .services
            .admin_client_keys
            .update_label(&update.id, label)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return Err(client_key_not_found()),
            Err(error) => return Err(client_key_error(&error)),
        }
    }
    if let Some(status) = update.status {
        match state
            .services
            .admin_client_keys
            .update_status(&update.id, &status)
            .await
        {
            Ok(true) => {}
            Ok(false) => return Err(client_key_not_found()),
            Err(error) => return Err(client_key_error(&error)),
        }
    }

    match state.services.admin_client_keys.get(&update.id).await {
        Ok(Some(key)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(ClientApiKeyData::from(key)),
        )),
        Ok(None) => Err(client_key_not_found()),
        Err(error) => Err(client_key_error(&error)),
    }
}

/// `POST /api/admin/keys/delete`
pub(crate) async fn batch_delete_api_keys(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<BatchDeleteClientApiKeysRequest>,
) -> Result<impl IntoResponse, AdminError> {
    match state
        .services
        .admin_client_keys
        .batch_delete(payload.ids)
        .await
    {
        Ok(deleted) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(BatchDeleteData {
                deleted: deleted.deleted,
                not_found: deleted.not_found,
            }),
        )),
        Err(error) => Err(client_key_error(&error)),
    }
}
