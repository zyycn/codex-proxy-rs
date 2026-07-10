//! 运行时服务集合。

use std::{env, error::Error, sync::Arc, time::Duration};

use axum::Router;

use crate::{
    accounts::{
        cookies::PgCookieStore,
        manage::{AccountManageService, AccountManageServiceParts},
        pool::{AccountPoolOptions, AccountPoolStaticSettings, RuntimeAccountPoolService},
        refresh::{
            RedisRefreshLeaseStore, RefreshPolicy, RuntimeRefreshPolicy, RuntimeTokenRefreshService,
        },
        store::{AccountStore, PgAccountStore},
    },
    auth::{
        service::SessionService,
        store::{PgAdminUserStore, RedisAdminSessionStore},
    },
    bootstrap::config::AppConfig,
    dispatch::{
        affinity::{RedisSessionAffinityStore, RuntimeSessionAffinityService},
        service::{ResponseDispatchService, ResponseDispatchServiceParts},
    },
    infra::{
        database::connect,
        logging::{init_tracing, LogGuard, RotationConfig},
        paths::{ensure_data_dir, load_or_create_installation_id},
        redis::RedisConnection,
    },
    keys::{manage::KeyManageService, service::KeyVerifier, store::PgClientKeyStore},
    models::{
        service::{ModelService, ModelServiceError},
        store::{ModelSnapshotStore, RedisModelSnapshotStore},
        types::ModelConfig,
    },
    settings::{service::RuntimeSettingsService, SettingsSnapshot},
    telemetry::{
        account_usage::query::AccountUsageQueryService,
        account_usage::store::{AccountUsageStore, PgAccountUsageStore},
        buckets::store::PgRequestBucketStore,
        ops::query::OpsQueryService,
        ops::store::PgOpsErrorLogStore,
        recorder::Recorder,
        usage::query::UsageQueryService,
        usage::store::{PgUsageRecordStore, DEFAULT_USAGE_RECORD_CAPTURE_BODY},
    },
    upstream::openai::{
        fingerprint::{Fingerprint, PgFingerprintStore, RuntimeFingerprint},
        token_client::{default_openai_token_client, OpenAiTokenClient, TokenClientConfig},
        transport::{
            build_reqwest_client, tls::CustomCaError, CodexBackendClient, CodexModelCatalogClient,
            CodexWebSocketPool, CodexWebSocketPoolConfig,
        },
    },
};

const RESTART_DELAY_ENV: &str = "CPR_RESTART_DELAY_MS";

/// 运行时热路径内存状态初始化错误。
#[derive(Debug, thiserror::Error)]
pub enum RuntimeServiceInitializationError {
    #[error("failed to initialize model catalog runtime cache: {0}")]
    Models(#[from] ModelServiceError),
}

// ============================================================================
// BackgroundTaskStores
// ============================================================================

/// 后台任务需要的具体存储适配器集合。
#[derive(Clone)]
pub struct BackgroundTaskStores {
    pub redis: RedisConnection,
    pub accounts: PgAccountStore,
    pub admin_users: PgAdminUserStore,
    pub admin_sessions: RedisAdminSessionStore,
    pub cookies: PgCookieStore,
    pub fingerprints: PgFingerprintStore,
    pub session_affinity: RedisSessionAffinityStore,
    pub refresh_leases: RedisRefreshLeaseStore,
    pub model_snapshots: RedisModelSnapshotStore,
    pub client_keys: PgClientKeyStore,
    pub usage_records: PgUsageRecordStore,
    pub ops_errors: PgOpsErrorLogStore,
    pub account_usage: PgAccountUsageStore,
    pub request_buckets: PgRequestBucketStore,
}

// ============================================================================
// Services struct
// ============================================================================

/// 运行时服务集合。
#[derive(Clone)]
pub struct Services {
    pub database: sqlx::PgPool,
    pub redis: RedisConnection,
    pub models: Arc<ModelService>,
    pub accounts: Arc<dyn AccountStore>,
    pub client_keys: Arc<KeyVerifier>,
    pub admin_client_keys: Arc<KeyManageService>,
    pub admin_sessions: Arc<SessionService>,
    pub settings: Arc<RuntimeSettingsService>,
    pub refresh_policy: RuntimeRefreshPolicy,
    pub admin_accounts: Arc<AccountManageService>,
    pub usage_records: Arc<UsageQueryService>,
    pub ops_errors: Arc<OpsQueryService>,
    pub usage: Arc<AccountUsageQueryService>,
    pub account_pool: Arc<RuntimeAccountPoolService>,
    pub token_refresh: Arc<RuntimeTokenRefreshService<OpenAiTokenClient>>,
    pub responses: Arc<ResponseDispatchService>,
    pub session_affinity: Arc<RuntimeSessionAffinityService>,
    pub codex: Arc<CodexBackendClient>,
    pub websocket_pool: Option<Arc<CodexWebSocketPool>>,
    pub fingerprint: RuntimeFingerprint,
    pub installation_id: Option<String>,
    pub background_tasks: BackgroundTaskStores,
    pub(crate) account_pool_static: AccountPoolStaticSettings,
}

/// 使用记录运行选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageRecordOptions {
    pub enabled: bool,
    pub capture_body: bool,
}

impl UsageRecordOptions {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            enabled: config.logging.enabled,
            capture_body: DEFAULT_USAGE_RECORD_CAPTURE_BODY,
        }
    }
}

impl Services {
    pub fn new(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: RuntimeFingerprint,
    ) -> Self {
        Self::try_new(config, stores, fingerprint)
            .expect("failed to build runtime services with configured TLS transport")
    }

    pub fn try_new(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: RuntimeFingerprint,
    ) -> Result<Self, CustomCaError> {
        Self::try_with_installation_id(config, stores, fingerprint, None)
    }

    pub(crate) fn try_with_installation_id(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: RuntimeFingerprint,
        installation_id: Option<String>,
    ) -> Result<Self, CustomCaError> {
        Self::try_with_installation_id_and_usage_record_options(
            config,
            stores,
            fingerprint,
            installation_id,
            UsageRecordOptions::from_config(config),
        )
    }

    pub fn try_with_installation_id_and_usage_record_options(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: RuntimeFingerprint,
        installation_id: Option<String>,
        usage_record_options: UsageRecordOptions,
    ) -> Result<Self, CustomCaError> {
        let installation_id = installation_id.filter(|id| !id.trim().is_empty());
        let database = stores.accounts.pool().clone();
        let redis = stores.redis.clone();
        let account_store = Arc::new(stores.accounts.clone()) as Arc<dyn AccountStore>;
        let account_usage_store_trait =
            Arc::new(stores.account_usage.clone()) as Arc<dyn AccountUsageStore>;
        let websocket_pool = config.ws_pool.enabled.then(|| {
            Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
                enabled: true,
                max_age: std::time::Duration::from_millis(config.ws_pool.max_age_ms),
                max_per_account: config.ws_pool.max_per_account,
                first_token_timeout: (config.ws_pool.first_token_timeout_ms > 0).then(|| {
                    std::time::Duration::from_millis(config.ws_pool.first_token_timeout_ms)
                }),
                ..CodexWebSocketPoolConfig::default()
            }))
        });
        let codex = {
            let websocket_first_token_timeout = (config.ws_pool.first_token_timeout_ms > 0)
                .then(|| std::time::Duration::from_millis(config.ws_pool.first_token_timeout_ms));
            let client = CodexBackendClient::new(
                build_reqwest_client(config.tls.force_http11)?,
                config.api.base_url.clone(),
                fingerprint.clone(),
            )
            .with_websocket_first_token_timeout(websocket_first_token_timeout);
            if let Some(pool) = &websocket_pool {
                Arc::new(client.with_websocket_pool(pool.clone()))
            } else {
                Arc::new(client)
            }
        };
        let usage_records = Arc::new(UsageQueryService::new(stores.usage_records.clone()));
        let ops_errors = Arc::new(OpsQueryService::new(stores.ops_errors.clone()));
        let recorder = Arc::new(Recorder::new(
            stores.usage_records.clone(),
            stores.ops_errors.clone(),
            usage_record_options.enabled,
            usage_record_options.capture_body,
        ));
        let settings_snapshot = settings_snapshot_from_config(config);
        let account_pool_static = AccountPoolStaticSettings {
            skip_quota_limited: config.quota.skip_exhausted,
            tier_priority: config.auth.tier_priority.clone(),
        };
        let account_pool = Arc::new(RuntimeAccountPoolService::new(
            account_store.clone(),
            account_usage_store_trait,
            account_pool_static.pool_options(&settings_snapshot),
            settings_snapshot.request_interval_ms,
        ));
        let refresh_policy = RuntimeRefreshPolicy::new(RefreshPolicy {
            refresh_margin_seconds: config.auth.refresh_margin_seconds,
            refresh_concurrency: config.auth.refresh_concurrency,
        });
        let settings = Arc::new(RuntimeSettingsService::new(
            settings_snapshot,
            stores.accounts.pool().clone(),
        ));
        let admin_sessions = Arc::new(SessionService::new(
            stores.admin_users.clone(),
            stores.admin_sessions.clone(),
            config.admin.default_username.clone(),
            config.admin.session_ttl_minutes,
        ));
        let admin_client_keys = Arc::new(KeyManageService::new(stores.client_keys.clone()));
        let client_keys = Arc::new(KeyVerifier::new(stores.client_keys.clone()));
        let token_client = default_openai_token_client(token_client_config(config));
        let token_refresh = Arc::new(
            RuntimeTokenRefreshService::new(
                stores.accounts.clone(),
                refresh_policy.clone(),
                token_client.clone(),
            )
            .with_refresh_lease_store(stores.refresh_leases.clone()),
        );
        let upstream_client: Arc<dyn CodexModelCatalogClient> = codex.clone();
        let store: Arc<dyn ModelSnapshotStore> = Arc::new(stores.model_snapshots.clone());
        let models = Arc::new(ModelService::new(
            ModelConfig {
                model_aliases: config.model_aliases.clone(),
            },
            Some(store),
            Some(upstream_client),
        ));
        let admin_accounts = Arc::new(AccountManageService::new(AccountManageServiceParts {
            store: stores.accounts.clone(),
            cookies: stores.cookies.clone(),
            codex: codex.clone(),
            models: models.clone(),
            account_pool: account_pool.clone(),
            token_refresher: Arc::new(token_client),
            refresh_leases: stores.refresh_leases.clone(),
            oauth: crate::accounts::manage::oauth::AccountOAuthService::new(
                reqwest::Client::new(),
                config.auth.oauth_client_id.clone(),
                config.auth.oauth_token_endpoint.clone(),
            ),
            refresh_policy: refresh_policy.clone(),
            installation_id: installation_id.clone(),
        }));
        let usage = Arc::new(AccountUsageQueryService::new(stores.account_usage.clone()));
        let session_affinity = Arc::new(RuntimeSessionAffinityService::new(
            stores.session_affinity.clone(),
        ));

        let cloudflare_recovery =
            crate::dispatch::recovery::cloudflare::CloudflareRecovery::new(stores.cookies.clone());
        let responses = Arc::new(ResponseDispatchService::new(ResponseDispatchServiceParts {
            account_pool: account_pool.clone(),
            models: models.clone(),
            codex: codex.clone(),
            session_affinity: session_affinity.clone(),
            recorder,
            installation_id: installation_id.clone(),
            cloudflare: cloudflare_recovery,
        }));

        Ok(Self {
            database,
            redis,
            models,
            accounts: account_store,
            client_keys,
            admin_client_keys,
            admin_sessions,
            settings,
            refresh_policy,
            admin_accounts,
            usage_records,
            ops_errors,
            usage,
            account_pool,
            token_refresh,
            responses,
            session_affinity,
            codex,
            websocket_pool,
            fingerprint,
            installation_id,
            background_tasks: stores,
            account_pool_static,
        })
    }

    /// 初始化请求热路径依赖的内存状态。
    pub async fn initialize_hot_path_state(&self) -> Result<(), RuntimeServiceInitializationError> {
        self.models.reload_from_store().await?;
        Ok(())
    }
}

impl From<&Services> for crate::api::router::ApiServices {
    fn from(services: &Services) -> Self {
        Self {
            health_probe: Arc::new(crate::bootstrap::state::RuntimeHealthProbe::new(
                services.database.clone(),
                services.redis.clone(),
            )),
            models: services.models.clone(),
            accounts: services.accounts.clone(),
            client_keys: services.client_keys.clone(),
            admin_client_keys: services.admin_client_keys.clone(),
            admin_sessions: services.admin_sessions.clone(),
            settings: services.settings.clone(),
            admin_accounts: services.admin_accounts.clone(),
            usage_records: services.usage_records.clone(),
            ops_errors: services.ops_errors.clone(),
            usage: services.usage.clone(),
            account_pool: services.account_pool.clone(),
            token_refresh: services.token_refresh.clone(),
            responses: services.responses.clone(),
            session_affinity: services.session_affinity.clone(),
            fingerprint: services.fingerprint.clone(),
            process_control: Arc::new(crate::bootstrap::shutdown::RuntimeProcessControl),
        }
    }
}

impl From<&Services> for crate::api::router::AppState {
    fn from(services: &Services) -> Self {
        Self {
            services: crate::api::router::ApiServices::from(services),
        }
    }
}

impl From<Services> for crate::api::router::AppState {
    fn from(services: Services) -> Self {
        Self::from(&services)
    }
}

fn token_client_config(config: &AppConfig) -> TokenClientConfig {
    TokenClientConfig {
        client_id: config.auth.oauth_client_id.clone(),
        token_endpoint: config.auth.oauth_token_endpoint.clone(),
    }
}

pub fn settings_snapshot_from_config(config: &AppConfig) -> SettingsSnapshot {
    SettingsSnapshot {
        model_aliases: config.model_aliases.clone(),
        refresh_margin_seconds: config.auth.refresh_margin_seconds,
        refresh_concurrency: config.auth.refresh_concurrency,
        max_concurrent_per_account: config.auth.max_concurrent_per_account,
        request_interval_ms: config.auth.request_interval_ms,
        rotation_strategy: config.auth.rotation_strategy.clone(),
    }
}

pub fn apply_settings_to_config(config: &mut AppConfig, settings: &SettingsSnapshot) {
    config.model_aliases = settings.model_aliases.clone();
    config.auth.refresh_margin_seconds = settings.refresh_margin_seconds;
    config.auth.refresh_concurrency = settings.refresh_concurrency;
    config.auth.max_concurrent_per_account = settings.max_concurrent_per_account;
    config.auth.request_interval_ms = settings.request_interval_ms;
    config.auth.rotation_strategy = settings.rotation_strategy.clone();
}

pub fn fingerprint_from_config(
    config: &crate::bootstrap::config::FingerprintConfig,
) -> Fingerprint {
    Fingerprint {
        originator: config.originator.clone(),
        app_version: config.app_version.clone(),
        build_number: config.build_number.clone(),
        platform: config.platform.clone(),
        arch: config.arch.clone(),
        chromium_version: config.chromium_version.clone(),
        user_agent_template: config.user_agent_template.clone(),
        default_headers: config
            .default_headers
            .iter()
            .map(|header| (header.name.clone(), header.value.clone()))
            .collect(),
        header_order: config.header_order.clone(),
        updated_at: None,
    }
}

pub fn account_pool_options_from_config(config: &AppConfig) -> AccountPoolOptions {
    AccountPoolStaticSettings {
        skip_quota_limited: config.quota.skip_exhausted,
        tier_priority: config.auth.tier_priority.clone(),
    }
    .pool_options(&settings_snapshot_from_config(config))
}

pub async fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    wait_for_scheduled_restart().await;

    let config = AppConfig::load()?;
    let host = config.server.host.clone();
    let port = config.server.port;
    let log_guard = init_logging(&config)?;
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    let (router, task_coordinator) = build_router(config).await?;

    let serve_result = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(crate::bootstrap::shutdown::shutdown_signal())
    .await;

    task_coordinator.shutdown().await;
    serve_result?;

    if let Some(executable_path) = crate::bootstrap::shutdown::restart_executable_path() {
        tracing::info!(
            executable = %executable_path.display(),
            "正在以更新后的二进制替换当前进程"
        );
        drop(log_guard);
        exec_replacement_process(executable_path)?;
    }

    Ok(())
}

async fn build_router(
    mut config: AppConfig,
) -> Result<
    (
        Router,
        crate::bootstrap::tasks::coordinator::TaskCoordinator,
    ),
    Box<dyn Error + Send + Sync>,
> {
    let pool = connect(&config.database.url).await?;
    let redis = RedisConnection::connect(&config.redis.url, "cpr").await?;
    let settings =
        RuntimeSettingsService::load_or_initialize(settings_snapshot_from_config(&config), &pool)
            .await?;
    apply_settings_to_config(&mut config, &settings);

    let fingerprint_store = PgFingerprintStore::new(pool.clone());
    let stores = BackgroundTaskStores {
        redis: redis.clone(),
        accounts: PgAccountStore::new(pool.clone()),
        admin_users: PgAdminUserStore::new(pool.clone()),
        admin_sessions: RedisAdminSessionStore::new(redis.clone()),
        cookies: PgCookieStore::new(pool.clone()),
        fingerprints: fingerprint_store.clone(),
        session_affinity: RedisSessionAffinityStore::new(redis.clone()),
        refresh_leases: RedisRefreshLeaseStore::new(redis.clone()),
        model_snapshots: RedisModelSnapshotStore::new(redis.clone()),
        client_keys: PgClientKeyStore::new(pool.clone()),
        usage_records: PgUsageRecordStore::new(pool.clone()),
        ops_errors: PgOpsErrorLogStore::new(pool.clone()),
        account_usage: PgAccountUsageStore::new(pool.clone()),
        request_buckets: PgRequestBucketStore::new(pool),
    };

    config.admin.validate_default_password()?;
    let default_admin_password = config.admin.default_password.clone();
    let data_dir = ensure_data_dir()?;
    let installation_id = load_or_create_installation_id(Some(&data_dir))?;
    let default_fingerprint = fingerprint_from_config(&config.fingerprint);
    let runtime_fingerprint = fingerprint_store
        .ensure_current_seed(&default_fingerprint)
        .await?;
    let runtime_fingerprint = RuntimeFingerprint::new(runtime_fingerprint);
    let runtime_config = crate::bootstrap::state::RuntimeConfig::from(config.clone());
    let services = Services::try_with_installation_id(
        &config,
        stores,
        runtime_fingerprint,
        Some(installation_id),
    )?;
    services.initialize_hot_path_state().await?;

    let created_default_admin = services
        .admin_sessions
        .ensure_default_admin(&default_admin_password)
        .await?;
    tracing::info!(
        created = created_default_admin,
        "默认管理员账号已完成初始化检查"
    );

    let restored_accounts = services.account_pool.restore_from_store().await?;
    tracing::info!(
        count = restored_accounts,
        "运行时账号池已从 PostgreSQL 恢复"
    );

    let task_coordinator =
        crate::bootstrap::tasks::coordinator::TaskCoordinator::start(&runtime_config, &services);
    let app_state = crate::api::AppState::from(&services);

    Ok((
        crate::api::router::router().with_state(app_state),
        task_coordinator,
    ))
}

fn init_logging(config: &AppConfig) -> Result<Option<LogGuard>, crate::infra::logging::LogError> {
    if !config.logging.enabled {
        return Ok(None);
    }

    let rotation = RotationConfig::new(&config.logging.directory, config.logging.retention_days);
    Ok(Some(init_tracing(&rotation)?))
}

async fn wait_for_scheduled_restart() {
    let Some(delay_ms) = env::var(RESTART_DELAY_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
    else {
        return;
    };

    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
}

#[cfg(unix)]
fn exec_replacement_process(
    executable_path: std::path::PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    let error = Command::new(executable_path)
        .args(env::args_os().skip(1))
        .exec();
    Err(Box::new(error))
}

#[cfg(not(unix))]
fn exec_replacement_process(
    _executable_path: std::path::PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    Err(Box::new(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "process exec restart is only supported on Unix",
    )))
}
