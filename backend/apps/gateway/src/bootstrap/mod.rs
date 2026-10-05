//! 网关唯一组合根：按启动模式连接各 Bundle

mod command;
mod config;
mod plugins;
mod server;
mod startup;

pub use command::{PluginCommand, plugin_command};
pub use config::GatewayConfig;
pub use server::run;

/// 组合根只保留包级错误分类，不展开内部实现或敏感配置
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Worker(#[from] gateway_core::task::WorkerDefinitionError),
    #[error(transparent)]
    PluginCommand(#[from] gateway_plugin_runtime::PluginCommandError),
    #[error(transparent)]
    Config(#[from] gateway_host::ConfigError),
    #[error(transparent)]
    Host(#[from] gateway_host::HostError),
    #[error(transparent)]
    Outbound(#[from] gateway_host::outbound::HttpError),
    #[error(transparent)]
    Store(#[from] gateway_store::StoreError),
    #[error(transparent)]
    CommandStoreDrain(#[from] gateway_store::CommandStoreDrainError),
    #[error(transparent)]
    OpenAi(#[from] provider_openai::OpenAiInitializeError),
    #[error(transparent)]
    Xai(#[from] provider_xai::XaiInitializeError),
    #[error(transparent)]
    Registry(#[from] gateway_core::engine::provider::RegistryError),
    #[error(transparent)]
    Core(#[from] gateway_core::CoreError),
    #[error(transparent)]
    Admin(#[from] gateway_admin::model::AdminError),
    #[error(transparent)]
    Api(#[from] gateway_api::ApiError),
}
