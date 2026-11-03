//! 管理服务与后台任务的启动组装，窄端口供服务器和 CLI 复用

use std::{path::Path, sync::Arc};

use gateway_core::{engine::probe::AccountProbe, runtime::SnapshotControl};

use crate::{
    AccountsService, AdminBundle, AdminConfig, AdminRuntimePorts, AdminServices, ClientConfig,
    ClientKeyService, CredentialsService, PluginDistributionPorts, PluginManagementService,
    PluginsService, SettingsService, UsageDataService, backup, freeze_recovery,
    model::AdminError,
    ports::{
        self, plugin_accounts::PluginAccountAccess, plugin_client_keys::PluginClientKeyAccess,
        provider::ProviderAdminRegistry, store::AdminStorePorts,
    },
    public_service, reset_credit_worker, use_case,
    use_case::{
        account_groups::DefaultAccountGroupService,
        accounts::DefaultAccountsService,
        auth::{AuthService, DefaultAuthService},
        backup::DefaultBackupService,
        client_distribution::DefaultClientDistributionService,
        client_keys::DefaultClientKeyService,
        observability::DefaultObservabilityService,
        settings::DefaultSettingsService,
        system::DefaultSystemService,
    },
};

/// 校验配置、接入已组装的 Provider 注册表并完成默认管理员幂等初始化
///
/// # Errors
///
/// 配置非法或默认管理员初始化失败时返回错误
pub async fn initialize(
    config: AdminConfig,
    client_config: ClientConfig,
    store: AdminStorePorts,
    runtime: AdminRuntimePorts,
) -> Result<AdminBundle, AdminError> {
    initialize_inner(config, client_config, store, runtime, None).await
}

/// 使用组合根已绑定给 Runtime 的同一账号窄端口，避免为完整 Admin 重建第二实例
pub async fn initialize_with_plugin_accounts(
    config: AdminConfig,
    client_config: ClientConfig,
    store: AdminStorePorts,
    runtime: AdminRuntimePorts,
    plugin_accounts: Arc<dyn PluginAccountAccess>,
) -> Result<AdminBundle, AdminError> {
    initialize_inner(config, client_config, store, runtime, Some(plugin_accounts)).await
}

async fn initialize_inner(
    mut config: AdminConfig,
    mut client_config: ClientConfig,
    store: AdminStorePorts,
    runtime: AdminRuntimePorts,
    plugin_accounts: Option<Arc<dyn PluginAccountAccess>>,
) -> Result<AdminBundle, AdminError> {
    let AdminRuntimePorts {
        upstream_info,
        timezone,
        service_middleware,
        plugin_preparation,
        plugin_management,
        published_snapshot,
        plugin_distribution,
        plugin_inspector,
        pricing_source,
        providers,
        snapshot,
        account_probe: probe,
        proxy_probe,
        client_distribution,
        system,
        client_key_verifier,
    } = runtime;
    config
        .resolve_and_validate(Path::new("."))
        .map_err(|error| AdminError::invalid(error.to_string()))?;
    client_config
        .resolve_and_validate(Path::new("."))
        .map_err(|error| AdminError::invalid(error.to_string()))?;

    let auth = Arc::new(DefaultAuthService::new(
        &config,
        client_config.session_ttl_minutes,
        store.auth(),
        client_key_verifier.clone(),
    ));
    auth.ensure_default_admin(config.default_password.expose())
        .await?;

    let accounts = initialize_accounts(
        store.accounts(),
        store.account_runtime(),
        store.settings(),
        timezone,
        providers.clone(),
        snapshot.clone(),
        published_snapshot.clone(),
        probe.clone(),
        upstream_info.clone(),
    );
    let backup_ports = store.backup();
    let backups = Arc::new(DefaultBackupService::new(
        backup_ports.repository(),
        backup_ports.object_store(),
        store.auth(),
        snapshot.clone(),
        timezone,
    ));
    let backup_task = backup::task::BackupTask::new(
        backup_ports.repository(),
        backup_ports.dump(),
        backup_ports.object_store(),
    )
    .with_timezone(timezone);
    let system_preflight = Arc::new(
        use_case::system_preflight::PluginSystemUpdatePreflight::new(
            store.plugins(),
            plugin_inspector.clone(),
        ),
    );
    let system = Arc::new(DefaultSystemService::new(system, system_preflight));
    let observations: Arc<dyn ports::store::ObservabilityStore> = Arc::new(
        use_case::observability::CachedObservabilityStore::new(store.observability()),
    );
    let key_usage = Arc::new(use_case::key_usage::DefaultKeyUsageService::new(
        auth.clone(),
        client_key_verifier,
        store.client_keys(),
        observations.clone(),
        system.clone(),
        timezone,
    ));
    let credentials = Arc::new(CredentialsService::new(
        providers.clone(),
        store.accounts(),
        store.proxies(),
        snapshot.clone(),
        upstream_info.clone(),
    ));
    let plugin_accounts = plugin_accounts.unwrap_or_else(|| {
        initialize_plugin_accounts(
            providers.clone(),
            store.accounts(),
            snapshot.clone(),
            upstream_info.clone(),
        )
    });
    let import_tasks = use_case::import_tasks::DefaultImportTasksService::new(credentials.clone());
    let import_task = use_case::import_tasks::ImportTaskWorker(import_tasks.clone());
    let settings = initialize_settings(
        store.settings(),
        snapshot.clone(),
        providers.clone(),
        pricing_source,
    );
    let usage_data = initialize_usage_data(store.usage_data(), store.settings(), providers.clone());
    let mut public_services = public_service::Registry::new(service_middleware);
    public_services.register_settings(&settings)?;
    public_services.register_accounts(&accounts)?;
    public_services.register_usage_data(&usage_data)?;
    let services = AdminServices {
        peer_link: Arc::new(use_case::peer_link::DefaultPeerLinkService::new(
            store.client_keys(),
            store.accounts(),
            store.account_runtime(),
            providers.clone(),
        )),
        timezone,
        public_services: Arc::new(public_services),
        plugin_management: Arc::new(PluginManagementService::new(
            plugin_management,
            store.plugins(),
            published_snapshot.clone(),
        )),
        plugins: Arc::new(PluginsService::new(
            store.plugins(),
            plugin_inspector,
            PluginDistributionPorts::new(plugin_distribution, store.proxies()),
            snapshot.clone(),
            plugin_preparation,
            published_snapshot,
            store.plugin_state(),
        )),
        key_usage,
        proxies: Arc::new(use_case::proxies::DefaultProxiesService::new(
            store.proxies(),
            proxy_probe,
            snapshot.clone(),
            providers.clone(),
            upstream_info.clone(),
        )),
        auth,
        accounts: accounts.clone(),
        account_groups: Arc::new(DefaultAccountGroupService::new(
            store.account_groups(),
            store.account_runtime(),
            snapshot.clone(),
        )),
        client_keys: Arc::new(DefaultClientKeyService::new(
            store.client_keys(),
            snapshot.clone(),
            providers.clone(),
        )),
        client_distribution: Arc::new(DefaultClientDistributionService::new(client_distribution)),
        observability: Arc::new(DefaultObservabilityService::new(
            observations.clone(),
            store.accounts(),
            store.settings(),
            providers.clone(),
            timezone,
            system.clone(),
            upstream_info,
        )),
        settings,
        usage_data,
        system,
        credentials,
        _plugin_accounts: plugin_accounts,
        import_tasks,
        backups,
    };
    let freeze_recovery =
        freeze_recovery::FreezeRecoveryTask::new(freeze_recovery::FreezeRecoveryDeps {
            accounts: Arc::clone(&accounts) as Arc<dyn AccountsService>,
            store: store.accounts(),
            runtime: store.account_runtime(),
            settings: store.settings(),
        });
    let worker_contributions = vec![
        backup_task.into_worker()?,
        import_task.into_worker()?,
        freeze_recovery.into_worker()?,
        reset_credit_worker::ResetCreditAutomationTask {
            accounts,
            store: store.accounts(),
            settings: store.settings(),
        }
        .into_worker()?,
    ];
    Ok(AdminBundle {
        services,
        worker_contributions,
    })
}

/// CLI 只组合账号用例，不初始化管理员、管理服务或后台任务；写入仍复用同一事务与审计
pub fn initialize_plugin_accounts(
    providers: ports::provider::ProviderAdminRegistry,
    accounts: Arc<dyn ports::store::AccountStore>,
    snapshot: Arc<dyn gateway_core::runtime::SnapshotControl>,
    upstream_info: Option<Arc<dyn ports::peer::PeerUpstreamService>>,
) -> Arc<dyn PluginAccountAccess> {
    Arc::new(use_case::plugin_accounts::DefaultPluginAccountAccess::new(
        providers,
        accounts,
        snapshot,
        upstream_info,
    ))
}

/// 为 Runtime 创建非秘密 Client Key 目录与预算重置的窄端口
#[must_use]
pub fn initialize_plugin_client_keys(
    providers: ports::provider::ProviderAdminRegistry,
    store: Arc<dyn ports::store::ClientKeyStore>,
    snapshot: Arc<dyn SnapshotControl>,
) -> Arc<dyn PluginClientKeyAccess> {
    let service: Arc<dyn ClientKeyService> =
        Arc::new(DefaultClientKeyService::new(store, snapshot, providers));
    Arc::new(use_case::plugin_client_keys::DefaultPluginClientKeyAccess::new(service))
}

/// 为 Runtime 组合实例自有资源写入；权限和归属在同一存储事务复核
#[must_use]
pub fn initialize_plugin_resources(
    store: Arc<dyn ports::plugin_resources::PluginResourceStore>,
    snapshot: Arc<dyn SnapshotControl>,
) -> Arc<dyn ports::plugin_resources::PluginResourceAccess> {
    Arc::new(use_case::plugin_resources::DefaultPluginResourceAccess { store, snapshot })
}

/// 服务端与 CLI 复用账号用例，不随命令启动后台调度
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "账号存储、发布、探测与互联分别通过独立端口注入"
)]
pub fn initialize_accounts(
    accounts: Arc<dyn ports::store::AccountStore>,
    account_runtime: Arc<dyn ports::store::AccountRuntimeStore>,
    settings: Arc<dyn ports::store::SettingsStore>,
    timezone: gateway_core::time::DeploymentTimeZone,
    providers: ProviderAdminRegistry,
    snapshot: Arc<dyn SnapshotControl>,
    published_snapshot: gateway_core::runtime::RuntimeSnapshotHandle,
    probe: Arc<dyn AccountProbe>,
    upstream_info: Option<Arc<dyn ports::peer::PeerUpstreamService>>,
) -> Arc<dyn AccountsService> {
    Arc::new(DefaultAccountsService::new(
        accounts,
        account_runtime,
        settings,
        timezone,
        providers,
        snapshot,
        published_snapshot,
        probe,
        upstream_info,
    ))
}

/// 设置服务不依赖 Web 管理会话，CLI 与服务器通过同一用例执行事务和发布
pub fn initialize_settings(
    store: Arc<dyn ports::store::SettingsStore>,
    snapshot: Arc<dyn SnapshotControl>,
    providers: ProviderAdminRegistry,
    pricing_source: Arc<dyn ports::pricing::PricingSource>,
) -> Arc<dyn SettingsService> {
    Arc::new(DefaultSettingsService::new(
        store,
        snapshot,
        providers,
        pricing_source,
    ))
}

/// HTTP、插件及 CLI 共用的用量领域用例，宿主提供已验证的调用主体
#[must_use]
pub fn initialize_usage_data(
    store: Arc<dyn ports::usage_data::UsageDataStore>,
    settings: Arc<dyn ports::store::SettingsStore>,
    providers: ProviderAdminRegistry,
) -> Arc<dyn UsageDataService> {
    Arc::new(use_case::usage_data::DefaultUsageDataService::new(
        store, settings, providers,
    ))
}
