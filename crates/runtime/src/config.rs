//! 配置映射。

use codex_proxy_platform::config as platform;

/// 运行时配置视图。
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// 模型配置。
    pub model: platform::ModelConfig,
    /// 管理端配置。
    pub admin: platform::AdminConfig,
    /// 认证与刷新配置。
    pub auth: platform::AuthConfig,
    /// 配额刷新配置。
    pub quota: platform::QuotaConfig,
}

impl From<platform::AppConfig> for RuntimeConfig {
    fn from(config: platform::AppConfig) -> Self {
        Self {
            model: config.model,
            admin: config.admin,
            auth: config.auth,
            quota: config.quota,
        }
    }
}
