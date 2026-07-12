//! 管理端 v1 接口访问 Key 处理器（列表、创建、生命周期）。

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api::AppState,
    api::admin::response::{
        AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse, BatchDeleteData,
        EditableUpdateMessages, parse_editable_update,
    },
    api::admin::session::AdminAuth,
    infra::{
        json::{NumberedPage, SortDirection, clamp_limit, clamp_page},
        time::{china_relative_time_str, china_rfc3339_str},
    },
    keys::types::{
        ClientApiKeyListSort, ClientApiKeySortField, KeyManageError, ManagedClientApiKey,
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiKeysQuery {
    page: Option<u32>,
    page_size: Option<u32>,
    search: Option<String>,
    sort_by: Option<String>,
    sort_direction: Option<String>,
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
    let page = clamp_page(query.page.unwrap_or(1));
    let page_size = clamp_limit(query.page_size.unwrap_or(50));
    let sort = client_api_key_list_sort(query.sort_by, query.sort_direction)?;
    match state
        .services
        .admin_client_keys
        .list_page(page, page_size, query.search, sort)
        .await
    {
        Ok(page) => {
            let page = NumberedPage {
                items: page.items.into_iter().map(ClientApiKeyData::from).collect(),
                total: page.total,
                page: page.page,
                page_size: page.page_size,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page),
            ))
        }
        Err(error) => Err(client_key_error(&error)),
    }
}

fn client_api_key_list_sort(
    sort_by: Option<String>,
    sort_direction: Option<String>,
) -> Result<Option<ClientApiKeyListSort>, AdminError> {
    let (sort_by, sort_direction) = match (sort_by, sort_direction) {
        (None, None) => return Ok(None),
        (Some(sort_by), Some(sort_direction)) => (sort_by, sort_direction),
        _ => {
            return Err(AdminError::bad_request(
                "Client API key sort field and direction must be provided together",
            ));
        }
    };
    let field = match sort_by.trim() {
        "name" => ClientApiKeySortField::Name,
        "enabled" => ClientApiKeySortField::Enabled,
        "createdAt" => ClientApiKeySortField::CreatedAt,
        "lastUsedAt" => ClientApiKeySortField::LastUsedAt,
        _ => return Err(AdminError::bad_request("Invalid client API key sort field")),
    };
    let direction = SortDirection::parse(&sort_direction)
        .ok_or_else(|| AdminError::bad_request("Invalid client API key sort direction"))?;
    Ok(Some(ClientApiKeyListSort { field, direction }))
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
