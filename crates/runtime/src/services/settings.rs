use super::*;

/// 运行时设置服务。
#[derive(Clone)]
pub struct RuntimeSettingsService {
    current: Arc<StdRwLock<Arc<AppConfig>>>,
    local_config_path: Arc<PathBuf>,
}

impl RuntimeSettingsService {
    /// 构造运行时设置服务。
    pub fn new(config: AppConfig) -> Self {
        Self::with_local_config_path(config, "local.yaml")
    }

    /// 构造带本地配置覆盖路径的运行时设置服务。
    pub fn with_local_config_path(
        config: AppConfig,
        local_config_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            current: Arc::new(StdRwLock::new(Arc::new(config))),
            local_config_path: Arc::new(local_config_path.into()),
        }
    }

    /// 返回当前配置快照。
    pub fn current(&self) -> Arc<AppConfig> {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Return the local settings overlay path configured for this runtime.
    pub fn local_config_path(&self) -> Arc<PathBuf> {
        self.local_config_path.clone()
    }

    /// 更新当前设置并写入本地配置覆盖文件。
    pub async fn update(
        &self,
        patch: AdminSettingsPatch,
    ) -> Result<Arc<AppConfig>, RuntimeSettingsError> {
        let mut next = (*self.current()).clone();
        let mut settings = admin_settings_from_config(&next);
        SettingsService::apply_patch(&mut settings, patch)?;
        apply_admin_settings_to_config(&mut next, settings);
        next.write_settings_overlay(self.local_config_path.as_ref())
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
    /// 设置补丁验证失败。
    #[error(transparent)]
    InvalidField(#[from] SettingsServiceError),
    /// 本地配置覆盖写入失败。
    #[error(transparent)]
    Persist(#[from] ConfigWriteError),
}

fn admin_settings_from_config(config: &AppConfig) -> AdminSettings {
    AdminSettings {
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
        quota_warning_thresholds: AdminQuotaWarningThresholds {
            primary: config.quota.warning_thresholds.primary.clone(),
            secondary: config.quota.warning_thresholds.secondary.clone(),
        },
        quota_skip_exhausted: config.quota.skip_exhausted,
        logs_enabled: config.logging.enabled,
        logs_capacity: config.logging.capacity,
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
