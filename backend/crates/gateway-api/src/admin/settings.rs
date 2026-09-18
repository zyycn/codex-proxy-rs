//! Runtime settings、旧设置页聚合投影与明文 Admin API Key wire。

use crate::auth::SessionState;

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
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse,
    WireValidationError, wire::map_admin_service_error,
};

/// 客户端模型到上游模型的全局精确映射。
pub type ModelMappings = BTreeMap<String, String>;

/// 运行配置投影与设置页字段的聚合响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSettingsView {
    pub openai_client_profile: Option<serde_json::Map<String, serde_json::Value>>,
    pub disable_fast: bool,
    pub request_location_enabled: bool,
    pub request_location: gateway_core::account::RequestLocation,
    pub model_mappings: ModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u64,
    pub max_concurrent_per_account: u64,
    pub request_interval_ms: u64,
    pub max_waiting_per_key: u32,
    pub max_waiting_per_account: u32,
    pub concurrency_wait_timeout_seconds: u32,
    pub responses_max_decompressed_body_bytes: u64,
    pub rotation_strategy: String,
    pub min_codex_desktop_version: Option<String>,
    pub min_codex_cli_version: Option<String>,
    pub usage_retention_days: u64,
    pub ops_event_retention_days: u64,
    pub audit_retention_days: u64,
    pub account_auto_freeze_enabled: bool,
    pub account_auto_freeze_threshold: u64,
    pub account_auto_freeze_window_seconds: u64,
    pub account_auto_freeze_duration_seconds: u64,
    pub account_auto_freeze_probe_enabled: bool,
    pub account_auto_freeze_probe_model: Option<String>,
    pub account_auto_freeze_adaptive_concurrency: bool,
    pub updated_at: DateTime<Utc>,
}

/// 原子替换全局运行参数的请求。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRuntimeSettingsRequest {
    #[serde(default, deserialize_with = "deserialize_profile_update")]
    pub openai_client_profile: Option<serde_json::Map<String, serde_json::Value>>,
    pub disable_fast: Option<bool>,
    pub request_location_enabled: bool,
    pub request_location: gateway_core::account::RequestLocation,
    pub model_mappings: ModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u64,
    pub max_concurrent_per_account: u64,
    pub request_interval_ms: u64,
    pub max_waiting_per_key: u32,
    pub max_waiting_per_account: u32,
    pub concurrency_wait_timeout_seconds: u32,
    pub responses_max_decompressed_body_bytes: u64,
    pub rotation_strategy: String,
    pub min_codex_desktop_version: Option<String>,
    pub min_codex_cli_version: Option<String>,
    pub usage_retention_days: u64,
    pub ops_event_retention_days: u64,
    pub audit_retention_days: u64,
    pub account_auto_freeze_enabled: bool,
    pub account_auto_freeze_threshold: u64,
    pub account_auto_freeze_window_seconds: u64,
    pub account_auto_freeze_duration_seconds: u64,
    pub account_auto_freeze_probe_enabled: bool,
    pub account_auto_freeze_probe_model: Option<String>,
    pub account_auto_freeze_adaptive_concurrency: bool,
}

impl UpdateRuntimeSettingsRequest {
    /// 校验公共运行参数。
    pub fn validate(&self) -> Result<(), WireValidationError> {
        self.request_location
            .validate()
            .map_err(|_| WireValidationError::new("requestLocation"))?;
        validate_model_mappings(&self.model_mappings)?;
        for (value, field) in [
            (self.max_waiting_per_key, "maxWaitingPerKey"),
            (self.max_waiting_per_account, "maxWaitingPerAccount"),
        ] {
            if value > 1_000 {
                return Err(WireValidationError::new(field));
            }
        }
        if self.responses_max_decompressed_body_bytes == 0
            || isize::try_from(self.responses_max_decompressed_body_bytes).is_err()
        {
            return Err(WireValidationError::new(
                "responsesMaxDecompressedBodyBytes",
            ));
        }
        if !(1..=120).contains(&self.concurrency_wait_timeout_seconds) {
            return Err(WireValidationError::new("concurrencyWaitTimeoutSeconds"));
        }
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
        for (value, field) in [
            (
                self.account_auto_freeze_threshold,
                "accountAutoFreezeThreshold",
            ),
            (
                self.account_auto_freeze_window_seconds,
                "accountAutoFreezeWindowSeconds",
            ),
            (
                self.account_auto_freeze_duration_seconds,
                "accountAutoFreezeDurationSeconds",
            ),
        ] {
            require_positive_i64(value, field)?;
        }
        if !(2..=1_000).contains(&self.account_auto_freeze_threshold) {
            return Err(WireValidationError::new("accountAutoFreezeThreshold"));
        }
        if !(60..=3_600).contains(&self.account_auto_freeze_window_seconds) {
            return Err(WireValidationError::new("accountAutoFreezeWindowSeconds"));
        }
        if !(300..=604_800).contains(&self.account_auto_freeze_duration_seconds) {
            return Err(WireValidationError::new("accountAutoFreezeDurationSeconds"));
        }
        validate_optional_probe_model(
            self.account_auto_freeze_probe_model.as_deref(),
            "accountAutoFreezeProbeModel",
        )?;
        Ok(())
    }

    fn into_command(self) -> Result<ReplaceRuntimeSettings, WireValidationError> {
        self.validate()?;
        Ok(ReplaceRuntimeSettings {
            openai_client_profile: self
                .openai_client_profile
                .map(gateway_core::account::OpaqueProviderData::new),
            disable_fast: self.disable_fast,
            request_location_enabled: self.request_location_enabled,
            request_location: self
                .request_location
                .normalized()
                .map_err(|_| WireValidationError::new("requestLocation"))?,
            model_mappings: domain_model_mappings(self.model_mappings)?,
            refresh_margin_seconds: self.refresh_margin_seconds,
            refresh_concurrency: u32::try_from(self.refresh_concurrency)
                .map_err(|_| WireValidationError::new("settingsRefreshConcurrencyOverflow"))?,
            max_concurrent_per_account: u32::try_from(self.max_concurrent_per_account)
                .map_err(|_| WireValidationError::new("settingsMaxConcurrencyOverflow"))?,
            request_interval_ms: self.request_interval_ms,
            max_waiting_per_key: self.max_waiting_per_key,
            max_waiting_per_account: self.max_waiting_per_account,
            concurrency_wait_timeout_seconds: self.concurrency_wait_timeout_seconds,
            responses_max_decompressed_body_bytes: self.responses_max_decompressed_body_bytes,
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
            account_auto_freeze_enabled: self.account_auto_freeze_enabled,
            account_auto_freeze_threshold: u32::try_from(self.account_auto_freeze_threshold)
                .map_err(|_| WireValidationError::new("settingsFreezeThresholdOverflow"))?,
            account_auto_freeze_window_seconds: self.account_auto_freeze_window_seconds,
            account_auto_freeze_duration_seconds: self.account_auto_freeze_duration_seconds,
            account_auto_freeze_probe_enabled: self.account_auto_freeze_probe_enabled,
            account_auto_freeze_probe_model: self.account_auto_freeze_probe_model,
            account_auto_freeze_adaptive_concurrency: self.account_auto_freeze_adaptive_concurrency,
        })
    }
}

impl From<RuntimeSettings> for RuntimeSettingsView {
    fn from(settings: RuntimeSettings) -> Self {
        Self {
            openai_client_profile: settings
                .openai_client_profile
                .map(gateway_core::account::OpaqueProviderData::into_inner),
            disable_fast: settings.disable_fast,
            request_location_enabled: settings.request_location_enabled,
            request_location: settings.request_location,
            model_mappings: wire_model_mappings(settings.model_mappings),
            refresh_margin_seconds: settings.refresh_margin_seconds,
            refresh_concurrency: u64::from(settings.refresh_concurrency),
            max_concurrent_per_account: u64::from(settings.max_concurrent_per_account),
            request_interval_ms: settings.request_interval_ms,
            max_waiting_per_key: settings.max_waiting_per_key,
            max_waiting_per_account: settings.max_waiting_per_account,
            concurrency_wait_timeout_seconds: settings.concurrency_wait_timeout_seconds,
            responses_max_decompressed_body_bytes: settings.responses_max_decompressed_body_bytes,
            rotation_strategy: settings.rotation_strategy.as_str().to_owned(),
            min_codex_desktop_version: settings.min_codex_desktop_version,
            min_codex_cli_version: settings.min_codex_cli_version,
            usage_retention_days: u64::from(settings.usage_retention_days),
            ops_event_retention_days: u64::from(settings.ops_event_retention_days),
            audit_retention_days: u64::from(settings.audit_retention_days),
            account_auto_freeze_enabled: settings.account_auto_freeze_enabled,
            account_auto_freeze_threshold: u64::from(settings.account_auto_freeze_threshold),
            account_auto_freeze_window_seconds: settings.account_auto_freeze_window_seconds,
            account_auto_freeze_duration_seconds: settings.account_auto_freeze_duration_seconds,
            account_auto_freeze_probe_enabled: settings.account_auto_freeze_probe_enabled,
            account_auto_freeze_probe_model: settings.account_auto_freeze_probe_model,
            account_auto_freeze_adaptive_concurrency: settings
                .account_auto_freeze_adaptive_concurrency,
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
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/settings", get(settings::<S>))
        .route(
            "/api/admin/settings/client-profiles/openai",
            get(client_profile_options::<S>),
        )
        .route(
            "/api/admin/settings/client-profiles/openai/preview",
            post(preview_client_profile::<S>),
        )
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
    S: SessionState + Send + Sync,
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
    S: SessionState + Send + Sync,
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
    S: SessionState + Send + Sync,
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
    S: SessionState + Send + Sync,
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
    S: SessionState + Send + Sync,
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
    S: SessionState + Send + Sync,
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

/// 探测模型为可选自由文本：非空、去首尾空白后不变、无控制字符且不超过 128 字节。
fn validate_optional_probe_model(
    value: Option<&str>,
    field: &'static str,
) -> Result<(), WireValidationError> {
    if value.is_some_and(|value| {
        value.is_empty()
            || value.len() > 128
            || value != value.trim()
            || value.bytes().any(|byte| byte.is_ascii_control())
    }) {
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
        "requestLocation" => "请求位置不合法，请检查国家代码、地区和城市".to_owned(),
        "settingsFreezeThresholdOverflow" => "accountAutoFreezeThreshold 不合法".to_owned(),
        "accountAutoFreezeThreshold" => "账号自动冻结阈值应为 2～1000 的整数".to_owned(),
        "accountAutoFreezeWindowSeconds" => "账号自动冻结统计窗口应为 60～3600 秒".to_owned(),
        "accountAutoFreezeDurationSeconds" => "账号自动冻结时长应为 300～604800 秒".to_owned(),
        "accountAutoFreezeProbeModel" => "探测模型格式不合法".to_owned(),
        "minCodexDesktopVersion" => "Codex Desktop 最低版本格式不合法".to_owned(),
        "minCodexCliVersion" => "Codex CLI 最低版本格式不合法".to_owned(),
        field => format!("{field} 字段不合法"),
    };
    AdminError::bad_request(message)
}

fn map_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    map_admin_service_error(error)
}

// 字段省略时保留已有配置；显式 null 不能清空唯一的通用默认。
fn deserialize_profile_update<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, D::Error> {
    serde_json::Map::<String, serde_json::Value>::deserialize(deserializer).map(Some)
}

async fn client_profile_options<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .settings()
        .client_profile_options()
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(result.into_inner()),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientProfilePreviewRequest {
    configuration: Option<serde_json::Map<String, serde_json::Value>>,
}

async fn preview_client_profile<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ClientProfilePreviewRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let configuration = request
        .configuration
        .map(gateway_core::account::OpaqueProviderData::new);
    let result = state
        .admin_services()
        .settings()
        .preview_client_profile(configuration.as_ref())
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(result.into_inner()),
    ))
}
