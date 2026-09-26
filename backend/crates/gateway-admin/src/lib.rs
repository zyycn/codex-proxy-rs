//! 管理控制面的语义模型、用例与外部能力端口。
//!
//! 本 crate 不包含 HTTP wire、数据库实现或具体 Provider 实现。

use std::{fmt, path::Path, sync::Arc, time::Duration};

use gateway_core::{
    engine::execution::ClientKeyVerifier,
    engine::probe::AccountProbe,
    runtime::SnapshotControl,
    task::{
        DaemonRestartPolicy, WorkerContribution, WorkerId, WorkerKind, WorkerLeaseRequest,
        WorkerRegistration, WorkerRunnable, WorkerSchedule,
    },
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

pub mod backup;
pub mod freeze_recovery;
pub mod model;
pub mod ports;
mod use_case;
pub use use_case::plugins::{PluginDistributionPorts, PluginManagementService, PluginsService};

pub use use_case::key_usage::KeyUsageService;

pub use use_case::{
    account_groups::AccountGroupService,
    accounts::AccountsService,
    auth::AuthService,
    backup::BackupService,
    client_distribution::ClientDistributionService,
    client_keys::ClientKeyService,
    credentials::{CredentialsService, ProviderCredentials},
    import_tasks::ImportTasksService,
    observability::ObservabilityService,
    proxies::ProxiesService,
    settings::SettingsService,
    system::SystemService,
};

use model::AdminError;
use ports::{
    client_distribution::ClientDistributionResolver, plugin_accounts::PluginAccountAccess,
    plugin_client_keys::PluginClientKeyAccess, provider::ProviderAdminRegistry,
    store::AdminStorePorts, system::SystemOperations,
};
use use_case::{
    account_groups::DefaultAccountGroupService, accounts::DefaultAccountsService,
    auth::DefaultAuthService, backup::DefaultBackupService,
    client_distribution::DefaultClientDistributionService, client_keys::DefaultClientKeyService,
    observability::DefaultObservabilityService, settings::DefaultSettingsService,
    system::DefaultSystemService,
};

const MINIMUM_INITIAL_PASSWORD_BYTES: usize = 12;
const WEAK_ADMIN_PASSWORDS: &[&str] = &[
    "",
    "admin",
    "123456",
    "password",
    "changeme",
    "change-me",
    "replace-me",
    "codex-proxy-rs",
];

const BACKUP_WORKER_OWNER: &str = "backup";
const DEFAULT_CLIENT_SESSION_TTL_MINUTES: u64 = 24 * 60;

/// 只用于首次幂等创建默认管理员的启动密码。
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct InitialAdminPassword(SecretString);

impl InitialAdminPassword {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }

    fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl PartialEq for InitialAdminPassword {
    fn eq(&self, other: &Self) -> bool {
        self.expose() == other.expose()
    }
}

impl Eq for InitialAdminPassword {}

impl fmt::Debug for InitialAdminPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InitialAdminPassword([REDACTED])")
    }
}

/// 管理控制面的启动配置。
#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct AdminConfig {
    pub session_ttl_minutes: u64,
    pub default_username: String,
    pub default_password: InitialAdminPassword,
}

/// Client 登录域的通用启动配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ClientConfig {
    pub session_ttl_minutes: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            session_ttl_minutes: DEFAULT_CLIENT_SESSION_TTL_MINUTES,
        }
    }
}

impl ClientConfig {
    /// 校验 Client session TTL；当前配置不含相对路径。
    ///
    /// # Errors
    ///
    /// 会话有效期为零或无法安全换算时返回错误。
    pub fn resolve_and_validate(&mut self, _source_dir: &Path) -> Result<(), AdminConfigError> {
        if self.session_ttl_minutes == 0 || i64::try_from(self.session_ttl_minutes).is_err() {
            return Err(AdminConfigError::InvalidField("client.session_ttl_minutes"));
        }
        Ok(())
    }
}

impl AdminConfig {
    /// 校验 Admin-owned 字段；当前配置不含相对路径。
    ///
    /// # Errors
    ///
    /// 用户名、会话有效期或初始密码不满足安全约束时返回错误。
    pub fn resolve_and_validate(&mut self, _source_dir: &Path) -> Result<(), AdminConfigError> {
        if self.default_username.trim().is_empty()
            || self.default_username.chars().any(char::is_control)
        {
            return Err(AdminConfigError::InvalidField("admin.default_username"));
        }
        if self.session_ttl_minutes == 0 || i64::try_from(self.session_ttl_minutes).is_err() {
            return Err(AdminConfigError::InvalidField("admin.session_ttl_minutes"));
        }
        let password = self.default_password.expose().trim();
        if password.len() < MINIMUM_INITIAL_PASSWORD_BYTES
            || password.contains('$')
            || WEAK_ADMIN_PASSWORDS.contains(&password.to_ascii_lowercase().as_str())
        {
            return Err(AdminConfigError::WeakInitialPassword);
        }
        Ok(())
    }
}

impl fmt::Debug for AdminConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdminConfig")
            .field("session_ttl_minutes", &self.session_ttl_minutes)
            .field("default_username", &self.default_username)
            .field("default_password", &"[REDACTED]")
            .finish()
    }
}

/// Admin-owned 启动配置错误；不回显任何配置值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdminConfigError {
    #[error("配置字段 `{0}` 不合法")]
    InvalidField(&'static str),
    #[error("admin.default_password 不符合初始密码策略")]
    WeakInitialPassword,
}

/// API 持有的管理资源能力集合。
///
/// 字段全部私有；调用方经 accessor 直接调用能力，不需要命名内部 `use_case` 模块。
#[derive(Clone)]
pub struct AdminServices {
    plugins: Arc<PluginsService>,
    plugin_management: Arc<PluginManagementService>,
    proxies: Arc<dyn ProxiesService>,
    auth: Arc<dyn AuthService>,
    key_usage: Arc<dyn KeyUsageService>,
    accounts: Arc<dyn AccountsService>,
    account_groups: Arc<dyn AccountGroupService>,
    client_keys: Arc<dyn ClientKeyService>,
    client_distribution: Arc<dyn ClientDistributionService>,
    observability: Arc<dyn ObservabilityService>,
    settings: Arc<dyn SettingsService>,
    system: Arc<dyn SystemService>,
    credentials: Arc<CredentialsService>,
    plugin_accounts: Arc<dyn PluginAccountAccess>,
    backups: Arc<dyn BackupService>,
    import_tasks: Arc<dyn ImportTasksService>,
}

impl AdminServices {
    #[must_use]
    pub fn plugin_management(&self) -> &PluginManagementService {
        &self.plugin_management
    }

    #[must_use]
    pub fn plugins(&self) -> &PluginsService {
        &self.plugins
    }

    #[must_use]
    pub fn import_tasks(&self) -> &dyn ImportTasksService {
        self.import_tasks.as_ref()
    }

    #[must_use]
    pub fn key_usage(&self) -> &dyn KeyUsageService {
        self.key_usage.as_ref()
    }

    /// 取得账号服务的共享句柄；后台编排（冻结恢复 worker）需要持有 Arc。
    #[must_use]
    pub fn accounts_handle(&self) -> Arc<dyn AccountsService> {
        Arc::clone(&self.accounts)
    }

    #[must_use]
    pub fn proxies(&self) -> &dyn ProxiesService {
        self.proxies.as_ref()
    }

    #[must_use]
    pub fn auth(&self) -> &dyn AuthService {
        self.auth.as_ref()
    }

    #[must_use]
    pub fn accounts(&self) -> &dyn AccountsService {
        self.accounts.as_ref()
    }

    #[must_use]
    pub fn account_groups(&self) -> &dyn AccountGroupService {
        self.account_groups.as_ref()
    }

    #[must_use]
    pub fn client_keys(&self) -> &dyn ClientKeyService {
        self.client_keys.as_ref()
    }

    #[must_use]
    pub fn client_distribution(&self) -> &dyn ClientDistributionService {
        self.client_distribution.as_ref()
    }

    #[must_use]
    pub fn observability(&self) -> &dyn ObservabilityService {
        self.observability.as_ref()
    }

    #[must_use]
    pub fn settings(&self) -> &dyn SettingsService {
        self.settings.as_ref()
    }

    #[must_use]
    pub fn system(&self) -> &dyn SystemService {
        self.system.as_ref()
    }

    #[must_use]
    pub fn credentials(&self) -> &CredentialsService {
        self.credentials.as_ref()
    }

    /// Runtime 只持有该窄端口的 Weak；AdminBundle 保持实际生命周期。
    #[must_use]
    pub fn plugin_accounts_handle(&self) -> Arc<dyn PluginAccountAccess> {
        Arc::clone(&self.plugin_accounts)
    }

    #[must_use]
    pub fn backups(&self) -> &dyn BackupService {
        self.backups.as_ref()
    }
}

/// Admin 初始化完成后的封闭能力包。
pub struct AdminBundle {
    services: AdminServices,
    worker_contributions: Vec<WorkerContribution>,
}

impl AdminBundle {
    #[must_use]
    pub fn services(&self) -> AdminServices {
        self.services.clone()
    }

    /// 取出 Admin Worker 贡献；只能调用一次，与其它 Bundle 的贡献一并交给 Host。
    pub fn take_worker_contributions(&mut self) -> Vec<WorkerContribution> {
        std::mem::take(&mut self.worker_contributions)
    }
}

/// 组合根提供给控制面的运行能力；与配置和存储端口分别传入。
pub struct AdminRuntimePorts {
    pub plugin_preparation: Arc<dyn ports::plugins::PluginPreparation>,
    pub plugin_management: Arc<dyn ports::plugin_management::PluginManagement>,
    pub published_snapshot: gateway_core::runtime::RuntimeSnapshotHandle,
    pub plugin_distribution: Arc<dyn ports::plugins::PluginDistribution>,
    pub plugin_inspector: Arc<dyn ports::plugins::PluginPackageInspector>,
    pub pricing_source: Arc<dyn ports::pricing::PricingSource>,
    pub providers: ProviderAdminRegistry,
    pub snapshot: Arc<dyn SnapshotControl>,
    pub account_probe: Arc<dyn AccountProbe>,
    pub proxy_probe: Arc<dyn ports::proxy::ProxyProbe>,
    pub client_distribution: Arc<dyn ClientDistributionResolver>,
    pub system: Arc<dyn SystemOperations>,
    pub client_key_verifier: Arc<dyn ClientKeyVerifier>,
}

/// 校验配置、接入已组装的 Provider 注册表并完成默认管理员幂等初始化。
///
/// # Errors
///
/// 配置非法或默认管理员初始化失败时返回错误。
pub async fn initialize(
    config: AdminConfig,
    client_config: ClientConfig,
    store: AdminStorePorts,
    runtime: AdminRuntimePorts,
) -> Result<AdminBundle, AdminError> {
    initialize_inner(config, client_config, store, runtime, None).await
}

/// 使用组合根已绑定给 Runtime 的同一账号窄端口，避免为完整 Admin 重建第二实例。
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
    let registry = providers;

    let auth = Arc::new(DefaultAuthService::new(
        config.default_username,
        config.session_ttl_minutes,
        client_config.session_ttl_minutes,
        store.auth(),
        client_key_verifier.clone(),
    ));
    auth.ensure_default_admin(config.default_password.expose())
        .await?;

    let accounts = Arc::new(DefaultAccountsService::new(
        store.accounts(),
        store.account_runtime(),
        registry.clone(),
        snapshot.clone(),
        probe.clone(),
    ));
    let backup_ports = store.backup();
    let backups = Arc::new(DefaultBackupService::new(
        backup_ports.repository(),
        backup_ports.object_store(),
        store.auth(),
        snapshot.clone(),
    ));
    let backup_task = backup::task::BackupTask::new(
        backup_ports.repository(),
        backup_ports.dump(),
        backup_ports.object_store(),
    );
    let system_preflight = Arc::new(use_case::plugin_update::PluginSystemUpdatePreflight::new(
        store.plugins(),
        plugin_inspector.clone(),
    ));
    let system = Arc::new(DefaultSystemService::new(system, system_preflight));
    let key_usage = Arc::new(use_case::key_usage::DefaultKeyUsageService::new(
        auth.clone(),
        client_key_verifier,
        store.client_keys(),
        store.observability(),
        system.clone(),
    ));
    let credentials = Arc::new(CredentialsService::new(
        registry.clone(),
        store.accounts(),
        store.proxies(),
        snapshot.clone(),
    ));
    let plugin_accounts = plugin_accounts.unwrap_or_else(|| {
        initialize_plugin_accounts(registry.clone(), store.accounts(), snapshot.clone())
    });
    let import_tasks = use_case::import_tasks::DefaultImportTasksService::new(credentials.clone());
    let import_task = use_case::import_tasks::ImportTaskWorker(import_tasks.clone());
    let services = AdminServices {
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
            registry.clone(),
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
            registry.clone(),
        )),
        client_distribution: Arc::new(DefaultClientDistributionService::new(client_distribution)),
        observability: Arc::new(DefaultObservabilityService::new(
            store.observability(),
            store.accounts(),
            store.settings(),
            registry.clone(),
        )),
        settings: Arc::new(DefaultSettingsService::new(
            store.settings(),
            snapshot.clone(),
            registry,
            pricing_source,
        )),
        system,
        credentials,
        plugin_accounts,
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
    let mut worker_contributions = backup_worker_contribution(backup_task)?;
    let id = WorkerId::try_new(WorkerKind::AccountImport, "admin")
        .map_err(|_| AdminError::internal("导入 Worker ID 不合法"))?;
    let restart = DaemonRestartPolicy::try_new(Duration::from_secs(1), Duration::from_secs(60))
        .map_err(|_| AdminError::internal("导入 Worker 重启策略不合法"))?;
    let registration = WorkerRegistration::try_new(
        id,
        WorkerRunnable::Daemon {
            restart,
            task: Box::new(import_task),
        },
    )
    .map_err(|_| AdminError::internal("导入 Worker 注册信息不合法"))?;
    worker_contributions.push(WorkerContribution::Registration(registration));
    worker_contributions.extend(freeze_recovery_worker_contribution(freeze_recovery)?);
    Ok(AdminBundle {
        services,
        worker_contributions,
    })
}

/// CLI 只组合账号用例，不初始化管理员、管理服务或后台任务；写入仍复用同一事务与审计。
pub fn initialize_plugin_accounts(
    providers: ports::provider::ProviderAdminRegistry,
    accounts: Arc<dyn ports::store::AccountStore>,
    snapshot: Arc<dyn gateway_core::runtime::SnapshotControl>,
) -> Arc<dyn PluginAccountAccess> {
    Arc::new(use_case::plugin_accounts::DefaultPluginAccountAccess::new(
        providers, accounts, snapshot,
    ))
}

/// 为 Runtime 创建只暴露非秘密 Client Key 目录的窄端口。
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

/// 为 Runtime 组合实例自有资源写入；权限和归属在同一存储事务复核。
#[must_use]
pub fn initialize_plugin_resources(
    store: Arc<dyn ports::plugin_resources::PluginResourceStore>,
    snapshot: Arc<dyn SnapshotControl>,
) -> Arc<dyn ports::plugin_resources::PluginResourceAccess> {
    Arc::new(use_case::plugin_resources::DefaultPluginResourceAccess { store, snapshot })
}

/// Backup Worker 注册：单个可取消 Daemon，owner 固定为 `backup`。
fn backup_worker_contribution(
    task: backup::task::BackupTask,
) -> Result<Vec<WorkerContribution>, AdminError> {
    let id = WorkerId::try_new(WorkerKind::Backup, BACKUP_WORKER_OWNER)
        .map_err(|_| AdminError::internal("备份 Worker ID 不合法"))?;
    let restart = DaemonRestartPolicy::try_new(Duration::from_secs(1), Duration::from_secs(60))
        .map_err(|_| AdminError::internal("备份 Worker 重启策略不合法"))?;
    let registration = WorkerRegistration::try_new(
        id,
        WorkerRunnable::Daemon {
            restart,
            task: Box::new(task),
        },
    )
    .map_err(|_| AdminError::internal("备份 Worker 注册信息不合法"))?;
    Ok(vec![WorkerContribution::Registration(registration)])
}

/// 冻结恢复 Worker 注册：按固定周期扫描活跃冻结，owner 固定。
fn freeze_recovery_worker_contribution(
    task: freeze_recovery::FreezeRecoveryTask,
) -> Result<Vec<WorkerContribution>, AdminError> {
    let id = WorkerId::try_new(
        WorkerKind::AccountFreezeRecovery,
        freeze_recovery::FREEZE_RECOVERY_WORKER_OWNER,
    )
    .map_err(|_| AdminError::internal("冻结恢复 Worker ID 不合法"))?;
    let schedule = WorkerSchedule::try_new(
        freeze_recovery::FREEZE_RECOVERY_INTERVAL,
        freeze_recovery::WORKER_INITIAL_BACKOFF,
        freeze_recovery::WORKER_MAXIMUM_BACKOFF,
        freeze_recovery::WORKER_LEASE_TTL,
        freeze_recovery::WORKER_LEASE_RENEWAL,
    )
    .map_err(|_| AdminError::internal("冻结恢复 Worker 调度配置不合法"))?;
    let lease = WorkerLeaseRequest::try_new(id.clone(), freeze_recovery::WORKER_LEASE_TTL)
        .map_err(|_| AdminError::internal("冻结恢复 Worker 租约配置不合法"))?;
    let registration = WorkerRegistration::try_new(
        id,
        WorkerRunnable::Scheduled {
            schedule,
            lease: Some(lease),
            task: Box::new(task),
        },
    )
    .map_err(|_| AdminError::internal("冻结恢复 Worker 注册信息不合法"))?;
    Ok(vec![WorkerContribution::Registration(registration)])
}
