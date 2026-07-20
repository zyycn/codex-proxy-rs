//! 网关唯一组合根：加载包级配置并连接各 Bundle。

use gateway_core::engine::provider::{ProviderRegistry, RegistryError};
use gateway_host::{ConfigError, HostConfig, LoadableConfig};
use serde::Deserialize;

const CONFIG_SCHEMA_VERSION: u32 = 1;

/// 顶层配置只组合各包拥有的配置段，不解释任何业务字段。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    schema_version: u32,
    host: HostConfig,
    store: gateway_store::StoreConfig,
    admin: gateway_admin::AdminConfig,
    api: gateway_api::ApiConfig,
    openai: provider_openai::OpenAiConfig,
    xai: provider_xai::XaiConfig,
}

impl LoadableConfig for GatewayConfig {
    fn resolve_and_validate(&mut self, source_dir: &std::path::Path) -> Result<(), ConfigError> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::InvalidField("schema_version"));
        }
        self.host.resolve_and_validate(source_dir)?;
        self.store
            .resolve_and_validate(source_dir)
            .map_err(|_| ConfigError::InvalidField("store"))?;
        self.admin
            .resolve_and_validate(source_dir)
            .map_err(|_| ConfigError::InvalidField("admin"))?;
        self.api
            .resolve_and_validate(source_dir)
            .map_err(|_| ConfigError::InvalidField("api"))?;
        self.openai
            .resolve_and_validate(source_dir)
            .map_err(|_| ConfigError::InvalidField("openai"))?;
        self.xai
            .resolve_and_validate(source_dir)
            .map_err(|_| ConfigError::InvalidField("xai"))?;
        Ok(())
    }
}

/// 按冻结顺序初始化全部 Bundle，并把进程阻塞权交给 Host。
pub async fn run() -> Result<(), BootstrapError> {
    let config = gateway_host::load_config::<GatewayConfig>()?;
    let GatewayConfig {
        schema_version: _,
        host,
        store,
        admin,
        api,
        openai,
        xai,
    } = config;

    let host = gateway_host::initialize(host).await?;
    let mut store = gateway_store::initialize(store).await?;
    let provider_ports = store.provider_ports();
    let mut openai = provider_openai::initialize(openai, provider_ports.clone()).await?;
    let mut xai = provider_xai::initialize(xai, provider_ports).await?;
    let providers = ProviderRegistry::new([openai.core_provider(), xai.core_provider()])?;
    let mut core = gateway_core::initialize(store.core_ports(), providers).await?;
    let admin = gateway_admin::initialize(
        admin,
        store.admin_ports(),
        vec![openai.admin_provider(), xai.admin_provider()],
        core.snapshot_control(),
        core.account_probe(),
        host.system_operations(),
    )
    .await?;

    let mut probes = store.health_probes();
    probes.extend(core.health_probes());
    let api = gateway_api::initialize(
        api,
        core.execution_service(),
        admin.services(),
        probes,
        host.worker_health(),
        host.connection_lifecycle(),
    )?;

    let mut plan = store.take_worker_contributions();
    plan.extend(core.take_worker_contributions());
    plan.extend(openai.take_worker_contributions());
    plan.extend(xai.take_worker_contributions());
    host.start_workers(plan, store.worker_leader_lease())?;
    host.serve(api.router()).await?;
    Ok(())
}

/// 组合根只保留包级错误分类，不展开内部实现或敏感配置。
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Config(#[from] gateway_host::ConfigError),
    #[error(transparent)]
    Host(#[from] gateway_host::HostError),
    #[error(transparent)]
    Store(#[from] gateway_store::StoreError),
    #[error(transparent)]
    OpenAi(#[from] provider_openai::OpenAiInitializeError),
    #[error(transparent)]
    Xai(#[from] provider_xai::XaiInitializeError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Core(#[from] gateway_core::CoreError),
    #[error(transparent)]
    Admin(#[from] gateway_admin::model::AdminError),
    #[error(transparent)]
    Api(#[from] gateway_api::ApiError),
}
