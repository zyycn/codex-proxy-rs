//! 插件帮助只读启动；命令仅组装命令平面和必要写泵

use super::{
    BootstrapError, GatewayConfig,
    plugins::{ManagementPorts, or_shutdown, prepare_plugins, service_middleware},
    startup::{Environment, Mode, Providers},
};

/// 这里只接收插件命名空间内的参数，不转交宿主配置或其他启动 secret
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
    let config = gateway_host::load_config::<GatewayConfig>()?;
    if command.is_help() {
        let environment = Environment::initialize(config.host, config.store, Mode::Help).await?;
        help(command, environment).await
    } else {
        let environment = Environment::initialize(config.host, config.store, Mode::Command).await?;
        let providers = Providers::initialize(config.openai, &environment).await?;
        execute(command, environment, providers).await
    }
}

async fn help(
    command: PluginCommand,
    environment: Environment,
) -> Result<gateway_plugin_runtime::PluginCommandOutput, BootstrapError> {
    let runtime = prepare_plugins(
        &environment.store,
        environment.plugin_cache,
        &environment.version,
        environment.plugin_http,
    )?;
    let commands = or_shutdown!(runtime, runtime.prepare_command_line().await);
    let stdout = commands.help(command.instance_id.as_deref(), command.name.as_deref());
    commands.shutdown().await;
    runtime.shutdown().await;
    Ok(gateway_plugin_runtime::PluginCommandOutput {
        stdout: stdout?,
        stderr: String::new(),
        exit_code: 0,
        saved_accounts: 0,
    })
}

async fn execute(
    command: PluginCommand,
    environment: Environment,
    providers: Providers,
) -> Result<gateway_plugin_runtime::PluginCommandOutput, BootstrapError> {
    let Environment {
        host,
        mut store,
        plugin_cache,
        version,
        plugin_http,
        ..
    } = environment;
    let plugin_runtime = prepare_plugins(&store, plugin_cache, &version, plugin_http)?;
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
        providers.core,
        Some(plugin_runtime.clone()),
        Some(plugin_runtime.observer_registry()),
        Some(plugin_runtime.policy_registry()),
        Some(plugin_runtime.middleware_registry()),
    );
    let management = ManagementPorts::new(&store, providers.admin.clone(), core.snapshot_control());
    or_shutdown!(plugin_runtime, management.bind(&plugin_runtime));
    let core = or_shutdown!(plugin_runtime, core.activate().await);
    let nested_models = core.nested_model_execution_port();
    let affinity_lookup = core.affinity_lookup_port();
    or_shutdown!(
        plugin_runtime,
        plugin_runtime.bind_model_ports(&nested_models, &affinity_lookup)
    );
    let settings = gateway_admin::initialize_settings(
        store.admin_ports().settings(),
        core.snapshot_control(),
        providers.admin,
        std::sync::Arc::new(gateway_host::pricing::ModelsDevPricing),
    );
    let mut services = gateway_admin::service::Registry::new(service_middleware(
        core.snapshots(),
        &plugin_runtime,
    ));
    or_shutdown!(plugin_runtime, services.register_settings(&settings));
    let services = std::sync::Arc::new(services);
    or_shutdown!(plugin_runtime, plugin_runtime.bind_services(&services));
    let commands = or_shutdown!(plugin_runtime, plugin_runtime.prepare_command_line().await);
    let result = async {
        store.start_command_line_writes()?;
        Ok::<_, BootstrapError>(
            commands
                .execute_cancellable(instance_id, name, &command.arguments, &host.cancellation())
                .await?,
        )
    }
    .await;
    commands.shutdown().await;
    plugin_runtime.shutdown().await;
    store.shutdown_command_line_writes().await?;
    result
}
