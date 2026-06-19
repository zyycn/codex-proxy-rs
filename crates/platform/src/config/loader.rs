use std::path::Path;

use serde::Serialize;

use super::{
    types::{AppConfig, AuthConfig, LoggingConfig, ModelConfig, QuotaConfig, UsageStatsConfig},
    ConfigResult, ConfigWriteError, ConfigWriteResult,
};

impl AppConfig {
    /// 从当前目录加载配置文件。
    pub fn load() -> ConfigResult<Self> {
        Self::load_from_dir(".")
    }

    /// 从指定目录加载配置文件。
    pub fn load_from_dir(config_dir: impl AsRef<Path>) -> ConfigResult<Self> {
        let config_dir = config_dir.as_ref();
        ::config::Config::builder()
            .add_source(::config::File::from(config_dir.join("config.yaml")).required(true))
            .add_source(::config::File::from(config_dir.join("local.yaml")).required(false))
            .add_source(::config::File::from(config_dir.join("local.yml")).required(false))
            .build()?
            .try_deserialize()
    }

    /// 写入管理端设置使用的本地配置覆盖文件。
    pub async fn write_settings_overlay(
        &self,
        local_config_path: impl AsRef<Path>,
    ) -> ConfigWriteResult<()> {
        let local_config_path = local_config_path.as_ref();
        if let Some(parent) = local_config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ConfigWriteError::CreateDirectory)?;
        }

        let overlay = SettingsConfigOverlay::from(self);
        let content = serde_yml::to_string(&overlay).map_err(ConfigWriteError::Serialize)?;
        tokio::fs::write(local_config_path, content)
            .await
            .map_err(ConfigWriteError::Write)
    }
}

#[derive(Debug, Serialize)]
struct SettingsConfigOverlay {
    model: ModelConfig,
    auth: AuthConfig,
    quota: QuotaConfig,
    usage_stats: UsageStatsConfig,
    logging: LoggingConfig,
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
