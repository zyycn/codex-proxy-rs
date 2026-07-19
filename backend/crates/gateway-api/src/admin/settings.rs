//! Runtime settings、旧设置页聚合投影与明文 Admin API Key wire。

use std::{collections::BTreeMap, fmt};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminRequestContext, AdminResponse, AdminServiceError,
    AdminServiceErrorKind, AdminSessionState, WireValidationError,
};

/// 按 Provider 分组的精确模型映射：Provider → 客户端模型 → 上游模型。
pub type ProviderModelMappings = BTreeMap<String, BTreeMap<String, String>>;

/// 运行配置投影与设置页字段的聚合响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSettingsView {
    pub config_revision: u64,
    pub provider_model_mappings: ProviderModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u64,
    pub max_concurrent_per_account: u64,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub usage_retention_days: u64,
    pub ops_event_retention_days: u64,
    pub audit_retention_days: u64,
    pub updated_at: DateTime<Utc>,
}

/// 原子替换全局运行参数的请求。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRuntimeSettingsRequest {
    pub expected_config_revision: u64,
    pub provider_model_mappings: ProviderModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u64,
    pub max_concurrent_per_account: u64,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub usage_retention_days: u64,
    pub ops_event_retention_days: u64,
    pub audit_retention_days: u64,
}

impl UpdateRuntimeSettingsRequest {
    /// 校验公共运行参数。
    pub fn validate(&self) -> Result<(), WireValidationError> {
        if self.expected_config_revision == 0 {
            return Err(WireValidationError::new("expectedConfigRevision"));
        }
        validate_provider_model_mappings(&self.provider_model_mappings)?;
        for (value, field) in [
            (self.refresh_margin_seconds, "refreshMarginSeconds"),
            (self.refresh_concurrency, "refreshConcurrency"),
            (self.max_concurrent_per_account, "maxConcurrentPerAccount"),
            (self.usage_retention_days, "usageRetentionDays"),
            (self.ops_event_retention_days, "opsEventRetentionDays"),
            (self.audit_retention_days, "auditRetentionDays"),
        ] {
            require_positive_i64(value, field)?;
        }
        if i64::try_from(self.request_interval_ms).is_err() {
            return Err(WireValidationError::new("requestIntervalMs"));
        }
        if !matches!(
            self.rotation_strategy.as_str(),
            "smart" | "quota_reset_priority" | "round_robin" | "sticky"
        ) {
            return Err(WireValidationError::new("rotationStrategy"));
        }
        Ok(())
    }
}

/// 管理 API Key 状态；状态读取不回显完整值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminApiKeyStatus {
    pub exists: bool,
}

/// 管理 API Key 重新生成响应。
#[derive(Serialize)]
pub struct RegeneratedAdminApiKey {
    pub key: String,
}

impl fmt::Debug for RegeneratedAdminApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegeneratedAdminApiKey")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

/// 管理 API Key 删除响应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DeletedAdminApiKey {
    pub message: &'static str,
}

impl Default for DeletedAdminApiKey {
    fn default() -> Self {
        Self {
            message: "Admin API key deleted",
        }
    }
}

/// 设置页聚合和明文 Admin API Key 管理应用端口。
#[async_trait]
pub trait AdminSettingsService: Send + Sync {
    async fn load(&self) -> Result<RuntimeSettingsView, AdminServiceError>;

    async fn replace(
        &self,
        context: &AdminRequestContext,
        request: UpdateRuntimeSettingsRequest,
    ) -> Result<RuntimeSettingsView, AdminServiceError>;

    async fn admin_api_key_exists(&self) -> Result<bool, AdminServiceError>;

    async fn regenerate_admin_api_key(
        &self,
        context: &AdminRequestContext,
    ) -> Result<String, AdminServiceError>;

    async fn delete_admin_api_key(
        &self,
        context: &AdminRequestContext,
    ) -> Result<(), AdminServiceError>;
}

/// 设置 HTTP module 所需最小 state。
pub trait AdminSettingsState: AdminSessionState {
    fn admin_settings_service(&self) -> &dyn AdminSettingsService;
}

/// 构造固定 GET/POST 设置路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSettingsState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/api/admin/settings",
            get(settings::<S>).post(update_settings::<S>),
        )
        .route(
            "/api/admin/settings/admin-api-key",
            get(admin_api_key_status::<S>).post(delete_admin_api_key::<S>),
        )
        .route(
            "/api/admin/settings/admin-api-key/regenerate",
            post(regenerate_admin_api_key::<S>),
        )
}

async fn settings<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSettingsState + Send + Sync,
{
    let data = state
        .admin_settings_service()
        .load()
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn update_settings<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<UpdateRuntimeSettingsRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSettingsState + Send + Sync,
{
    request.validate().map_err(map_wire_error)?;
    let data = state
        .admin_settings_service()
        .replace(auth.context(), request)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn admin_api_key_status<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSettingsState + Send + Sync,
{
    let exists = state
        .admin_settings_service()
        .admin_api_key_exists()
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminApiKeyStatus { exists }),
    ))
}

async fn regenerate_admin_api_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSettingsState + Send + Sync,
{
    let key = state
        .admin_settings_service()
        .regenerate_admin_api_key(auth.context())
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(RegeneratedAdminApiKey { key }),
    ))
}

async fn delete_admin_api_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSettingsState + Send + Sync,
{
    state
        .admin_settings_service()
        .delete_admin_api_key(auth.context())
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DeletedAdminApiKey::default()),
    ))
}

fn require_positive_i64(value: u64, field: &'static str) -> Result<(), WireValidationError> {
    if value == 0 || i64::try_from(value).is_err() {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn validate_provider_model_mappings(
    mappings: &ProviderModelMappings,
) -> Result<(), WireValidationError> {
    if mappings.len() > 32 {
        return Err(WireValidationError::new("providerModelMappings"));
    }
    for (provider, entries) in mappings {
        if !valid_slug(provider, 64) || entries.len() > 512 {
            return Err(WireValidationError::new("providerModelMappings"));
        }
        for (requested, upstream) in entries {
            if !valid_model_name(requested, 256) || !valid_model_name(upstream, 256) {
                return Err(WireValidationError::new("providerModelMappings"));
            }
        }
    }
    Ok(())
}

fn valid_slug(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_model_name(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && !value.bytes().any(|byte| byte.is_ascii_control())
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
            AdminError::service_unavailable("Settings repository unavailable")
        }
        AdminServiceErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}
