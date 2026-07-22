//! 管理控制面的语义模型、用例与外部能力端口。
//!
//! 本 crate 不包含 HTTP wire、数据库实现或具体 Provider 实现。

use std::{fmt, path::Path, sync::Arc};

use gateway_core::{
    engine::probe::AccountProbe,
    routing::{ProviderKind, snapshot::SnapshotControl},
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

pub mod model;
pub mod ports;
mod use_case;

pub use use_case::{
    accounts::AccountsService, auth::AuthService, client_keys::ClientKeyService,
    observability::ObservabilityService, openai::OpenAiService, settings::SettingsService,
    system::SystemService, xai::XaiService,
};

use model::{AdminError, AdminErrorKind};
use ports::{
    provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind, ProviderAdminRegistry},
    store::AdminStorePorts,
    system::SystemOperations,
};
use use_case::{
    accounts::DefaultAccountsService, auth::DefaultAuthService,
    client_keys::DefaultClientKeyService, observability::DefaultObservabilityService,
    openai::DefaultOpenAiService, settings::DefaultSettingsService, system::DefaultSystemService,
    xai::DefaultXaiService,
};

const OPENAI_PROVIDER_KIND: &str = "openai";
const XAI_PROVIDER_KIND: &str = "xai";
const MINIMUM_INITIAL_PASSWORD_BYTES: usize = 12;
const WEAK_INITIAL_PASSWORDS: &[&str] = &[
    "",
    "admin",
    "123456",
    "password",
    "changeme",
    "change-me",
    "replace-me",
    "codex-proxy-rs",
];

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
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    pub session_ttl_minutes: u64,
    pub default_username: String,
    pub default_password: InitialAdminPassword,
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
            || WEAK_INITIAL_PASSWORDS.contains(&password.to_ascii_lowercase().as_str())
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
    #[error("configuration field `{0}` is invalid")]
    InvalidField(&'static str),
    #[error("admin.default_password does not meet the initial password policy")]
    WeakInitialPassword,
}

/// API 持有的管理资源能力集合。
///
/// 字段全部私有；调用方经 accessor 直接调用能力，不需要命名内部 `use_case` 模块。
#[derive(Clone)]
pub struct AdminServices {
    auth: Arc<dyn AuthService>,
    accounts: Arc<dyn AccountsService>,
    client_keys: Arc<dyn ClientKeyService>,
    observability: Arc<dyn ObservabilityService>,
    settings: Arc<dyn SettingsService>,
    system: Arc<dyn SystemService>,
    openai: Arc<dyn OpenAiService>,
    xai: Arc<dyn XaiService>,
}

impl AdminServices {
    #[must_use]
    pub fn auth(&self) -> &dyn AuthService {
        self.auth.as_ref()
    }

    #[must_use]
    pub fn accounts(&self) -> &dyn AccountsService {
        self.accounts.as_ref()
    }

    #[must_use]
    pub fn client_keys(&self) -> &dyn ClientKeyService {
        self.client_keys.as_ref()
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
    pub fn openai(&self) -> &dyn OpenAiService {
        self.openai.as_ref()
    }

    #[must_use]
    pub fn xai(&self) -> &dyn XaiService {
        self.xai.as_ref()
    }
}

/// Admin 初始化完成后的封闭能力包。
pub struct AdminBundle {
    services: AdminServices,
}

impl AdminBundle {
    #[must_use]
    pub fn services(&self) -> AdminServices {
        self.services.clone()
    }
}

/// 校验配置、建立动态 Provider 注册表并完成默认管理员幂等初始化。
///
/// # Errors
///
/// 配置非法、Provider 注册冲突/缺失或默认管理员初始化失败时返回错误。
pub async fn initialize(
    mut config: AdminConfig,
    store: AdminStorePorts,
    providers: Vec<Arc<dyn ProviderAdmin>>,
    snapshot: Arc<dyn SnapshotControl>,
    probe: Arc<dyn AccountProbe>,
    system: Arc<dyn SystemOperations>,
) -> Result<AdminBundle, AdminError> {
    config
        .resolve_and_validate(Path::new("."))
        .map_err(|error| AdminError::invalid(error.to_string()))?;
    let registry = ProviderAdminRegistry::new(providers).map_err(map_provider_registry_error)?;
    let openai = registry
        .require(&provider_kind(OPENAI_PROVIDER_KIND)?)
        .map_err(map_provider_registry_error)?;
    let xai = registry
        .require(&provider_kind(XAI_PROVIDER_KIND)?)
        .map_err(map_provider_registry_error)?;

    let auth = Arc::new(DefaultAuthService::new(
        config.default_username,
        config.session_ttl_minutes,
        store.auth(),
    ));
    auth.ensure_default_admin(config.default_password.expose())
        .await?;

    let accounts = Arc::new(DefaultAccountsService::new(
        store.accounts(),
        store.settings(),
        registry.clone(),
        snapshot.clone(),
        probe.clone(),
    ));
    let services = AdminServices {
        auth,
        accounts,
        client_keys: Arc::new(DefaultClientKeyService::new(
            store.client_keys(),
            snapshot.clone(),
        )),
        observability: Arc::new(DefaultObservabilityService::new(
            store.observability(),
            store.settings(),
            registry,
        )),
        settings: Arc::new(DefaultSettingsService::new(
            store.settings(),
            snapshot.clone(),
        )),
        system: Arc::new(DefaultSystemService::new(system)),
        openai: Arc::new(DefaultOpenAiService::new(
            openai,
            store.accounts(),
            snapshot.clone(),
        )),
        xai: Arc::new(DefaultXaiService::new(
            xai,
            store.accounts(),
            snapshot.clone(),
        )),
    };
    Ok(AdminBundle { services })
}

fn provider_kind(value: &'static str) -> Result<ProviderKind, AdminError> {
    ProviderKind::new(value).map_err(|_| AdminError::internal("Built-in Provider kind is invalid"))
}

fn map_provider_registry_error(error: ProviderAdminError) -> AdminError {
    let kind = match error.kind() {
        ProviderAdminErrorKind::Invalid | ProviderAdminErrorKind::Unsupported => {
            AdminErrorKind::Invalid
        }
        ProviderAdminErrorKind::NotFound => AdminErrorKind::NotFound,
        ProviderAdminErrorKind::Conflict => AdminErrorKind::Conflict,
        ProviderAdminErrorKind::Unavailable => AdminErrorKind::Unavailable,
        ProviderAdminErrorKind::Internal => AdminErrorKind::Internal,
    };
    AdminError::new(kind, "Provider registry initialization failed")
}
