//! 配置设置领域逻辑与运行时写回服务。

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, RwLock as StdRwLock},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{
    types::{AppConfig, QuotaWarningThresholds},
    ConfigWriteError,
};

const ROTATION_STRATEGIES: [&str; 3] = ["least_used", "round_robin", "sticky"];

/// 管理端可变设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSettings {
    /// 默认模型 ID。
    pub default_model: String,
    /// 默认 reasoning effort。
    pub default_reasoning_effort: Option<String>,
    /// 默认服务层级。
    pub service_tier: Option<String>,
    /// 模型别名映射。
    pub model_aliases: BTreeMap<String, String>,
    /// 是否启用访问令牌刷新。
    pub refresh_enabled: bool,
    /// 访问令牌过期前多少秒开始刷新。
    pub refresh_margin_seconds: u64,
    /// 访问令牌刷新并发数。
    pub refresh_concurrency: u32,
    /// 单账号最大并发请求数。
    pub max_concurrent_per_account: usize,
    /// 同账号请求间隔毫秒数。
    pub request_interval_ms: u64,
    /// 账号轮换策略。
    pub rotation_strategy: String,
    /// 计划类型优先级。
    pub tier_priority: Vec<String>,
    /// 配额刷新间隔分钟数。
    pub quota_refresh_interval_minutes: u64,
    /// 配额预警阈值。
    pub quota_warning_thresholds: AdminQuotaWarningThresholds,
    /// 配额耗尽账号是否跳过调度。
    pub quota_skip_exhausted: bool,
    /// 是否启用事件日志。
    pub logs_enabled: bool,
    /// 事件日志容量。
    pub logs_capacity: u32,
    /// 事件日志是否捕获请求/响应体。
    pub logs_capture_body: bool,
    /// 用量历史保留天数。
    pub usage_history_retention_days: Option<u64>,
}

impl Default for AdminSettings {
    fn default() -> Self {
        Self {
            default_model: "gpt-4o".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            model_aliases: BTreeMap::new(),
            refresh_enabled: true,
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "least_used".to_string(),
            tier_priority: Vec::new(),
            quota_refresh_interval_minutes: 5,
            quota_warning_thresholds: AdminQuotaWarningThresholds::default(),
            quota_skip_exhausted: true,
            logs_enabled: false,
            logs_capacity: 2_000,
            logs_capture_body: false,
            usage_history_retention_days: None,
        }
    }
}

/// 配额预警阈值集合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminQuotaWarningThresholds {
    /// primary 配额预警百分比。
    pub primary: Vec<u8>,
    /// secondary 配额预警百分比。
    pub secondary: Vec<u8>,
}

impl Default for AdminQuotaWarningThresholds {
    fn default() -> Self {
        Self {
            primary: vec![80, 90],
            secondary: vec![80, 90],
        }
    }
}

/// 管理端设置补丁。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSettingsPatch {
    /// 默认模型 ID。
    pub default_model: Option<String>,
    /// 单账号最大并发请求数。
    pub max_concurrent_per_account: Option<usize>,
    /// 同账号请求间隔毫秒数。
    pub request_interval_ms: Option<u64>,
    /// 账号轮换策略。
    pub rotation_strategy: Option<String>,
}

/// 设置领域服务。
#[derive(Debug, Clone, Default)]
pub struct SettingsService;

impl SettingsService {
    /// 将管理端设置补丁应用到当前设置。
    pub fn apply_patch(
        current: &mut AdminSettings,
        patch: AdminSettingsPatch,
    ) -> Result<(), SettingsServiceError> {
        if let Some(default_model) = patch.default_model {
            current.default_model = non_empty_string("defaultModel", default_model)?;
        }
        if let Some(max_concurrent_per_account) = patch.max_concurrent_per_account {
            current.max_concurrent_per_account =
                positive_usize("maxConcurrentPerAccount", max_concurrent_per_account)?;
        }
        if let Some(request_interval_ms) = patch.request_interval_ms {
            current.request_interval_ms = request_interval_ms;
        }
        if let Some(rotation_strategy) = patch.rotation_strategy {
            current.rotation_strategy = validate_rotation_strategy(rotation_strategy)?;
        }
        Ok(())
    }
}

/// 设置领域错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SettingsServiceError {
    /// 字段值无效。
    #[error("invalid setting `{field}`: {message}")]
    InvalidField {
        /// 字段名。
        field: String,
        /// 错误说明。
        message: String,
    },
}

impl SettingsServiceError {
    /// 返回无效字段名。
    pub fn field(&self) -> &str {
        match self {
            Self::InvalidField { field, .. } => field,
        }
    }
    /// 返回字段错误说明。
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidField { message, .. } => message,
        }
    }
}

fn validate_rotation_strategy(strategy: String) -> Result<String, SettingsServiceError> {
    let strategy = non_empty_string("rotationStrategy", strategy)?;
    if ROTATION_STRATEGIES.contains(&strategy.as_str()) {
        Ok(strategy)
    } else {
        Err(invalid_field(
            "rotationStrategy",
            "must be one of least_used, round_robin, sticky",
        ))
    }
}

fn non_empty_string(field: &str, value: String) -> Result<String, SettingsServiceError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        Err(invalid_field(field, "must not be empty"))
    } else {
        Ok(value)
    }
}

fn positive_usize(field: &str, value: usize) -> Result<usize, SettingsServiceError> {
    if value == 0 {
        Err(invalid_field(field, "must be greater than 0"))
    } else {
        Ok(value)
    }
}

fn invalid_field(field: &str, message: impl Into<String>) -> SettingsServiceError {
    SettingsServiceError::InvalidField {
        field: field.to_string(),
        message: message.into(),
    }
}

/// 运行时设置服务。
#[derive(Clone)]
pub struct RuntimeSettingsService {
    current: Arc<StdRwLock<Arc<AppConfig>>>,
    config_path: Arc<PathBuf>,
}

impl RuntimeSettingsService {
    /// 使用默认 `config.yaml` 路径构造运行时设置服务。
    pub fn new(config: AppConfig) -> Self {
        Self::with_config_path(config, "config.yaml")
    }

    /// 使用指定配置路径构造运行时设置服务。
    pub fn with_config_path(config: AppConfig, config_path: impl Into<PathBuf>) -> Self {
        Self {
            current: Arc::new(StdRwLock::new(Arc::new(config))),
            config_path: Arc::new(config_path.into()),
        }
    }

    /// 返回当前运行时配置快照。
    pub fn current(&self) -> Arc<AppConfig> {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 返回配置写回路径。
    pub fn config_path(&self) -> Arc<PathBuf> {
        self.config_path.clone()
    }

    /// 应用设置补丁、写回配置文件并更新运行时配置快照。
    pub async fn update(
        &self,
        patch: AdminSettingsPatch,
    ) -> Result<Arc<AppConfig>, RuntimeSettingsError> {
        let mut next = (*self.current()).clone();
        let mut settings = admin_settings_from_config(&next);
        SettingsService::apply_patch(&mut settings, patch)?;
        apply_admin_settings_to_config(&mut next, settings);
        next.write_settings_config(self.config_path.as_ref())
            .await?;
        let next = Arc::new(next);
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next.clone();
        Ok(next)
    }
}

/// 运行时设置错误。
#[derive(Debug, Error)]
pub enum RuntimeSettingsError {
    /// 设置字段校验失败。
    #[error(transparent)]
    InvalidField(#[from] SettingsServiceError),
    /// 配置持久化失败。
    #[error(transparent)]
    Persist(#[from] ConfigWriteError),
}

fn admin_settings_from_config(config: &AppConfig) -> AdminSettings {
    let defaults = AdminSettings::default();
    AdminSettings {
        default_model: config.model.default_model.clone(),
        default_reasoning_effort: config.model.default_reasoning_effort.clone(),
        service_tier: config.model.service_tier.clone(),
        model_aliases: config.model.aliases.clone(),
        refresh_enabled: defaults.refresh_enabled,
        refresh_margin_seconds: config.auth.refresh_margin_seconds,
        refresh_concurrency: config.auth.refresh_concurrency,
        max_concurrent_per_account: config.auth.max_concurrent_per_account,
        request_interval_ms: config.auth.request_interval_ms,
        rotation_strategy: config.auth.rotation_strategy.clone(),
        tier_priority: config.auth.tier_priority.clone(),
        quota_refresh_interval_minutes: config.quota.refresh_interval_minutes,
        quota_warning_thresholds: AdminQuotaWarningThresholds {
            primary: config.quota.warning_thresholds.primary.clone(),
            secondary: config.quota.warning_thresholds.secondary.clone(),
        },
        quota_skip_exhausted: defaults.quota_skip_exhausted,
        logs_enabled: defaults.logs_enabled,
        logs_capacity: defaults.logs_capacity,
        logs_capture_body: config.logging.capture_body,
        usage_history_retention_days: config.usage_stats.history_retention_days,
    }
}

fn apply_admin_settings_to_config(config: &mut AppConfig, settings: AdminSettings) {
    config.model.default_model = settings.default_model;
    config.model.default_reasoning_effort = settings.default_reasoning_effort;
    config.model.service_tier = settings.service_tier;
    config.model.aliases = settings.model_aliases;
    config.auth.refresh_enabled = settings.refresh_enabled;
    config.auth.refresh_margin_seconds = settings.refresh_margin_seconds;
    config.auth.refresh_concurrency = settings.refresh_concurrency;
    config.auth.max_concurrent_per_account = settings.max_concurrent_per_account;
    config.auth.request_interval_ms = settings.request_interval_ms;
    config.auth.rotation_strategy = settings.rotation_strategy;
    config.auth.tier_priority = settings.tier_priority;
    config.quota.refresh_interval_minutes = settings.quota_refresh_interval_minutes;
    config.quota.warning_thresholds = QuotaWarningThresholds {
        primary: settings.quota_warning_thresholds.primary,
        secondary: settings.quota_warning_thresholds.secondary,
    };
    config.quota.skip_exhausted = settings.quota_skip_exhausted;
    config.logging.enabled = settings.logs_enabled;
    config.logging.capacity = settings.logs_capacity;
    config.logging.capture_body = settings.logs_capture_body;
    config.usage_stats.history_retention_days = settings.usage_history_retention_days;
}
