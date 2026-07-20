//! Provider instance 管理边界。

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use gateway_admin::model::{
    PageSize, Revision,
    catalog::{
        CatalogListQuery as CatalogQuery, CreateProviderInstance, DeleteProviderInstance,
        ProviderInstance, ProviderInstanceDetail, ProviderInstanceMutation, ProviderInstancePage,
        SetProviderInstanceEnabled, UpdateProviderInstance,
    },
};
use gateway_core::routing::{ProviderInstanceId, ProviderKind};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminSessionState, WireValidationError,
    wire::map_admin_service_error,
};

const DEFAULT_PAGE_SIZE: u16 = 50;
const MAX_PAGE_SIZE: u16 = 200;
const MAX_CURSOR_BYTES: usize = 1024;
const MAX_TEXT_BYTES: usize = 2048;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogListQuery {
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

impl CatalogListQuery {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        validate_cursor(self.cursor.as_deref())?;
        validate_limit(self.limit)
    }

    #[must_use]
    pub fn page_size(&self) -> u16 {
        self.limit.unwrap_or(DEFAULT_PAGE_SIZE)
    }

    fn into_command(self) -> Result<CatalogQuery, WireValidationError> {
        self.validate()?;
        let page_size =
            PageSize::new(self.page_size()).map_err(|_| WireValidationError::new("limit"))?;
        let cursor = self
            .cursor
            .map(ProviderInstanceId::new)
            .transpose()
            .map_err(|_| WireValidationError::new("catalogCursor"))?;
        Ok(CatalogQuery { cursor, page_size })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogIdQuery {
    pub id: String,
}

impl CatalogIdQuery {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_id(&self.id, "id")
    }

    fn into_id(self) -> Result<ProviderInstanceId, WireValidationError> {
        self.validate()?;
        ProviderInstanceId::new(self.id)
            .map_err(|_| WireValidationError::new("catalogDetailNotFound"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogMutationData {
    pub config_revision: u64,
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProviderInstanceRequest {
    pub id: String,
    pub expected_config_revision: u64,
    pub provider_kind: String,
    pub name: String,
    pub base_url: String,
}

impl CreateProviderInstanceRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_provider_instance_id(&self.id, "id")?;
        require_revision(self.expected_config_revision)?;
        require_provider_kind(&self.provider_kind)?;
        require_text(&self.name, "name")?;
        require_text(&self.base_url, "baseUrl")
    }

    fn into_command(self) -> Result<CreateProviderInstance, WireValidationError> {
        self.validate()?;
        Ok(CreateProviderInstance {
            expected_config_revision: revision(self.expected_config_revision)?,
            id: ProviderInstanceId::new(self.id).map_err(|_| WireValidationError::new("id"))?,
            provider_kind: ProviderKind::new(self.provider_kind)
                .map_err(|_| WireValidationError::new("providerKind"))?,
            name: self.name,
            base_url: self.base_url,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProviderInstanceRequest {
    pub id: String,
    pub expected_config_revision: u64,
    pub name: String,
    pub base_url: String,
}

impl UpdateProviderInstanceRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_provider_instance_id(&self.id, "id")?;
        require_revision(self.expected_config_revision)?;
        require_text(&self.name, "name")?;
        require_text(&self.base_url, "baseUrl")
    }

    fn into_command(self) -> Result<UpdateProviderInstance, WireValidationError> {
        self.validate()?;
        Ok(UpdateProviderInstance {
            expected_config_revision: revision(self.expected_config_revision)?,
            id: ProviderInstanceId::new(self.id).map_err(|_| WireValidationError::new("id"))?,
            name: self.name,
            base_url: self.base_url,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogRevisionRequest {
    pub id: String,
    pub expected_config_revision: u64,
}

impl CatalogRevisionRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_id(&self.id, "id")?;
        require_revision(self.expected_config_revision)
    }

    fn into_parts(self) -> Result<(Revision, ProviderInstanceId), WireValidationError> {
        self.validate()?;
        Ok((
            revision(self.expected_config_revision)?,
            ProviderInstanceId::new(self.id)
                .map_err(|_| WireValidationError::new("catalogMutationNotFound"))?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceView {
    pub id: String,
    pub provider_kind: String,
    pub name: String,
    pub base_url: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<ProviderInstance> for ProviderInstanceView {
    fn from(instance: ProviderInstance) -> Self {
        Self {
            id: instance.id.to_string(),
            provider_kind: instance.provider_kind.to_string(),
            name: instance.name,
            base_url: instance.base_url,
            enabled: instance.enabled,
            created_at: instance.created_at,
            updated_at: instance.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceListData {
    pub config_revision: u64,
    pub items: Vec<ProviderInstanceView>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceDetailData {
    pub config_revision: u64,
    pub item: ProviderInstanceView,
}

impl From<ProviderInstancePage> for ProviderInstanceListData {
    fn from(page: ProviderInstancePage) -> Self {
        Self {
            config_revision: page.config_revision.get(),
            items: page.items.into_iter().map(Into::into).collect(),
            next_cursor: page.next_cursor.map(|cursor| cursor.to_string()),
        }
    }
}

impl From<ProviderInstanceDetail> for ProviderInstanceDetailData {
    fn from(detail: ProviderInstanceDetail) -> Self {
        Self {
            config_revision: detail.config_revision.get(),
            item: detail.item.into(),
        }
    }
}

pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/api/admin/provider-instances",
            get(list_provider_instances::<S>),
        )
        .route(
            "/api/admin/provider-instances/detail",
            get(provider_instance::<S>),
        )
        .route(
            "/api/admin/provider-instances/create",
            post(create_provider_instance::<S>),
        )
        .route(
            "/api/admin/provider-instances/update",
            post(update_provider_instance::<S>),
        )
        .route(
            "/api/admin/provider-instances/enable",
            post(enable_provider_instance::<S>),
        )
        .route(
            "/api/admin/provider-instances/disable",
            post(disable_provider_instance::<S>),
        )
        .route(
            "/api/admin/provider-instances/delete",
            post(delete_provider_instance::<S>),
        )
}

async fn list_provider_instances<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<CatalogListQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .catalog()
        .list(query.into_command().map_err(map_wire_error)?)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProviderInstanceListData::from(result)),
    ))
}

async fn provider_instance<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<CatalogIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .catalog()
        .get(&id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProviderInstanceDetailData::from(result)),
    ))
}

async fn create_provider_instance<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<CreateProviderInstanceRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let id = command.id.to_string();
    let result = state
        .admin_services()
        .catalog()
        .create(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    mutation_response(StatusCode::CREATED, result, id)
}

async fn update_provider_instance<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<UpdateProviderInstanceRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let id = command.id.to_string();
    let result = state
        .admin_services()
        .catalog()
        .update(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    mutation_response(StatusCode::OK, result, id)
}

async fn enable_provider_instance<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<CatalogRevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    set_provider_instance_enabled(auth, state, request, true).await
}

async fn disable_provider_instance<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<CatalogRevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    set_provider_instance_enabled(auth, state, request, false).await
}

async fn set_provider_instance_enabled<S>(
    auth: AdminAuth,
    state: S,
    request: CatalogRevisionRequest,
    enabled: bool,
) -> Result<AdminResponse<AdminEnvelope<CatalogMutationData>>, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (expected_config_revision, id) = request.into_parts().map_err(map_wire_error)?;
    let response_id = id.to_string();
    let result = state
        .admin_services()
        .catalog()
        .set_enabled(
            &auth.context().mutation_context(),
            SetProviderInstanceEnabled {
                expected_config_revision,
                id,
                enabled,
            },
        )
        .await
        .map_err(map_service_error)?;
    mutation_response(StatusCode::OK, result, response_id)
}

async fn delete_provider_instance<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<CatalogRevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (expected_config_revision, id) = request.into_parts().map_err(map_wire_error)?;
    let response_id = id.to_string();
    let config_revision = state
        .admin_services()
        .catalog()
        .delete(
            &auth.context().mutation_context(),
            DeleteProviderInstance {
                expected_config_revision,
                id,
            },
        )
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(CatalogMutationData {
            config_revision: config_revision.get(),
            id: response_id,
        }),
    ))
}

fn mutation_response(
    status: StatusCode,
    result: ProviderInstanceMutation,
    fallback_id: String,
) -> Result<AdminResponse<AdminEnvelope<CatalogMutationData>>, AdminError> {
    let id = result
        .instance
        .map_or(fallback_id, |instance| instance.id.to_string());
    Ok(AdminResponse::new(
        status,
        AdminEnvelope::ok(CatalogMutationData {
            config_revision: result.config_revision.get(),
            id,
        }),
    ))
}

fn require_revision(value: u64) -> Result<(), WireValidationError> {
    if value == 0 || i64::try_from(value).is_err() {
        return Err(WireValidationError::new("expectedConfigRevision"));
    }
    Ok(())
}

fn revision(value: u64) -> Result<Revision, WireValidationError> {
    require_revision(value)?;
    Revision::new(value).map_err(|_| WireValidationError::new("expectedConfigRevision"))
}

fn require_id(value: &str, field: &'static str) -> Result<(), WireValidationError> {
    require_text(value, field)?;
    if value.starts_with("__")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn require_provider_instance_id(
    value: &str,
    field: &'static str,
) -> Result<(), WireValidationError> {
    ProviderInstanceId::new(value).map_or_else(|_| Err(WireValidationError::new(field)), |_| Ok(()))
}

fn require_text(value: &str, field: &'static str) -> Result<(), WireValidationError> {
    if value.trim().is_empty()
        || value.len() > MAX_TEXT_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn require_provider_kind(value: &str) -> Result<(), WireValidationError> {
    if matches!(value, "openai" | "xai") {
        Ok(())
    } else {
        Err(WireValidationError::new("providerKind"))
    }
}

fn validate_cursor(cursor: Option<&str>) -> Result<(), WireValidationError> {
    if cursor.is_some_and(|value| value.is_empty() || value.len() > MAX_CURSOR_BYTES) {
        Err(WireValidationError::new("cursor"))
    } else {
        Ok(())
    }
}

fn validate_limit(limit: Option<u16>) -> Result<(), WireValidationError> {
    if limit.is_some_and(|value| value == 0 || value > MAX_PAGE_SIZE) {
        Err(WireValidationError::new("limit"))
    } else {
        Ok(())
    }
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    match error.field() {
        "catalogCursor" => AdminError::bad_request("Invalid catalog cursor"),
        "catalogDetailNotFound" => AdminError::not_found("Provider instance was not found"),
        "catalogMutationNotFound" => AdminError::not_found("provider instance was not found"),
        field => AdminError::bad_request(format!("Invalid field: {field}")),
    }
}

fn map_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    if error.kind() == gateway_admin::model::AdminErrorKind::Invalid
        && error.to_string() == "Invalid provider instance cursor"
    {
        return AdminError::bad_request("Invalid catalog cursor");
    }
    map_admin_service_error(error, "Catalog unavailable")
}
