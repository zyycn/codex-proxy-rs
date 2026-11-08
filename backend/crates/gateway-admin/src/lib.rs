//! 管理控制面的语义模型、用例与外部能力端口
//!
//! 本 crate 不包含 HTTP wire、数据库实现或具体 Provider 实现

use std::{fmt, path::Path, sync::Arc};

use gateway_core::{
    engine::execution::ClientKeyVerifier, engine::probe::AccountProbe, runtime::SnapshotControl,
    task::WorkerContribution,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

pub mod backup;
mod bootstrap;
pub mod freeze_recovery;
pub mod model;
pub mod ports;
pub mod public_service;
mod reset_credit_worker;
mod use_case;
pub use use_case::plugins::{PluginDistributionPorts, PluginManagementService, PluginsService};

pub use use_case::key_usage::KeyUsageService;
pub use use_case::peer_link::{PeerLinkError, PeerLinkService};

pub use use_case::{
    account_groups::AccountGroupService,
    accounts::AccountsService,
    auth::AuthService,
    backup::BackupService,
    client_distribution::ClientDistributionService,
    client_keys::ClientKeyService,
    credentials::{CredentialsService, ProviderCredentials},
    import_tasks::ImportTasksService,
    observability::{ObservabilityService, ProviderUsageBillingProjector},
    proxies::ProxiesService,
    settings::SettingsService,
    system::SystemService,
    usage_data::UsageDataService,
};

pub use bootstrap::{
    initialize, initialize_accounts, initialize_plugin_accounts, initialize_plugin_client_keys,
    initialize_plugin_proxies, initialize_plugin_resources, initialize_settings,
    initialize_usage_data, initialize_with_plugin_accounts,
};

use ports::{
    client_distribution::ClientDistributionResolver, plugin_accounts::PluginAccountAccess,
    provider::ProviderAdminRegistry, system::SystemOperations,
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

const DEFAULT_CLIENT_SESSION_TTL_MINUTES: u64 = 24 * 60;

/// 只用于首次幂等创建默认管理员的启动密码
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

/// 管理控制面的启动配置
#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct AdminConfig {
    pub session_ttl_minutes: u64,
    #[serde(default = "default_absolute_session_ttl_minutes")]
    pub session_absolute_ttl_minutes: u64,
    pub default_username: String,
    pub default_password: InitialAdminPassword,
}

fn default_absolute_session_ttl_minutes() -> u64 {
    30 * 24 * 60
}

/// Client 登录域的通用启动配置
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
    /// 校验 Client session TTL；当前配置不含相对路径
    ///
    /// # Errors
    ///
    /// 会话有效期为零或无法安全换算时返回错误
    pub fn resolve_and_validate(&mut self, _source_dir: &Path) -> Result<(), AdminConfigError> {
        if self.session_ttl_minutes == 0 || i64::try_from(self.session_ttl_minutes).is_err() {
            return Err(AdminConfigError::InvalidField("client.session_ttl_minutes"));
        }
        Ok(())
    }
}

impl AdminConfig {
    /// 校验 Admin-owned 字段；当前配置不含相对路径
    ///
    /// # Errors
    ///
    /// 用户名、会话有效期或初始密码不满足安全约束时返回错误
    pub fn resolve_and_validate(&mut self, _source_dir: &Path) -> Result<(), AdminConfigError> {
        if self.default_username.trim().is_empty()
            || self.default_username.chars().any(char::is_control)
        {
            return Err(AdminConfigError::InvalidField("admin.default_username"));
        }
        if self.session_ttl_minutes == 0 || i64::try_from(self.session_ttl_minutes).is_err() {
            return Err(AdminConfigError::InvalidField("admin.session_ttl_minutes"));
        }
        if self.session_absolute_ttl_minutes == 0
            || i64::try_from(self.session_absolute_ttl_minutes).is_err()
        {
            return Err(AdminConfigError::InvalidField(
                "admin.session_absolute_ttl_minutes",
            ));
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
            .field(
                "session_absolute_ttl_minutes",
                &self.session_absolute_ttl_minutes,
            )
            .field("default_username", &self.default_username)
            .field("default_password", &"[REDACTED]")
            .finish()
    }
}

/// Admin-owned 启动配置错误；不回显任何配置值
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdminConfigError {
    #[error("配置字段 `{0}` 不合法")]
    InvalidField(&'static str),
    #[error("admin.default_password 不符合初始密码策略")]
    WeakInitialPassword,
}

/// API 持有的管理资源能力集合
///
/// 字段全部私有；调用方经 accessor 直接调用能力，不需要命名内部 `use_case` 模块
#[derive(Clone)]
pub struct AdminServices {
    timezone: gateway_core::time::DeploymentTimeZone,
    public_services: Arc<public_service::Registry>,
    plugins: Arc<PluginsService>,
    plugin_management: Arc<PluginManagementService>,
    proxies: Arc<dyn ProxiesService>,
    auth: Arc<dyn AuthService>,
    key_usage: Arc<dyn KeyUsageService>,
    peer_link: Arc<dyn PeerLinkService>,
    accounts: Arc<dyn AccountsService>,
    account_groups: Arc<dyn AccountGroupService>,
    client_keys: Arc<dyn ClientKeyService>,
    client_distribution: Arc<dyn ClientDistributionService>,
    observability: Arc<dyn ObservabilityService>,
    settings: Arc<dyn SettingsService>,
    usage_data: Arc<dyn UsageDataService>,
    system: Arc<dyn SystemService>,
    credentials: Arc<CredentialsService>,
    // Runtime 只持有 Weak，这个强引用保障账号端口与 Admin 服务同寿命
    _plugin_accounts: Arc<dyn PluginAccountAccess>,
    backups: Arc<dyn BackupService>,
    import_tasks: Arc<dyn ImportTasksService>,
}

impl AdminServices {
    #[must_use]
    pub const fn timezone(&self) -> gateway_core::time::DeploymentTimeZone {
        self.timezone
    }

    pub fn public_services(&self) -> Arc<public_service::Registry> {
        self.public_services.clone()
    }

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

    #[must_use]
    pub fn peer_link(&self) -> &dyn PeerLinkService {
        self.peer_link.as_ref()
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
    pub fn usage_data(&self) -> Arc<dyn UsageDataService> {
        self.usage_data.clone()
    }

    #[must_use]
    pub fn observability(&self) -> &dyn ObservabilityService {
        self.observability.as_ref()
    }

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

    #[must_use]
    pub fn backups(&self) -> &dyn BackupService {
        self.backups.as_ref()
    }
}

/// Admin 初始化完成后的封闭能力包
pub struct AdminBundle {
    services: AdminServices,
    worker_contributions: Vec<WorkerContribution>,
}

impl AdminBundle {
    #[must_use]
    pub fn services(&self) -> AdminServices {
        self.services.clone()
    }

    /// 取出 Admin Worker 贡献；只能调用一次，与其它 Bundle 的贡献一并交给 Host
    pub fn take_worker_contributions(&mut self) -> Vec<WorkerContribution> {
        std::mem::take(&mut self.worker_contributions)
    }
}

pub use use_case::peer_link::upstream::{
    PeerUpstreamBundle, initialize as initialize_peer_upstream,
};

/// 组合根提供给控制面的运行能力；与配置和存储端口分别传入
pub struct AdminRuntimePorts {
    pub upstream_info: Option<Arc<dyn ports::peer::PeerUpstreamService>>,
    pub timezone: gateway_core::time::DeploymentTimeZone,
    pub service_middleware: public_service::PlanSource,
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
