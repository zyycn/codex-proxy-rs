//! 管理端设置领域逻辑。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const ROTATION_STRATEGIES: [&str; 3] = ["least_used", "round_robin", "sticky"];

/// 管理端可变设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSettings {
    /// 默认模型。
    pub default_model: String,
    /// 默认 reasoning effort。
    pub default_reasoning_effort: Option<String>,
    /// 默认服务层级。
    pub service_tier: Option<String>,
    /// 模型别名。
    pub model_aliases: BTreeMap<String, String>,
    /// 是否启用 token 刷新。
    pub refresh_enabled: bool,
    /// token 刷新提前秒数。
    pub refresh_margin_seconds: u64,
    /// token 刷新并发度。
    pub refresh_concurrency: u32,
    /// 单账号最大并发请求数。
    pub max_concurrent_per_account: usize,
    /// 单账号请求间隔毫秒。
    pub request_interval_ms: u64,
    /// 账号轮换策略。
    pub rotation_strategy: String,
    /// 订阅层级优先级。
    pub tier_priority: Vec<String>,
    /// 配额刷新间隔分钟。
    pub quota_refresh_interval_minutes: u64,
    /// 配额预警阈值。
    pub quota_warning_thresholds: AdminQuotaWarningThresholds,
    /// 是否跳过配额耗尽账号。
    pub quota_skip_exhausted: bool,
    /// 是否启用日志。
    pub logs_enabled: bool,
    /// 日志容量。
    pub logs_capacity: u32,
    /// 是否捕获请求/响应体。
    pub logs_capture_body: bool,
    /// 用量历史保留天数。
    pub usage_history_retention_days: Option<u64>,
}

impl Default for AdminSettings {
    fn default() -> Self {
        Self {
            default_model: "gpt-5.5".to_string(),
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
    /// 主阈值列表。
    pub primary: Vec<u8>,
    /// 次阈值列表。
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
    /// 默认模型。
    pub default_model: Option<String>,
    /// 默认 reasoning effort。
    pub default_reasoning_effort: Option<String>,
    /// 默认服务层级。
    pub service_tier: Option<String>,
    /// 模型别名。
    pub model_aliases: Option<BTreeMap<String, String>>,
    /// 是否启用 token 刷新。
    pub refresh_enabled: Option<bool>,
    /// token 刷新提前秒数。
    pub refresh_margin_seconds: Option<u64>,
    /// token 刷新并发度。
    pub refresh_concurrency: Option<u32>,
    /// 单账号最大并发请求数。
    pub max_concurrent_per_account: Option<usize>,
    /// 单账号请求间隔毫秒。
    pub request_interval_ms: Option<u64>,
    /// 账号轮换策略。
    pub rotation_strategy: Option<String>,
    /// 订阅层级优先级。
    pub tier_priority: Option<Vec<String>>,
    /// 配额刷新间隔分钟。
    pub quota_refresh_interval_minutes: Option<u64>,
    /// 配额预警阈值。
    pub quota_warning_thresholds: Option<AdminQuotaWarningThresholds>,
    /// 是否跳过配额耗尽账号。
    pub quota_skip_exhausted: Option<bool>,
    /// 是否启用日志。
    pub logs_enabled: Option<bool>,
    /// 日志容量。
    pub logs_capacity: Option<u32>,
    /// 是否捕获请求/响应体。
    pub logs_capture_body: Option<bool>,
    /// 用量历史保留天数。
    pub usage_history_retention_days: Option<u64>,
}

/// 设置领域服务。
#[derive(Debug, Clone, Default)]
pub struct SettingsService;

impl SettingsService {
    /// 合并并验证补丁设置。
    pub fn apply_patch(
        current: &mut AdminSettings,
        patch: AdminSettingsPatch,
    ) -> Result<(), SettingsServiceError> {
        if let Some(default_model) = patch.default_model {
            current.default_model = non_empty_string("defaultModel", default_model)?;
        }
        if let Some(default_reasoning_effort) = patch.default_reasoning_effort {
            current.default_reasoning_effort = Some(non_empty_string(
                "defaultReasoningEffort",
                default_reasoning_effort,
            )?);
        }
        if let Some(service_tier) = patch.service_tier {
            current.service_tier = Some(non_empty_string("serviceTier", service_tier)?);
        }
        if let Some(model_aliases) = patch.model_aliases {
            current.model_aliases = validate_aliases(model_aliases)?;
        }
        if let Some(refresh_enabled) = patch.refresh_enabled {
            current.refresh_enabled = refresh_enabled;
        }
        if let Some(refresh_margin_seconds) = patch.refresh_margin_seconds {
            current.refresh_margin_seconds = refresh_margin_seconds;
        }
        if let Some(refresh_concurrency) = patch.refresh_concurrency {
            current.refresh_concurrency = positive_u32("refreshConcurrency", refresh_concurrency)?;
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
        if let Some(tier_priority) = patch.tier_priority {
            current.tier_priority = validate_tier_priority(tier_priority)?;
        }
        if let Some(quota_refresh_interval_minutes) = patch.quota_refresh_interval_minutes {
            current.quota_refresh_interval_minutes = positive_u64(
                "quotaRefreshIntervalMinutes",
                quota_refresh_interval_minutes,
            )?;
        }
        if let Some(quota_warning_thresholds) = patch.quota_warning_thresholds {
            validate_thresholds(&quota_warning_thresholds)?;
            current.quota_warning_thresholds = quota_warning_thresholds;
        }
        if let Some(quota_skip_exhausted) = patch.quota_skip_exhausted {
            current.quota_skip_exhausted = quota_skip_exhausted;
        }
        if let Some(logs_enabled) = patch.logs_enabled {
            current.logs_enabled = logs_enabled;
        }
        if let Some(logs_capacity) = patch.logs_capacity {
            current.logs_capacity = positive_u32("logsCapacity", logs_capacity)?;
        }
        if let Some(logs_capture_body) = patch.logs_capture_body {
            current.logs_capture_body = logs_capture_body;
        }
        if let Some(usage_history_retention_days) = patch.usage_history_retention_days {
            current.usage_history_retention_days = Some(positive_u64(
                "usageHistoryRetentionDays",
                usage_history_retention_days,
            )?);
        }

        Ok(())
    }
}

/// 设置领域错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SettingsServiceError {
    /// 设置字段非法。
    #[error("invalid setting `{field}`: {message}")]
    InvalidField {
        /// 字段名。
        field: String,
        /// 错误说明。
        message: String,
    },
}

impl SettingsServiceError {
    /// 返回出错字段名。
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

fn validate_aliases(
    aliases: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, SettingsServiceError> {
    let mut validated = BTreeMap::new();
    for (alias, target) in aliases {
        let alias = non_empty_string("modelAliases", alias)?;
        let target = non_empty_string("modelAliases", target)?;
        validated.insert(alias, target);
    }
    Ok(validated)
}

fn validate_tier_priority(tiers: Vec<String>) -> Result<Vec<String>, SettingsServiceError> {
    let mut validated = Vec::with_capacity(tiers.len());
    for tier in tiers {
        validated.push(non_empty_string("tierPriority", tier)?);
    }
    Ok(validated)
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

fn validate_thresholds(
    thresholds: &AdminQuotaWarningThresholds,
) -> Result<(), SettingsServiceError> {
    if thresholds.primary.is_empty() && thresholds.secondary.is_empty() {
        return Err(invalid_field(
            "quotaWarningThresholds",
            "must include at least one threshold",
        ));
    }
    if thresholds
        .primary
        .iter()
        .chain(thresholds.secondary.iter())
        .any(|threshold| *threshold > 100)
    {
        return Err(invalid_field(
            "quotaWarningThresholds",
            "thresholds must be between 0 and 100",
        ));
    }
    Ok(())
}

fn non_empty_string(field: &str, value: String) -> Result<String, SettingsServiceError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        Err(invalid_field(field, "must not be empty"))
    } else {
        Ok(value)
    }
}

fn positive_u32(field: &str, value: u32) -> Result<u32, SettingsServiceError> {
    if value == 0 {
        Err(invalid_field(field, "must be greater than 0"))
    } else {
        Ok(value)
    }
}

fn positive_u64(field: &str, value: u64) -> Result<u64, SettingsServiceError> {
    if value == 0 {
        Err(invalid_field(field, "must be greater than 0"))
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
