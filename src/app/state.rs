//! 应用状态 —— AppState。

use crate::app::services::Services;
use crate::config::types::AppConfig;

/// 应用状态。
#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub services: Services,
}

/// 运行时配置镜像（从 AppConfig 衍生）。
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub model: crate::config::types::ModelConfig,
    pub admin: crate::config::types::AdminConfig,
    pub auth: crate::config::types::AuthConfig,
    pub quota: crate::config::types::QuotaConfig,
}

impl From<AppConfig> for RuntimeConfig {
    fn from(config: AppConfig) -> Self {
        Self {
            model: config.model.clone(),
            admin: config.admin.clone(),
            auth: config.auth.clone(),
            quota: config.quota,
        }
    }
}
