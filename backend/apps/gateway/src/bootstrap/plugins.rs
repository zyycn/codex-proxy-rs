//! 插件 Runtime 的组装与回调资源持有；服务和命令模式复用同一组管理端口

use std::sync::Arc;

use gateway_admin::{
    model::AdminError,
    ports::{
        plugin_accounts::PluginAccountAccess, plugin_client_keys::PluginClientKeyAccess,
        plugin_resources::PluginResourceAccess, provider::ProviderAdminRegistry,
    },
};
use gateway_core::runtime::{RuntimeSnapshotHandle, SnapshotControl};
use gateway_host::ConfigError;
use gateway_plugin_runtime::PluginRuntime;

use super::BootstrapError;

// 必须在调用方仍持有 Core 和回调资源时异步关闭，不能先退出该作用域再清理
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

pub(super) use or_shutdown;

pub(super) struct ManagementPorts {
    // Runtime 保存 Weak；组合根持有这些 Arc，直到插件全部关闭
    pub(super) accounts: Arc<dyn PluginAccountAccess>,
    keys: Arc<dyn PluginClientKeyAccess>,
    resources: Arc<dyn PluginResourceAccess>,
}

impl ManagementPorts {
    pub(super) fn new(
        store: &gateway_store::StoreBundle,
        providers: ProviderAdminRegistry,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        let ports = store.admin_ports();
        Self {
            accounts: gateway_admin::initialize_plugin_accounts(
                providers.clone(),
                ports.accounts(),
                snapshot.clone(),
            ),
            keys: gateway_admin::initialize_plugin_client_keys(
                providers,
                ports.client_keys(),
                snapshot.clone(),
            ),
            resources: gateway_admin::initialize_plugin_resources(
                ports.plugin_resources(),
                snapshot,
            ),
        }
    }

    pub(super) fn bind(&self, runtime: &PluginRuntime) -> Result<(), AdminError> {
        runtime.bind_account_ports(&self.accounts)?;
        runtime.bind_client_key_ports(&self.keys)?;
        runtime.bind_resource_ports(&self.resources)
    }
}

pub(super) fn service_middleware(
    snapshots: RuntimeSnapshotHandle,
    runtime: &PluginRuntime,
) -> gateway_admin::service::PlanSource {
    let middleware = runtime.middleware_registry();
    Arc::new(move || {
        let snapshot = snapshots.snapshot_for_diagnostics()?;
        middleware.resolve(snapshot.extensions()?)
    })
}

pub(super) fn prepare_plugins(
    store: &gateway_store::StoreBundle,
    plugin_cache: std::path::PathBuf,
    version: &str,
    http: Arc<gateway_host::outbound::HttpClient>,
) -> Result<Arc<gateway_plugin_runtime::PluginRuntime>, BootstrapError> {
    Ok(Arc::new(
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
            Arc::new(gateway_host::process::ProcessSupervisor::default()),
        )
        .with_oauth_pending(store.provider_ports().oauth_pending()),
    ))
}
