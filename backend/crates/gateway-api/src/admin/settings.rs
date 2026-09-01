//! Runtime settings、旧设置页聚合投影与明文 Admin API Key wire。

use std::{collections::BTreeMap, fmt};

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use gateway_admin::model::client_distribution::{
    ClientDownloadPackage, CodexDesktopWindowsDownloads,
};
use gateway_admin::model::settings::{
    ModelMappings as DomainModelMappings, ReplaceRuntimeSettings, RotationStrategy, RuntimeSettings,
};
use gateway_core::policy::CodexClientVersion;
use gateway_core::routing::{PublicModelId, UpstreamModelId};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse, AdminSessionState,
    WireValidationError, wire::map_admin_service_error,
};

/// 客户端模型到上游模型的全局精确映射。
pub type ModelMappings = BTreeMap<String, String>;

/// 运行配置投影与设置页字段的聚合响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSettingsView {
    pub model_mappings: ModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u64,
    pub max_concurrent_per_account: u64,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub min_codex_desktop_version: Option<String>,
    pub min_codex_cli_version: Option<String>,
    pub usage_retention_days: u64,
    pub ops_event_retention_days: u64,
    pub audit_retention_days: u64,
    pub updated_at: DateTime<Utc>,
}

/// 原子替换全局运行参数的请求。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRuntimeSettingsRequest {
    pub model_mappings: ModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u64,
    pub max_concurrent_per_account: u64,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub min_codex_desktop_version: Option<String>,
    pub min_codex_cli_version: Option<String>,
    pub usage_retention_days: u64,
    pub ops_event_retention_days: u64,
    pub audit_retention_days: u64,
}

impl UpdateRuntimeSettingsRequest {
    /// 校验公共运行参数。
    pub fn validate(&self) -> Result<(), WireValidationError> {
        validate_model_mappings(&self.model_mappings)?;
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
        if RotationStrategy::parse(&self.rotation_strategy).is_none() {
            return Err(WireValidationError::new("rotationStrategy"));
        }
        validate_optional_client_version(
            self.min_codex_desktop_version.as_deref(),
            "minCodexDesktopVersion",
        )?;
        validate_optional_client_version(
            self.min_codex_cli_version.as_deref(),
            "minCodexCliVersion",
        )?;
        Ok(())
    }

    fn into_command(self) -> Result<ReplaceRuntimeSettings, WireValidationError> {
        self.validate()?;
        Ok(ReplaceRuntimeSettings {
            model_mappings: domain_model_mappings(self.model_mappings)?,
            refresh_margin_seconds: self.refresh_margin_seconds,
            refresh_concurrency: u32::try_from(self.refresh_concurrency)
                .map_err(|_| WireValidationError::new("settingsRefreshConcurrencyOverflow"))?,
            max_concurrent_per_account: u32::try_from(self.max_concurrent_per_account)
                .map_err(|_| WireValidationError::new("settingsMaxConcurrencyOverflow"))?,
            request_interval_ms: self.request_interval_ms,
            rotation_strategy: RotationStrategy::parse(&self.rotation_strategy)
                .ok_or_else(|| WireValidationError::new("rotationStrategy"))?,
            min_codex_desktop_version: self.min_codex_desktop_version,
            min_codex_cli_version: self.min_codex_cli_version,
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
            model_mappings: wire_model_mappings(settings.model_mappings),
            refresh_margin_seconds: settings.refresh_margin_seconds,
            refresh_concurrency: u64::from(settings.refresh_concurrency),
            max_concurrent_per_account: u64::from(settings.max_concurrent_per_account),
            request_interval_ms: settings.request_interval_ms,
            rotation_strategy: settings.rotation_strategy.as_str().to_owned(),
            min_codex_desktop_version: settings.min_codex_desktop_version,
            min_codex_cli_version: settings.min_codex_cli_version,
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

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ClientDownloadsQuery {
    refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientDownloadPackageView {
    architecture: String,
    source: String,
    version: Option<String>,
    file_name: String,
    size_bytes: Option<u64>,
    download_url: String,
    expires_at: Option<DateTime<Utc>>,
}

impl From<ClientDownloadPackage> for ClientDownloadPackageView {
    fn from(package: ClientDownloadPackage) -> Self {
        Self {
            architecture: package.architecture.as_str().to_owned(),
            source: package.source.as_str().to_owned(),
            version: package.version,
            file_name: package.file_name,
            size_bytes: package.size_bytes,
            download_url: package.download_url,
            expires_at: package.expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexDesktopWindowsDownloadsView {
    resolved_at: DateTime<Utc>,
    cached: bool,
    warning: Option<String>,
    packages: Vec<ClientDownloadPackageView>,
}

impl From<CodexDesktopWindowsDownloads> for CodexDesktopWindowsDownloadsView {
    fn from(downloads: CodexDesktopWindowsDownloads) -> Self {
        Self {
            resolved_at: downloads.resolved_at,
            cached: downloads.cached,
            warning: downloads.warning,
            packages: downloads.packages.into_iter().map(Into::into).collect(),
        }
    }
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
            "/api/admin/settings/client-downloads/codex-desktop/windows",
            get(codex_desktop_windows_downloads::<S>),
        )
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

async fn codex_desktop_windows_downloads<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<ClientDownloadsQuery>,
) -> impl IntoResponse
where
    S: AdminSessionState + Send + Sync,
{
    let downloads = state
        .admin_services()
        .client_distribution()
        .codex_desktop_windows(query.refresh)
        .await;
    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(CodexDesktopWindowsDownloadsView::from(downloads)),
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
    AdminJson(request): AdminJson<UpdateRuntimeSettingsRequest>,
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

fn validate_model_mappings(mappings: &ModelMappings) -> Result<(), WireValidationError> {
    if mappings.len() > 512 {
        return Err(WireValidationError::new("modelMappings"));
    }
    for (requested, upstream) in mappings {
        if !valid_model_name(requested, 256) || !valid_model_name(upstream, 256) {
            return Err(WireValidationError::new("modelMappings"));
        }
    }
    Ok(())
}

fn domain_model_mappings(
    mappings: ModelMappings,
) -> Result<DomainModelMappings, WireValidationError> {
    mappings
        .into_iter()
        .map(|(requested, upstream)| {
            Ok((
                PublicModelId::new(requested)
                    .map_err(|_| WireValidationError::new("modelMappings"))?,
                UpstreamModelId::new(upstream)
                    .map_err(|_| WireValidationError::new("modelMappings"))?,
            ))
        })
        .collect()
}

fn wire_model_mappings(mappings: DomainModelMappings) -> ModelMappings {
    mappings
        .into_iter()
        .map(|(requested, upstream)| (requested.to_string(), upstream.to_string()))
        .collect()
}

fn valid_model_name(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn validate_optional_client_version(
    value: Option<&str>,
    field: &'static str,
) -> Result<(), WireValidationError> {
    if value.is_some_and(|value| CodexClientVersion::parse(value).is_err()) {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    let message = match error.field() {
        "settingsRefreshConcurrencyOverflow" => "refreshConcurrency 不合法".to_owned(),
        "settingsMaxConcurrencyOverflow" => "maxConcurrentPerAccount 不合法".to_owned(),
        "settingsUsageRetentionOverflow" => "usageRetentionDays 不合法".to_owned(),
        "settingsOpsRetentionOverflow" => "opsEventRetentionDays 不合法".to_owned(),
        "settingsAuditRetentionOverflow" => "auditRetentionDays 不合法".to_owned(),
        "minCodexDesktopVersion" => "Codex Desktop 最低版本格式不合法".to_owned(),
        "minCodexCliVersion" => "Codex CLI 最低版本格式不合法".to_owned(),
        field => format!("{field} 字段不合法"),
    };
    AdminError::bad_request(message)
}

fn map_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    map_admin_service_error(error)
}
