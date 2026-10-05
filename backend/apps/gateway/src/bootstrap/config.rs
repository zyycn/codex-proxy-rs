//! 网关顶层配置的加载、路径解析与跨模块参数校验

use gateway_host::{ConfigError, HostConfig, LoadableConfig};
use serde::Deserialize;

const CONFIG_SCHEMA_VERSION: u32 = 1;

/// 顶层配置只组合各包拥有的配置段，不解释任何业务字段
/// 各启动配置忽略未知字段，允许升级时保留旧配置；已知字段仍按类型和业务约束校验
#[derive(Debug, Deserialize)]
pub struct GatewayConfig {
    schema_version: u32,
    pub(super) host: HostConfig,
    pub(super) store: gateway_store::StoreConfig,
    pub(super) admin: gateway_admin::AdminConfig,
    #[serde(default)]
    pub(super) client: gateway_admin::ClientConfig,
    pub(super) api: gateway_api::ApiConfig,
    #[serde(default)]
    pub(super) openai: provider_openai::OpenAiConfig,
}

impl LoadableConfig for GatewayConfig {
    const EXTERNAL_SECTIONS: &'static [&'static str] = &["services"];

    fn resolve_and_validate(&mut self, source_dir: &std::path::Path) -> Result<(), ConfigError> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::InvalidField("schema_version"));
        }
        self.api
            .resolve_and_validate(source_dir)
            .map_err(|_| ConfigError::InvalidField("api"))?;
        self.host
            .resolve_and_validate(source_dir, &self.api.asset_directory)?;
        let runtime_data_dir = self.host.runtime_data_dir().to_path_buf();
        self.store
            .resolve_and_validate(&runtime_data_dir)
            .map_err(|_| ConfigError::InvalidField("store"))?;
        self.admin
            .resolve_and_validate(source_dir)
            .map_err(|_| ConfigError::InvalidField("admin"))?;
        self.client
            .resolve_and_validate(source_dir)
            .map_err(|_| ConfigError::InvalidField("client"))?;
        self.openai
            .resolve_and_validate(&runtime_data_dir)
            .map_err(|_| ConfigError::InvalidField("openai"))?;
        Ok(())
    }
}
