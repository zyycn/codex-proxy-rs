use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use thiserror::Error;
use tokio::sync::RwLock;

use crate::config::{
    AppConfig, AuthConfig, LoggingConfig, ModelConfig, QuotaConfig, QuotaWarningThresholds,
    UsageStatsConfig,
};

const ROTATION_STRATEGIES: [&str; 3] = ["least_used", "round_robin", "sticky"];

#[derive(Clone)]
pub struct SettingsService {
    current: Arc<RwLock<AppConfig>>,
    local_config_path: Arc<PathBuf>,
}

#[derive(Debug, Default)]
pub struct SettingsUpdate {
    pub default_model: Option<String>,
    pub default_reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub model_aliases: Option<BTreeMap<String, String>>,
    pub refresh_enabled: Option<bool>,
    pub refresh_margin_seconds: Option<u64>,
    pub refresh_concurrency: Option<u32>,
    pub max_concurrent_per_account: Option<usize>,
    pub request_interval_ms: Option<u64>,
    pub rotation_strategy: Option<String>,
    pub tier_priority: Option<Vec<String>>,
    pub quota_refresh_interval_minutes: Option<u64>,
    pub quota_warning_thresholds: Option<QuotaWarningThresholds>,
    pub quota_skip_exhausted: Option<bool>,
    pub logs_enabled: Option<bool>,
    pub logs_capacity: Option<u32>,
    pub logs_capture_body: Option<bool>,
    pub usage_history_retention_days: Option<u64>,
}

#[derive(Debug, Error)]
pub enum SettingsServiceError {
    #[error("invalid setting `{field}`: {message}")]
    InvalidField { field: String, message: String },
    #[error("serialize settings overlay")]
    Serialize(#[source] serde_yaml::Error),
    #[error("create settings directory")]
    CreateDirectory(#[source] std::io::Error),
    #[error("write settings overlay")]
    Write(#[source] std::io::Error),
}

#[derive(Debug, serde::Serialize)]
struct SettingsConfigOverlay {
    model: ModelConfig,
    auth: AuthConfig,
    quota: QuotaConfig,
    usage_stats: UsageStatsConfig,
    logging: LoggingConfig,
}

impl SettingsService {
    pub fn new(config: AppConfig, local_config_path: impl Into<PathBuf>) -> Self {
        Self {
            current: Arc::new(RwLock::new(config)),
            local_config_path: Arc::new(local_config_path.into()),
        }
    }

    pub async fn current(&self) -> AppConfig {
        self.current.read().await.clone()
    }

    pub async fn update(&self, update: SettingsUpdate) -> Result<AppConfig, SettingsServiceError> {
        let mut current = self.current.write().await;
        let mut next = current.clone();
        apply_update(&mut next, update)?;
        self.write_overlay(&next).await?;
        *current = next.clone();
        Ok(next)
    }

    async fn write_overlay(&self, config: &AppConfig) -> Result<(), SettingsServiceError> {
        if let Some(parent) = self
            .local_config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(SettingsServiceError::CreateDirectory)?;
        }

        let overlay = SettingsConfigOverlay::from(config);
        let content = serde_yaml::to_string(&overlay).map_err(SettingsServiceError::Serialize)?;
        tokio::fs::write(self.local_config_path.as_ref(), content)
            .await
            .map_err(SettingsServiceError::Write)
    }
}

impl From<&AppConfig> for SettingsConfigOverlay {
    fn from(config: &AppConfig) -> Self {
        Self {
            model: config.model.clone(),
            auth: config.auth.clone(),
            quota: config.quota.clone(),
            usage_stats: config.usage_stats.clone(),
            logging: config.logging.clone(),
        }
    }
}

fn apply_update(
    config: &mut AppConfig,
    update: SettingsUpdate,
) -> Result<(), SettingsServiceError> {
    if let Some(default_model) = update.default_model {
        config.model.default_model = non_empty_string("defaultModel", default_model)?;
    }
    if let Some(default_reasoning_effort) = update.default_reasoning_effort {
        config.model.default_reasoning_effort = Some(non_empty_string(
            "defaultReasoningEffort",
            default_reasoning_effort,
        )?);
    }
    if let Some(service_tier) = update.service_tier {
        config.model.service_tier = Some(non_empty_string("serviceTier", service_tier)?);
    }
    if let Some(model_aliases) = update.model_aliases {
        config.model.aliases = validate_aliases(model_aliases)?;
    }
    if let Some(refresh_enabled) = update.refresh_enabled {
        config.auth.refresh_enabled = refresh_enabled;
    }
    if let Some(refresh_margin_seconds) = update.refresh_margin_seconds {
        config.auth.refresh_margin_seconds = refresh_margin_seconds;
    }
    if let Some(refresh_concurrency) = update.refresh_concurrency {
        config.auth.refresh_concurrency = positive_u32("refreshConcurrency", refresh_concurrency)?;
    }
    if let Some(max_concurrent_per_account) = update.max_concurrent_per_account {
        config.auth.max_concurrent_per_account =
            positive_usize("maxConcurrentPerAccount", max_concurrent_per_account)?;
    }
    if let Some(request_interval_ms) = update.request_interval_ms {
        config.auth.request_interval_ms = request_interval_ms;
    }
    if let Some(rotation_strategy) = update.rotation_strategy {
        config.auth.rotation_strategy = validate_rotation_strategy(rotation_strategy)?;
    }
    if let Some(tier_priority) = update.tier_priority {
        config.auth.tier_priority = validate_tier_priority(tier_priority)?;
    }
    if let Some(quota_refresh_interval_minutes) = update.quota_refresh_interval_minutes {
        config.quota.refresh_interval_minutes = positive_u64(
            "quotaRefreshIntervalMinutes",
            quota_refresh_interval_minutes,
        )?;
    }
    if let Some(quota_warning_thresholds) = update.quota_warning_thresholds {
        validate_thresholds(&quota_warning_thresholds)?;
        config.quota.warning_thresholds = quota_warning_thresholds;
    }
    if let Some(quota_skip_exhausted) = update.quota_skip_exhausted {
        config.quota.skip_exhausted = quota_skip_exhausted;
    }
    if let Some(logs_enabled) = update.logs_enabled {
        config.logging.enabled = logs_enabled;
    }
    if let Some(logs_capacity) = update.logs_capacity {
        config.logging.capacity = positive_u32("logsCapacity", logs_capacity)?;
    }
    if let Some(logs_capture_body) = update.logs_capture_body {
        config.logging.capture_body = logs_capture_body;
    }
    if let Some(usage_history_retention_days) = update.usage_history_retention_days {
        config.usage_stats.history_retention_days = Some(positive_u64(
            "usageHistoryRetentionDays",
            usage_history_retention_days,
        )?);
    }

    Ok(())
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

fn validate_thresholds(thresholds: &QuotaWarningThresholds) -> Result<(), SettingsServiceError> {
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
