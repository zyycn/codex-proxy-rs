//! 网关唯一组合根：加载包级配置并连接各 Bundle。

use gateway_core::engine::provider::{ProviderRegistry, RegistryError};
use gateway_host::{ConfigError, HostConfig, LoadableConfig};
use serde::Deserialize;

const CONFIG_SCHEMA_VERSION: u32 = 1;

/// 顶层配置只组合各包拥有的配置段，不解释任何业务字段。
/// 各启动配置忽略未知字段，允许升级时保留旧配置；已知字段仍按类型和业务约束校验。
#[derive(Debug, Deserialize)]
pub struct GatewayConfig {
    schema_version: u32,
    host: HostConfig,
    store: gateway_store::StoreConfig,
    admin: gateway_admin::AdminConfig,
    #[serde(default)]
    client: gateway_admin::ClientConfig,
    api: gateway_api::ApiConfig,
    #[serde(default)]
    openai: provider_openai::OpenAiConfig,
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

/// 按冻结顺序初始化全部 Bundle，并把进程阻塞权交给 Host。
pub async fn run() -> Result<(), BootstrapError> {
    launch(None).await.map(|_| ())
}

/// 这里只接收插件命名空间内的参数，不转交宿主配置或其他启动 secret。
pub struct PluginCommand {
    pub instance_id: Option<String>,
    pub name: Option<String>,
    pub arguments: Vec<String>,
}

impl PluginCommand {
    fn is_help(&self) -> bool {
        self.name.is_none()
            || self
                .arguments
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h"))
    }
}

pub async fn plugin_command(
    command: PluginCommand,
) -> Result<gateway_plugin_runtime::PluginCommandOutput, BootstrapError> {
    launch(Some(command)).await
}

async fn launch(
    command: Option<PluginCommand>,
) -> Result<gateway_plugin_runtime::PluginCommandOutput, BootstrapError> {
    macro_rules! or_shutdown {
        ($runtime:expr, $result:expr) => {
            match $result {
                Ok(value) => value,
                Err(error) => {
                    $runtime.shutdown().await;
                    return Err(error.into());
                }
            }
        };
    }

    let config = gateway_host::load_config::<GatewayConfig>()?;
    let GatewayConfig {
        schema_version: _,
        host,
        store,
        admin,
        client,
        api,
        openai,
    } = config;

    let plugin_cache = host.runtime_data_dir().join("plugins");
    let host = if command.is_some() {
        gateway_host::initialize_command_line(host).await?
    } else {
        gateway_host::initialize(host).await?
    };
    host.report_startup_ready("Host");
    let help = command.as_ref().is_some_and(PluginCommand::is_help);
    let mut store = if help {
        gateway_store::initialize_read_only(store).await?
    } else if command.is_some() {
        gateway_store::initialize_command_line(store).await?
    } else {
        gateway_store::initialize(store).await?
    };
    host.report_startup_ready("Store");
    let version = host
        .system_operations()
        .version()
        .await
        .map_err(|_| ConfigError::InvalidField("host.version"))?
        .version;
    let plugin_http = std::sync::Arc::new(gateway_host::outbound::HttpClient::new()?);
    if let Some(command) = command.as_ref().filter(|_| help) {
        let runtime = prepare_plugins(&store, plugin_cache, &version, plugin_http.clone())?;
        let commands = or_shutdown!(runtime, runtime.prepare_command_line().await);
        let stdout = commands.help(command.instance_id.as_deref(), command.name.as_deref());
        commands.shutdown().await;
        runtime.shutdown().await;
        return Ok(gateway_plugin_runtime::PluginCommandOutput {
            stdout: stdout?,
            stderr: String::new(),
            exit_code: 0,
            saved_accounts: 0,
        });
    }
    let provider_ports = store.provider_ports();
    let mut openai = provider_openai::initialize(openai, provider_ports.clone()).await?;
    host.report_startup_ready("OpenAI Provider");
    let mut xai = provider_xai::initialize(provider_ports).await?;
    host.report_startup_ready("xAI Provider");
    let providers = ProviderRegistry::new([openai.core_provider(), xai.core_provider()])?;
    let plugin_runtime = prepare_plugins(&store, plugin_cache, &version, plugin_http.clone())?;
    let admin_providers = gateway_admin::ports::provider::ProviderAdminRegistry::new([
        openai.admin_provider(),
        xai.admin_provider(),
    ])
    .map_err(|_| gateway_admin::model::AdminError::invalid("Provider 管理身份冲突"))?;
    if let Some(command) = command {
        let instance_id = or_shutdown!(
            plugin_runtime,
            command
                .instance_id
                .as_deref()
                .ok_or_else(|| gateway_admin::model::AdminError::invalid("插件实例 ID 缺失"))
        );
        let name = or_shutdown!(
            plugin_runtime,
            command
                .name
                .as_deref()
                .ok_or_else(|| gateway_admin::model::AdminError::invalid("插件命令名缺失"))
        );
        let core = gateway_core::prepare_command_plane(
            store.core_ports(),
            providers.clone(),
            Some(plugin_runtime.clone()),
            Some(plugin_runtime.observer_registry()),
            Some(plugin_runtime.policy_registry()),
            Some(plugin_runtime.middleware_registry()),
        );
        let accounts = gateway_admin::initialize_plugin_accounts(
            admin_providers.clone(),
            store.admin_ports().accounts(),
            core.snapshot_control(),
        );
        or_shutdown!(plugin_runtime, plugin_runtime.bind_account_ports(&accounts));
        let keys = gateway_admin::initialize_plugin_client_keys(
            admin_providers.clone(),
            store.admin_ports().client_keys(),
            core.snapshot_control(),
        );
        or_shutdown!(plugin_runtime, plugin_runtime.bind_client_key_ports(&keys));
        let plugin_resources = gateway_admin::initialize_plugin_resources(
            store.admin_ports().plugin_resources(),
            core.snapshot_control(),
        );
        or_shutdown!(
            plugin_runtime,
            plugin_runtime.bind_resource_ports(&plugin_resources)
        );
        let core = or_shutdown!(plugin_runtime, core.activate().await);
        let nested_models = core.nested_model_execution_port();
        let affinity_lookup = core.affinity_lookup_port();
        or_shutdown!(
            plugin_runtime,
            plugin_runtime.bind_model_ports(&nested_models, &affinity_lookup)
        );
        let commands = or_shutdown!(plugin_runtime, plugin_runtime.prepare_command_line().await);
        or_shutdown!(plugin_runtime, store.start_command_line_writes());
        let result = commands
            .execute_cancellable(instance_id, name, &command.arguments, &host.cancellation())
            .await;
        commands.shutdown().await;
        plugin_runtime.shutdown().await;
        store.shutdown_command_line_writes().await?;
        return Ok(result?);
    }
    let core = gateway_core::prepare(
        store.core_ports(),
        providers,
        Some(plugin_runtime.clone()),
        Some(plugin_runtime.observer_registry()),
        Some(plugin_runtime.policy_registry()),
        Some(plugin_runtime.middleware_registry()),
        Some(plugin_runtime.frontend_authentication_registry()),
    );
    let plugin_accounts = gateway_admin::initialize_plugin_accounts(
        admin_providers.clone(),
        store.admin_ports().accounts(),
        core.snapshot_control(),
    );
    or_shutdown!(
        plugin_runtime,
        plugin_runtime.bind_account_ports(&plugin_accounts)
    );
    let plugin_keys = gateway_admin::initialize_plugin_client_keys(
        admin_providers.clone(),
        store.admin_ports().client_keys(),
        core.snapshot_control(),
    );
    or_shutdown!(
        plugin_runtime,
        plugin_runtime.bind_client_key_ports(&plugin_keys)
    );
    let plugin_resources = gateway_admin::initialize_plugin_resources(
        store.admin_ports().plugin_resources(),
        core.snapshot_control(),
    );
    or_shutdown!(
        plugin_runtime,
        plugin_runtime.bind_resource_ports(&plugin_resources)
    );
    let mut core = or_shutdown!(plugin_runtime, core.activate().await);
    let nested_models = core.nested_model_execution_port();
    let affinity_lookup = core.affinity_lookup_port();
    or_shutdown!(
        plugin_runtime,
        plugin_runtime.bind_model_ports(&nested_models, &affinity_lookup)
    );
    host.report_startup_ready("Core");
    let host_version = or_shutdown!(
        plugin_runtime,
        version
            .trim_start_matches('v')
            .parse()
            .map_err(|_| ConfigError::InvalidField("host.version"))
    );
    let plugin_inspector = std::sync::Arc::new(gateway_plugin_runtime::PackageInspector::new(
        gateway_plugin_runtime::PackageLimits::default(),
        host_version,
    ));
    let plugin_distribution = std::sync::Arc::new(or_shutdown!(
        plugin_runtime,
        gateway_host::plugin_distribution::HttpPluginDistribution::new(plugin_http)
    ));
    let mut admin = or_shutdown!(
        plugin_runtime,
        gateway_admin::initialize_with_plugin_accounts(
            admin,
            client,
            store.admin_ports(),
            gateway_admin::AdminRuntimePorts {
                providers: admin_providers,
                plugin_preparation: plugin_runtime.clone(),
                plugin_management: plugin_runtime.clone(),
                published_snapshot: core.snapshots(),
                plugin_inspector,
                plugin_distribution,
                pricing_source: std::sync::Arc::new(gateway_host::pricing::ModelsDevPricing),
                snapshot: core.snapshot_control(),
                account_probe: core.account_probe(),
                proxy_probe: host.proxy_probe(provider_openai::build_reqwest_client_with_custom_ca),
                client_distribution: host.client_distribution_resolver(),
                system: host.system_operations(),
                client_key_verifier: core.client_key_verifier(),
            },
            plugin_accounts,
        )
        .await
    );
    let official_files = host.official_plugin_release_files();
    let official_identity = host.official_plugin_release_identity();
    let official_import = or_shutdown!(
        plugin_runtime,
        admin
            .services()
            .plugins()
            .import_official_release(
                official_files.as_ref(),
                &official_identity,
                &gateway_admin::model::MutationContext {
                    actor: gateway_admin::model::MutationActor::System,
                    request_id: "startup-official-plugin-import".to_owned(),
                },
            )
            .await
    );
    if official_import.artifacts > 0 {
        host.report_startup_ready("Official plugins");
    }
    host.report_startup_ready("Admin");

    let mut probes = store.health_probes();
    probes.extend(core.health_probes());
    probes.push(host.logging_health_probe());
    let api = or_shutdown!(
        plugin_runtime,
        gateway_api::initialize(
            api,
            core.execution_service(),
            admin.services(),
            probes,
            host.worker_health(),
            host.connection_lifecycle(),
        )
    );
    host.report_startup_ready("API");

    let mut plan = store.take_worker_contributions();
    plan.extend(core.take_worker_contributions());
    plan.extend(openai.take_worker_contributions());
    plan.extend(xai.take_worker_contributions());
    plan.extend(admin.take_worker_contributions());
    plan.push(plugin_runtime.maintenance_worker(core.snapshots())?);
    or_shutdown!(
        plugin_runtime,
        host.start_workers(plan, store.worker_leader_lease())
    );
    host.report_startup_ready("Workers");
    let served = host.serve(api.router()).await;
    plugin_runtime.shutdown().await;
    served?;
    Ok(gateway_plugin_runtime::PluginCommandOutput {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        saved_accounts: 0,
    })
}

fn prepare_plugins(
    store: &gateway_store::StoreBundle,
    plugin_cache: std::path::PathBuf,
    version: &str,
    http: std::sync::Arc<gateway_host::outbound::HttpClient>,
) -> Result<std::sync::Arc<gateway_plugin_runtime::PluginRuntime>, BootstrapError> {
    Ok(std::sync::Arc::new(
        gateway_plugin_runtime::PluginRuntime::new(
            store.admin_ports().plugins(),
            store.admin_ports().plugin_state(),
            gateway_plugin_runtime::PluginRuntimeConfig {
                cache_directory: plugin_cache,
                host_version: version
                    .trim_start_matches('v')
                    .parse()
                    .map_err(|_| ConfigError::InvalidField("host.version"))?,
                package_limits: gateway_plugin_runtime::PackageLimits::default(),
                rpc_limits: gateway_plugin_runtime::RpcLimits::default(),
                restart_circuit: Default::default(),
            },
            http,
            std::sync::Arc::new(gateway_host::process::ProcessSupervisor::default()),
        )
        .with_oauth_pending(store.provider_ports().oauth_pending()),
    ))
}

/// 组合根只保留包级错误分类，不展开内部实现或敏感配置。
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
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Core(#[from] gateway_core::CoreError),
    #[error(transparent)]
    Admin(#[from] gateway_admin::model::AdminError),
    #[error(transparent)]
    Api(#[from] gateway_api::ApiError),
}
