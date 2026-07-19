//! Provider instance 管理边界。

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use gateway_core::routing::ProviderInstanceId;
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminRequestContext, AdminResponse, AdminServiceError,
    AdminServiceErrorKind, AdminSessionState, WireValidationError,
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

#[async_trait]
pub trait CatalogAdminService: Send + Sync {
    async fn list_provider_instances(
        &self,
        query: CatalogListQuery,
    ) -> Result<ProviderInstanceListData, AdminServiceError>;
    async fn provider_instance(
        &self,
        id: String,
    ) -> Result<ProviderInstanceDetailData, AdminServiceError>;
    async fn create_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: CreateProviderInstanceRequest,
    ) -> Result<CatalogMutationData, AdminServiceError>;
    async fn update_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: UpdateProviderInstanceRequest,
    ) -> Result<CatalogMutationData, AdminServiceError>;
    async fn enable_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: CatalogRevisionRequest,
    ) -> Result<CatalogMutationData, AdminServiceError>;
    async fn disable_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: CatalogRevisionRequest,
    ) -> Result<CatalogMutationData, AdminServiceError>;
    async fn delete_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: CatalogRevisionRequest,
    ) -> Result<CatalogMutationData, AdminServiceError>;
}

pub trait CatalogAdminState: AdminSessionState {
    fn catalog_admin_service(&self) -> &dyn CatalogAdminService;
}

pub fn router<S>() -> Router<S>
where
    S: CatalogAdminState + Clone + Send + Sync + 'static,
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

macro_rules! get_handler {
    ($name:ident, $query:ty, $method:ident) => {
        async fn $name<S>(
            _auth: AdminAuth,
            State(state): State<S>,
            Query(query): Query<$query>,
        ) -> Result<impl IntoResponse, AdminError>
        where
            S: CatalogAdminState + Send + Sync,
        {
            query.validate().map_err(map_wire_error)?;
            let data = state
                .catalog_admin_service()
                .$method(query)
                .await
                .map_err(map_service_error)?;
            Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
        }
    };
}

macro_rules! get_id_handler {
    ($name:ident, $method:ident) => {
        async fn $name<S>(
            _auth: AdminAuth,
            State(state): State<S>,
            Query(query): Query<CatalogIdQuery>,
        ) -> Result<impl IntoResponse, AdminError>
        where
            S: CatalogAdminState + Send + Sync,
        {
            query.validate().map_err(map_wire_error)?;
            let data = state
                .catalog_admin_service()
                .$method(query.id)
                .await
                .map_err(map_service_error)?;
            Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
        }
    };
}

macro_rules! post_handler {
    ($name:ident, $request:ty, $method:ident, $status:expr) => {
        async fn $name<S>(
            auth: AdminAuth,
            State(state): State<S>,
            Json(request): Json<$request>,
        ) -> Result<impl IntoResponse, AdminError>
        where
            S: CatalogAdminState + Send + Sync,
        {
            request.validate().map_err(map_wire_error)?;
            let data = state
                .catalog_admin_service()
                .$method(auth.context(), request)
                .await
                .map_err(map_service_error)?;
            Ok(AdminResponse::new($status, AdminEnvelope::ok(data)))
        }
    };
}

get_handler!(
    list_provider_instances,
    CatalogListQuery,
    list_provider_instances
);
get_id_handler!(provider_instance, provider_instance);
post_handler!(
    create_provider_instance,
    CreateProviderInstanceRequest,
    create_provider_instance,
    StatusCode::CREATED
);
post_handler!(
    update_provider_instance,
    UpdateProviderInstanceRequest,
    update_provider_instance,
    StatusCode::OK
);
post_handler!(
    enable_provider_instance,
    CatalogRevisionRequest,
    enable_provider_instance,
    StatusCode::OK
);
post_handler!(
    disable_provider_instance,
    CatalogRevisionRequest,
    disable_provider_instance,
    StatusCode::OK
);
post_handler!(
    delete_provider_instance,
    CatalogRevisionRequest,
    delete_provider_instance,
    StatusCode::OK
);

fn require_revision(value: u64) -> Result<(), WireValidationError> {
    if value == 0 || i64::try_from(value).is_err() {
        return Err(WireValidationError::new("expectedConfigRevision"));
    }
    Ok(())
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
    AdminError::bad_request(format!("Invalid field: {}", error.field()))
}

fn map_service_error(error: AdminServiceError) -> AdminError {
    match error.kind() {
        AdminServiceErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminServiceErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminServiceErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminServiceErrorKind::Unavailable => {
            AdminError::service_unavailable("Catalog unavailable")
        }
        AdminServiceErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}
