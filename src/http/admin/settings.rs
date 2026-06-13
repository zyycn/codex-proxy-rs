use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    app::state::AppState,
    config::{AppConfig, QuotaWarningThresholds},
    http::middleware::RequestId,
    service::settings::{SettingsServiceError, SettingsUpdate},
};

use super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};

const ALLOWED_SETTINGS_KEYS: [&str; 18] = [
    "defaultModel",
    "defaultReasoningEffort",
    "serviceTier",
    "modelAliases",
    "refreshEnabled",
    "refreshMarginSeconds",
    "refreshConcurrency",
    "maxConcurrentPerAccount",
    "requestIntervalMs",
    "rotationStrategy",
    "tierPriority",
    "quotaRefreshIntervalMinutes",
    "quotaWarningThresholds",
    "quotaSkipExhausted",
    "logsEnabled",
    "logsCapacity",
    "logsCaptureBody",
    "usageHistoryRetentionDays",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSettingsData {
    pub default_model: String,
    pub default_reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub model_aliases: std::collections::BTreeMap<String, String>,
    pub refresh_enabled: bool,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: usize,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub tier_priority: Vec<String>,
    pub quota_refresh_interval_minutes: u64,
    pub quota_warning_thresholds: QuotaWarningThresholds,
    pub quota_skip_exhausted: bool,
    pub logs_enabled: bool,
    pub logs_capacity: u32,
    pub logs_capture_body: bool,
    pub usage_history_retention_days: Option<u64>,
}

impl AdminSettingsData {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            default_model: config.model.default_model.clone(),
            default_reasoning_effort: config.model.default_reasoning_effort.clone(),
            service_tier: config.model.service_tier.clone(),
            model_aliases: config.model.aliases.clone(),
            refresh_enabled: config.auth.refresh_enabled,
            refresh_margin_seconds: config.auth.refresh_margin_seconds,
            refresh_concurrency: config.auth.refresh_concurrency,
            max_concurrent_per_account: config.auth.max_concurrent_per_account,
            request_interval_ms: config.auth.request_interval_ms,
            rotation_strategy: config.auth.rotation_strategy.clone(),
            tier_priority: config.auth.tier_priority.clone(),
            quota_refresh_interval_minutes: config.quota.refresh_interval_minutes,
            quota_warning_thresholds: config.quota.warning_thresholds.clone(),
            quota_skip_exhausted: config.quota.skip_exhausted,
            logs_enabled: config.logging.enabled,
            logs_capacity: config.logging.capacity,
            logs_capture_body: config.logging.capture_body,
            usage_history_retention_days: config.usage_stats.history_retention_days,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminSettingsPatch {
    default_model: Option<String>,
    default_reasoning_effort: Option<String>,
    service_tier: Option<String>,
    model_aliases: Option<std::collections::BTreeMap<String, String>>,
    refresh_enabled: Option<bool>,
    refresh_margin_seconds: Option<u64>,
    refresh_concurrency: Option<u32>,
    max_concurrent_per_account: Option<usize>,
    request_interval_ms: Option<u64>,
    rotation_strategy: Option<String>,
    tier_priority: Option<Vec<String>>,
    quota_refresh_interval_minutes: Option<u64>,
    quota_warning_thresholds: Option<QuotaWarningThresholds>,
    quota_skip_exhausted: Option<bool>,
    logs_enabled: Option<bool>,
    logs_capacity: Option<u32>,
    logs_capture_body: Option<bool>,
    usage_history_retention_days: Option<u64>,
}

impl From<AdminSettingsPatch> for SettingsUpdate {
    fn from(patch: AdminSettingsPatch) -> Self {
        Self {
            default_model: patch.default_model,
            default_reasoning_effort: patch.default_reasoning_effort,
            service_tier: patch.service_tier,
            model_aliases: patch.model_aliases,
            refresh_enabled: patch.refresh_enabled,
            refresh_margin_seconds: patch.refresh_margin_seconds,
            refresh_concurrency: patch.refresh_concurrency,
            max_concurrent_per_account: patch.max_concurrent_per_account,
            request_interval_ms: patch.request_interval_ms,
            rotation_strategy: patch.rotation_strategy,
            tier_priority: patch.tier_priority,
            quota_refresh_interval_minutes: patch.quota_refresh_interval_minutes,
            quota_warning_thresholds: patch.quota_warning_thresholds,
            quota_skip_exhausted: patch.quota_skip_exhausted,
            logs_enabled: patch.logs_enabled,
            logs_capacity: patch.logs_capacity,
            logs_capture_body: patch.logs_capture_body,
            usage_history_retention_days: patch.usage_history_retention_days,
        }
    }
}

pub async fn settings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let config = state.services.settings.current().await;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSettingsData::from_config(&config), request_id),
    ))
}

pub async fn update_settings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let patch = parse_settings_patch(&body, &request_id)?;
    let config = state
        .services
        .settings
        .update(patch.into())
        .await
        .map_err(|err| settings_service_error(err, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSettingsData::from_config(&config), request_id),
    ))
}

fn parse_settings_patch(body: &[u8], request_id: &str) -> Result<AdminSettingsPatch, AdminError> {
    let value = serde_json::from_slice::<Value>(body).map_err(|_| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40000,
            "Malformed JSON body",
            request_id,
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Settings payload must be an object",
            request_id,
        )
    })?;

    if let Some(key) = object
        .keys()
        .find(|key| !ALLOWED_SETTINGS_KEYS.contains(&key.as_str()))
    {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("Unsupported settings field: {key}"),
            request_id,
        ));
    }

    serde_json::from_value(value).map_err(|_| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Invalid settings payload",
            request_id,
        )
    })
}

fn settings_service_error(error: SettingsServiceError, request_id: &str) -> AdminError {
    match error {
        SettingsServiceError::InvalidField { field, message } => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("Invalid setting {field}: {message}"),
            request_id,
        ),
        SettingsServiceError::Serialize(_)
        | SettingsServiceError::CreateDirectory(_)
        | SettingsServiceError::Write(_) => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50000,
            "Failed to persist settings",
            request_id,
        ),
    }
}
