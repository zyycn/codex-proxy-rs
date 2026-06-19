//! 管理端设置处理器。

use std::collections::BTreeMap;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_core::admin::settings::{AdminSettingsPatch, SettingsServiceError};
use codex_proxy_platform::config::{AppConfig, QuotaWarningThresholds};
use codex_proxy_runtime::{services::RuntimeSettingsError, state::AppState};
use serde::Serialize;
use serde_json::Value;

use crate::{
    admin_api::{require_admin_session, AdminEnvelope, AdminError, AdminResponse},
    middleware::request_id::RequestId,
};

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

/// 管理端设置响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSettingsData {
    /// 默认模型。
    pub default_model: String,
    /// 默认推理强度。
    pub default_reasoning_effort: Option<String>,
    /// 默认服务层级。
    pub service_tier: Option<String>,
    /// 模型别名。
    pub model_aliases: BTreeMap<String, String>,
    /// 是否启用刷新。
    pub refresh_enabled: bool,
    /// 刷新提前秒数。
    pub refresh_margin_seconds: u64,
    /// 刷新并发度。
    pub refresh_concurrency: u32,
    /// 单账号最大并发。
    pub max_concurrent_per_account: usize,
    /// 请求间隔毫秒。
    pub request_interval_ms: u64,
    /// 轮换策略。
    pub rotation_strategy: String,
    /// 套餐优先级。
    pub tier_priority: Vec<String>,
    /// 配额刷新周期。
    pub quota_refresh_interval_minutes: u64,
    /// 配额预警阈值。
    pub quota_warning_thresholds: QuotaWarningThresholds,
    /// 是否跳过配额耗尽账号。
    pub quota_skip_exhausted: bool,
    /// 是否启用日志。
    pub logs_enabled: bool,
    /// 日志容量。
    pub logs_capacity: u32,
    /// 是否捕获请求体。
    pub logs_capture_body: bool,
    /// 用量历史保留天数。
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

/// 读取管理端设置。
pub async fn settings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let config = state.services.settings.current();

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSettingsData::from_config(&config), request_id),
    ))
}

/// 更新管理端设置。
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
        .update(patch)
        .await
        .map_err(|error| settings_service_error(error, &request_id))?;

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

fn settings_service_error(error: RuntimeSettingsError, request_id: &str) -> AdminError {
    match error {
        RuntimeSettingsError::InvalidField(SettingsServiceError::InvalidField {
            field,
            message,
        }) => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("Invalid setting {field}: {message}"),
            request_id,
        ),
        RuntimeSettingsError::Persist(_) => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50000,
            "Failed to persist settings",
            request_id,
        ),
    }
}
