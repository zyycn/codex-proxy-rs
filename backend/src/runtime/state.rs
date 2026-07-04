//! 运行时共享状态。

use crate::config::schema::AppConfig;
use crate::runtime::services::Services;

/// HTTP handler 共享的运行时状态。
#[derive(Clone)]
pub struct AppState {
    pub services: Services,
}

/// 运行时配置镜像（从 AppConfig 衍生）。
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub admin: crate::config::schema::AdminConfig,
    pub auth: crate::config::schema::AuthConfig,
    pub quota: crate::config::schema::QuotaConfig,
}

impl From<AppConfig> for RuntimeConfig {
    fn from(config: AppConfig) -> Self {
        Self {
            admin: config.admin.clone(),
            auth: config.auth.clone(),
            quota: config.quota,
        }
    }
}
