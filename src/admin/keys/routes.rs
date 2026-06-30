//! 管理端 v1 接口访问 Key 处理器（列表、创建、生命周期）。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use validator::{Validate, ValidationError};

use crate::{
    admin::auth::session::require_admin_auth,
    admin::keys::service::{AdminClientApiKey, AdminClientKeyError},
    admin::response::{
        AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse, BatchDeleteData,
    },
    infra::{
        json::{clamp_limit, Page},
        time::{china_relative_time_str, china_rfc3339_str},
    },
    runtime::state::AppState,
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

impl From<AdminClientApiKey> for ClientApiKeyData {
    fn from(k: AdminClientApiKey) -> Self {
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

fn client_key_error(error: &AdminClientKeyError) -> AdminError {
    match error {
        AdminClientKeyError::InvalidStatus(_)
        | AdminClientKeyError::EmptyName
        | AdminClientKeyError::EmptyIds
        | AdminClientKeyError::LabelTooLong => AdminError::bad_request(error.to_string()),
        _ => AdminError::internal(error.to_string()),
    }
}

fn client_key_not_found() -> AdminError {
    AdminError::not_found("Client API key not found")
}

#[derive(Debug, Deserialize, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientApiKeyUpdateRequest {
    #[validate(custom(function = "validate_non_empty_trimmed"))]
    id: String,
    label: Option<Option<String>>,
    #[validate(custom(function = "validate_non_empty_trimmed"))]
    status: Option<String>,
}

struct ParsedClientApiKeyUpdate {
    id: String,
    label: Option<Option<String>>,
    status: Option<String>,
}

fn parse_client_api_key_update(payload: &Value) -> Result<ParsedClientApiKeyUpdate, AdminError> {
    if !payload.is_object() {
        return Err(AdminError::bad_request(
            "Client API key update request must be an object",
        ));
    }
    let update = serde_json::from_value::<ClientApiKeyUpdateRequest>(payload.clone())
        .map_err(|_| AdminError::bad_request("Invalid client API key update request"))?;
    update
        .validate()
        .map_err(|_| AdminError::bad_request("Invalid client API key update request"))?;
    if update.label.is_none() && update.status.is_none() {
        return Err(AdminError::bad_request(
            "Client API key update request must include label or status",
        ));
    }
    Ok(ParsedClientApiKeyUpdate {
        id: update.id,
        label: update.label,
        status: update.status,
    })
}

fn validate_non_empty_trimmed(value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new("required"));
    }
    Ok(())
}

/// `GET /api/admin/keys`
pub(crate) async fn api_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ApiKeysQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
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
    headers: HeaderMap,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
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
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;

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
    headers: HeaderMap,
    Json(payload): Json<BatchDeleteClientApiKeysRequest>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
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
