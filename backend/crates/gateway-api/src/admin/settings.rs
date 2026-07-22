//! Runtime settings、旧设置页聚合投影与明文 Admin API Key wire。

use std::{collections::BTreeMap, fmt};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use gateway_admin::model::{
    Revision,
    settings::{
        ProviderModelMappings as DomainProviderModelMappings, ReplaceRuntimeSettings,
        RotationStrategy, RuntimeSettings,
    },
};
use gateway_core::routing::{ProviderKind, PublicModelId, UpstreamModelId};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminSessionState, WireValidationError,
    wire::map_admin_service_error,
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

    fn into_command(self) -> Result<ReplaceRuntimeSettings, WireValidationError> {
        self.validate()?;
        Ok(ReplaceRuntimeSettings {
            expected_config_revision: Revision::new(self.expected_config_revision)
                .map_err(|_| WireValidationError::new("settingsRevisionOverflow"))?,
            provider_model_mappings: domain_provider_model_mappings(self.provider_model_mappings)?,
            refresh_margin_seconds: self.refresh_margin_seconds,
            refresh_concurrency: u32::try_from(self.refresh_concurrency)
                .map_err(|_| WireValidationError::new("settingsRefreshConcurrencyOverflow"))?,
            max_concurrent_per_account: u32::try_from(self.max_concurrent_per_account)
                .map_err(|_| WireValidationError::new("settingsMaxConcurrencyOverflow"))?,
            request_interval_ms: self.request_interval_ms,
            rotation_strategy: parse_rotation_strategy(&self.rotation_strategy)
                .ok_or_else(|| WireValidationError::new("rotationStrategy"))?,
            usage_retention_days: u32::try_from(self.usage_retention_days)
                .map_err(|_| WireValidationError::new("settingsUsageRetentionOverflow"))?,
            ops_event_retention_days: u32::try_from(self.ops_event_retention_days)
                .map_err(|_| WireValidationError::new("settingsOpsRetentionOverflow"))?,
            audit_retention_days: u32::try_from(self.audit_retention_days)
                .map_err(|_| WireValidationError::new("settingsAuditRetentionOverflow"))?,
        })
    }
}

impl From<RuntimeSettings> for RuntimeSettingsView {
    fn from(settings: RuntimeSettings) -> Self {
        Self {
            config_revision: settings.config_revision.get(),
            provider_model_mappings: wire_provider_model_mappings(settings.provider_model_mappings),
            refresh_margin_seconds: settings.refresh_margin_seconds,
            refresh_concurrency: u64::from(settings.refresh_concurrency),
            max_concurrent_per_account: u64::from(settings.max_concurrent_per_account),
            request_interval_ms: settings.request_interval_ms,
            rotation_strategy: rotation_strategy_name(settings.rotation_strategy).to_owned(),
            usage_retention_days: u64::from(settings.usage_retention_days),
            ops_event_retention_days: u64::from(settings.ops_event_retention_days),
            audit_retention_days: u64::from(settings.audit_retention_days),
            updated_at: settings.updated_at,
        }
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

/// 构造固定 GET/POST 设置路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/settings", get(settings::<S>))
        .route("/api/admin/settings/update", post(update_settings::<S>))
        .route(
            "/api/admin/settings/admin-api-key",
            get(admin_api_key_status::<S>),
        )
        .route(
            "/api/admin/settings/admin-api-key/delete",
            post(delete_admin_api_key::<S>),
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
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .settings()
        .load()
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(RuntimeSettingsView::from(result)),
    ))
}

async fn update_settings<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<UpdateRuntimeSettingsRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .settings()
        .replace(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(RuntimeSettingsView::from(result)),
    ))
}

async fn admin_api_key_status<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let exists = state
        .admin_services()
        .settings()
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
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .settings()
        .regenerate_admin_api_key(&auth.context().mutation_context())
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(RegeneratedAdminApiKey {
            key: result.key.expose_for_response().to_owned(),
        }),
    ))
}

async fn delete_admin_api_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    state
        .admin_services()
        .settings()
        .delete_admin_api_key(&auth.context().mutation_context())
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

fn domain_provider_model_mappings(
    mappings: ProviderModelMappings,
) -> Result<DomainProviderModelMappings, WireValidationError> {
    mappings
        .into_iter()
        .map(|(provider, entries)| {
            let provider = ProviderKind::new(provider)
                .map_err(|_| WireValidationError::new("providerModelMappings"))?;
            let entries = entries
                .into_iter()
                .map(|(requested, upstream)| {
                    Ok((
                        PublicModelId::new(requested)
                            .map_err(|_| WireValidationError::new("providerModelMappings"))?,
                        UpstreamModelId::new(upstream)
                            .map_err(|_| WireValidationError::new("providerModelMappings"))?,
                    ))
                })
                .collect::<Result<_, WireValidationError>>()?;
            Ok((provider, entries))
        })
        .collect()
}

fn wire_provider_model_mappings(mappings: DomainProviderModelMappings) -> ProviderModelMappings {
    mappings
        .into_iter()
        .map(|(provider, entries)| {
            (
                provider.to_string(),
                entries
                    .into_iter()
                    .map(|(requested, upstream)| (requested.to_string(), upstream.to_string()))
                    .collect(),
            )
        })
        .collect()
}

fn parse_rotation_strategy(value: &str) -> Option<RotationStrategy> {
    match value {
        "smart" => Some(RotationStrategy::Smart),
        "quota_reset_priority" => Some(RotationStrategy::QuotaResetPriority),
        "round_robin" => Some(RotationStrategy::RoundRobin),
        "sticky" => Some(RotationStrategy::Sticky),
        _ => None,
    }
}

const fn rotation_strategy_name(strategy: RotationStrategy) -> &'static str {
    match strategy {
        RotationStrategy::Smart => "smart",
        RotationStrategy::QuotaResetPriority => "quota_reset_priority",
        RotationStrategy::RoundRobin => "round_robin",
        RotationStrategy::Sticky => "sticky",
    }
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
    let message = match error.field() {
        "settingsRevisionOverflow" => "expectedConfigRevision must be positive".to_owned(),
        "settingsRefreshConcurrencyOverflow" => "Invalid refreshConcurrency".to_owned(),
        "settingsMaxConcurrencyOverflow" => "Invalid maxConcurrentPerAccount".to_owned(),
        "settingsUsageRetentionOverflow" => "Invalid usageRetentionDays".to_owned(),
        "settingsOpsRetentionOverflow" => "Invalid opsEventRetentionDays".to_owned(),
        "settingsAuditRetentionOverflow" => "Invalid auditRetentionDays".to_owned(),
        field => format!("Invalid field: {field}"),
    };
    AdminError::bad_request(message)
}

fn map_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    map_admin_service_error(error, "Settings repository unavailable")
}
