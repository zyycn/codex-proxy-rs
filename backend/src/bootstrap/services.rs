//! 运行时服务集合。

use std::{env, error::Error, future::IntoFuture, sync::Arc, time::Duration};

use axum::Router;
use secrecy::ExposeSecret;

use crate::{
    api::admin::{
        accounts_routes::query::{AccountListQueryService, RefreshActivityQuery},
        dashboard_routes::{
            DashboardDesktopRelease, DashboardDesktopReleaseQuery, DashboardDesktopReleaseSnapshot,
            DashboardQueryService, DashboardQueryServiceParts, DashboardWireProfile,
            DashboardWireProfileQuery,
        },
    },
    auth::{
        service::SessionService,
        store::{PgAdminUserStore, RedisAdminSessionStore},
    },
    bootstrap::config::{AppConfig, BootstrapConfig},
    dispatch::{
        affinity::{RedisSessionAffinityStore, SessionAffinityService},
        service::{ResponseDispatchService, ResponseDispatchServiceParts},
    },
    fleet::{
        cookies::PgCookieStore,
        manage::{AccountManageService, AccountManageServiceParts},
        pool::{
            AccountPoolOptions, AccountPoolRuntimeOptions, AccountPoolService,
            AccountPoolStaticSettings, RotationStrategy,
        },
        refresh::{
            RedisRefreshLeaseStore, RefreshPolicy, RuntimeRefreshPolicy, TokenRefreshService,
        },
        store::{AccountStore, PgAccountStore},
        usage::AccountUsageStore,
    },
    infra::{
        database::connect,
        identity::AccountPseudonymizer,
        logging::{LogError, LogGuard, RotationConfig, TracingConfig, init_tracing},
        paths::{ensure_data_dir, load_or_create_identity_secret},
        redis::RedisConnection,
    },
    keys::{manage::KeyManageService, service::KeyVerifier, store::PgClientKeyStore},
    models::{
        gateway::ModelCatalogSource,
        service::{ModelService, ModelServiceError},
        store::{ModelSnapshotStore, RedisModelSnapshotStore},
        types::ModelConfig,
    },
    settings::{SettingsSnapshot, service::SettingsService},
    telemetry::{
        account_usage::query::AccountUsageQueryService,
        account_usage::store::PgAccountUsageStore,
        buckets::store::PgRequestBucketStore,
        ops::query::OpsQueryService,
        ops::store::PgOpsErrorLogStore,
        recorder::Recorder,
        usage::query::UsageQueryService,
        usage::store::{DEFAULT_USAGE_RECORD_CAPTURE_BODY, PgUsageRecordStore},
    },
    update::service::SystemUpdateService,
    upstream::openai::{
        desktop_release::DesktopReleaseStatus,
        profile::{CodexWireProfile, CodexWireProfileState},
        token_client::{OpenAiTokenClient, TokenClientConfig, default_openai_token_client},
        transport::{
            CodexBackendClient, CodexWebSocketPool, CodexWebSocketPoolConfig, build_reqwest_client,
            tls::CustomCaError,
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
    pub settings: Arc<SettingsService>,
    pub refresh_policy: RuntimeRefreshPolicy,
    pub admin_accounts: Arc<AccountManageService>,
    pub usage_records: Arc<UsageQueryService>,
    pub ops_errors: Arc<OpsQueryService>,
    pub usage: Arc<AccountUsageQueryService>,
    pub account_pool: Arc<AccountPoolService>,
    pub token_refresh: Arc<TokenRefreshService<OpenAiTokenClient>>,
    pub responses: Arc<ResponseDispatchService>,
    pub session_affinity: Arc<SessionAffinityService>,
    pub codex: Arc<CodexBackendClient>,
    pub websocket_pool: Option<Arc<CodexWebSocketPool>>,
    pub wire_profile: CodexWireProfileState,
    pub desktop_release: DesktopReleaseStatus,
    pub account_pseudonymizer: Arc<AccountPseudonymizer>,
    pub system_update: Arc<SystemUpdateService>,
    pub connection_drain: crate::api::middleware::connection_drain::ConnectionDrain,
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
            enabled: config.telemetry.enabled,
            capture_body: DEFAULT_USAGE_RECORD_CAPTURE_BODY,
        }
    }
}

impl DashboardWireProfileQuery for CodexWireProfileState {
    fn snapshot(&self) -> DashboardWireProfile {
        let profile = CodexWireProfileState::snapshot(self);
        let user_agent = profile.user_agent();
        DashboardWireProfile {
            originator: profile.originator,
            codex_version: profile.codex_version,
            desktop_version: profile.desktop_version,
            desktop_build: profile.desktop_build,
            os_type: profile.os_type,
            os_version: profile.os_version,
            arch: profile.arch,
            terminal: profile.terminal,
            user_agent,
            verified_at: profile.verified_at,
        }
    }
}

impl DashboardDesktopReleaseQuery for DesktopReleaseStatus {
    fn snapshot(&self) -> DashboardDesktopReleaseSnapshot {
        let snapshot = DesktopReleaseStatus::snapshot(self);
        DashboardDesktopReleaseSnapshot {
            checked_at: snapshot.checked_at,
            latest: snapshot.latest.map(|release| DashboardDesktopRelease {
                version: release.version,
                build: release.build,
                published_at: release.published_at,
                minimum_system_version: release.minimum_system_version,
                hardware_requirements: release.hardware_requirements,
                download_url: release.download_url,
                download_size: release.download_size,
                signature_present: release.signature_present,
            }),
            last_error: snapshot.last_error,
        }
    }
}

impl Services {
    pub fn new(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        wire_profile: CodexWireProfileState,
    ) -> Self {
        Self::try_new(config, stores, wire_profile)
            .expect("failed to build runtime services with configured TLS transport")
    }

    pub fn try_new(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        wire_profile: CodexWireProfileState,
    ) -> Result<Self, CustomCaError> {
        Self::try_with_usage_record_options(
            config,
            stores,
            wire_profile,
            UsageRecordOptions::from_config(config),
        )
    }

    pub fn try_with_usage_record_options(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        wire_profile: CodexWireProfileState,
        usage_record_options: UsageRecordOptions,
    ) -> Result<Self, CustomCaError> {
        let mut identity_secret = [0u8; 32];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut identity_secret);
        Self::try_with_identity_secret_and_usage_record_options(
            config,
            stores,
            wire_profile,
            identity_secret,
            usage_record_options,
        )
    }

    fn try_with_identity_secret_and_usage_record_options(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        wire_profile: CodexWireProfileState,
        identity_secret: [u8; 32],
        usage_record_options: UsageRecordOptions,
    ) -> Result<Self, CustomCaError> {
        let account_pseudonymizer = Arc::new(AccountPseudonymizer::new(identity_secret));
        let account_identity = Arc::new(crate::dispatch::affinity::AccountIdentityService::new(
            (*account_pseudonymizer).clone(),
        ));
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
                max_total: config.ws_pool.max_total,
                max_connecting: config.ws_pool.max_connecting,
                initial_event_timeout: (config.ws_pool.initial_event_timeout_ms > 0).then(|| {
                    std::time::Duration::from_millis(config.ws_pool.initial_event_timeout_ms)
                }),
                ..CodexWebSocketPoolConfig::default()
            }))
        });
        let codex = {
            let websocket_initial_event_timeout = (config.ws_pool.initial_event_timeout_ms > 0)
                .then(|| std::time::Duration::from_millis(config.ws_pool.initial_event_timeout_ms));
            let client = CodexBackendClient::new(
                build_reqwest_client()?,
                config.api.base_url.clone(),
                wire_profile.clone(),
            )
            .with_websocket_initial_event_timeout(websocket_initial_event_timeout);
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
        let account_pool_options = account_pool_options_from_config(config);
        let account_pool_static = AccountPoolStaticSettings {
            skip_quota_limited: account_pool_options.skip_quota_limited,
            tier_priority: account_pool_options.tier_priority.clone(),
        };
        let account_pool = Arc::new(AccountPoolService::new(
            account_store.clone(),
            account_usage_store_trait,
            account_pool_options,
            settings_snapshot.request_interval_ms,
        ));
        let refresh_policy = RuntimeRefreshPolicy::new(RefreshPolicy {
            refresh_margin_seconds: config.auth.refresh_margin_seconds,
            refresh_concurrency: config.auth.refresh_concurrency,
        });
        let settings = Arc::new(SettingsService::new(
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
            TokenRefreshService::new(
                stores.accounts.clone(),
                refresh_policy.clone(),
                token_client.clone(),
            )
            .with_refresh_lease_store(stores.refresh_leases.clone())
            .with_account_pool_sync(account_pool.clone()),
        );
        let upstream_client: Arc<dyn ModelCatalogSource> = codex.clone();
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
            upstream: codex.clone(),
            models: models.clone(),
            account_pool: account_pool.clone(),
            token_refresher: Arc::new(token_client),
            refresh_leases: stores.refresh_leases.clone(),
            oauth: crate::fleet::manage::oauth::AccountOAuthService::new(
                reqwest::Client::new(),
                config.auth.oauth_client_id.clone(),
                config.auth.oauth_token_endpoint.clone(),
            ),
            refresh_policy: refresh_policy.clone(),
            account_pseudonymizer: account_pseudonymizer.clone(),
        }));
        let usage = Arc::new(AccountUsageQueryService::new(stores.account_usage.clone()));
        let session_affinity =
            Arc::new(SessionAffinityService::new(stores.session_affinity.clone()));

        let connection_drain = crate::api::middleware::connection_drain::ConnectionDrain::default();
        let cloudflare_recovery = crate::dispatch::controllers::cloudflare::CloudflareRecovery::new(
            stores.cookies.clone(),
        );
        let responses = Arc::new(ResponseDispatchService::new(ResponseDispatchServiceParts {
            account_pool: account_pool.clone(),
            models: models.clone(),
            codex: codex.clone(),
            session_affinity: session_affinity.clone(),
            recorder,
            account_identity,
            cloudflare: cloudflare_recovery,
            shutdown: connection_drain.cancellation_token(),
        }));
        let system_update = Arc::new(SystemUpdateService::from_env());

        let desktop_release = DesktopReleaseStatus::default();

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
            wire_profile,
            desktop_release,
            account_pseudonymizer,
            system_update,
            connection_drain,
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
        let refresh_activity = services.token_refresh.clone() as Arc<dyn RefreshActivityQuery>;
        let account_list = Arc::new(AccountListQueryService::new(
            services.admin_accounts.clone(),
            services.usage.clone(),
            refresh_activity.clone(),
        ));
        let dashboard = Arc::new(DashboardQueryService::new(DashboardQueryServiceParts {
            accounts: services.accounts.clone(),
            usage: services.usage.clone(),
            usage_records: services.usage_records.clone(),
            account_pool: services.account_pool.clone(),
            refresh_activity,
            settings: services.settings.clone(),
            wire_profile: Arc::new(services.wire_profile.clone()),
            desktop_release: Arc::new(services.desktop_release.clone()),
        }));
        Self {
            account_list,
            dashboard,
            health_probe: Arc::new(crate::bootstrap::state::RuntimeHealthProbe::new(
                services.database.clone(),
                services.redis.clone(),
            )),
            models: services.models.clone(),
            client_keys: services.client_keys.clone(),
            admin_client_keys: services.admin_client_keys.clone(),
            admin_sessions: services.admin_sessions.clone(),
            settings: services.settings.clone(),
            admin_accounts: services.admin_accounts.clone(),
            usage_records: services.usage_records.clone(),
            ops_errors: services.ops_errors.clone(),
            account_pool: services.account_pool.clone(),
            responses: services.responses.clone(),
            session_affinity: services.session_affinity.clone(),
            process_control: Arc::new(crate::bootstrap::shutdown::RuntimeProcessControl),
            system_update: services.system_update.clone(),
            connection_drain: services.connection_drain.clone(),
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

pub fn wire_profile_from_config(
    config: &crate::bootstrap::config::WireProfileConfig,
) -> CodexWireProfile {
    CodexWireProfile {
        originator: config.originator.clone(),
        codex_version: config.codex_version.clone(),
        desktop_version: config.desktop_version.clone(),
        desktop_build: config.desktop_build.clone(),
        os_type: config.os_type.clone(),
        os_version: config.os_version.clone(),
        arch: config.arch.clone(),
        terminal: config.terminal.clone(),
        verified_at: config.verified_at,
    }
}

pub fn account_pool_options_from_config(config: &AppConfig) -> AccountPoolOptions {
    let static_settings = AccountPoolStaticSettings {
        skip_quota_limited: config.quota.skip_exhausted,
        tier_priority: config.auth.tier_priority.clone(),
    };
    account_pool_runtime_options(&settings_snapshot_from_config(config), &static_settings).options
}

pub fn account_pool_runtime_options(
    settings: &SettingsSnapshot,
    static_settings: &AccountPoolStaticSettings,
) -> AccountPoolRuntimeOptions {
    AccountPoolRuntimeOptions {
        options: AccountPoolOptions {
            max_concurrent_per_account: settings.max_concurrent_per_account,
            rotation_strategy: match settings.rotation_strategy.as_str() {
                "smart" => RotationStrategy::Smart,
                "quota_reset_priority" => RotationStrategy::QuotaResetPriority,
                "round_robin" => RotationStrategy::RoundRobin,
                "sticky" => RotationStrategy::Sticky,
                _ => RotationStrategy::Smart,
            },
            skip_quota_limited: static_settings.skip_quota_limited,
            tier_priority: static_settings.tier_priority.clone(),
            ..AccountPoolOptions::default()
        },
        request_interval_ms: settings.request_interval_ms,
    }
}

pub async fn run(config: BootstrapConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    wait_for_scheduled_restart().await;

    let host = config.app().server.host.clone();
    let port = config.app().server.port;
    let log_guard = init_logging(config.app())?;
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    let (router, task_coordinator, connection_drain) = build_router(config).await?;
    let mut task_coordinator = Some(task_coordinator);
    let mut restart_path = None;

    let serve_result = {
        let (graceful_shutdown_tx, graceful_shutdown_rx) = tokio::sync::oneshot::channel();
        let serve = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = graceful_shutdown_rx.await;
        })
        .into_future();
        tokio::pin!(serve);
        tokio::select! {
            result = &mut serve => result,
            action = crate::bootstrap::shutdown::shutdown_signal() => {
                match action {
                    crate::bootstrap::shutdown::ShutdownAction::Restart(executable_path) => {
                        restart_path = Some(executable_path);
                        Ok(())
                    }
                    crate::bootstrap::shutdown::ShutdownAction::Graceful => {
                        let _ = graceful_shutdown_tx.send(());
                        let active_websocket_connections = connection_drain.begin_shutdown();
                        tracing::info!(
                            active_websocket_connections,
                            "正在排空入站连接并关闭运行时任务"
                        );
                        let coordinator = task_coordinator.take();
                        let connection_drain = connection_drain.clone();
                        let runtime_shutdown = async move {
                            let shutdown_tasks = async move {
                                if let Some(coordinator) = coordinator {
                                    coordinator.shutdown().await;
                                }
                            };
                            tokio::join!(shutdown_tasks, connection_drain.wait());
                        };
                        let drain_runtime = async {
                            let (serve_result, ()) = tokio::join!(&mut serve, runtime_shutdown);
                            serve_result
                        };
                        tokio::pin!(drain_runtime);
                        tokio::select! {
                            result = &mut drain_runtime => result,
                            () = tokio::time::sleep(HTTP_DRAIN_TIMEOUT) => {
                                tracing::warn!(
                                    timeout_secs = HTTP_DRAIN_TIMEOUT.as_secs(),
                                    "HTTP graceful shutdown timed out; cancelling remaining connections"
                                );
                                Ok(())
                            }
                            _ = crate::bootstrap::shutdown::shutdown_signal() => {
                                tracing::warn!("Received a second shutdown signal; cancelling remaining connections");
                                Ok(())
                            }
                        }
                    }
                }
            }
        }
    };

    if let Some(executable_path) = restart_path {
        tracing::info!(
            executable = %executable_path.display(),
            "正在以更新后的二进制替换当前进程"
        );
        drop(log_guard);
        return exec_replacement_process(executable_path);
    }

    if let Some(coordinator) = task_coordinator.take() {
        let active_websocket_connections = connection_drain.begin_shutdown();
        tracing::info!(
            active_websocket_connections,
            "HTTP 服务已退出，正在关闭其余运行时任务"
        );
        tokio::join!(coordinator.shutdown(), connection_drain.wait());
    }
    serve_result?;

    Ok(())
}

const HTTP_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

async fn build_router(
    bootstrap_config: BootstrapConfig,
) -> Result<
    (
        Router,
        crate::bootstrap::tasks::coordinator::TaskCoordinator,
        crate::api::middleware::connection_drain::ConnectionDrain,
    ),
    Box<dyn Error + Send + Sync>,
> {
    let crate::bootstrap::config::BootstrapConfigParts {
        app: mut config,
        database_url,
        redis_url,
        admin_default_password,
    } = bootstrap_config.into_parts();

    let pool = connect(database_url.expose_secret()).await?;
    drop(database_url);
    let redis = RedisConnection::connect(redis_url.expose_secret(), "cpr").await?;
    drop(redis_url);
    let settings =
        SettingsService::load_or_initialize(settings_snapshot_from_config(&config), &pool).await?;
    apply_settings_to_config(&mut config, &settings);

    let stores = BackgroundTaskStores {
        redis: redis.clone(),
        accounts: PgAccountStore::new(pool.clone()),
        admin_users: PgAdminUserStore::new(pool.clone()),
        admin_sessions: RedisAdminSessionStore::new(redis.clone()),
        cookies: PgCookieStore::new(pool.clone()),
        session_affinity: RedisSessionAffinityStore::new(redis.clone()),
        refresh_leases: RedisRefreshLeaseStore::new(redis.clone()),
        model_snapshots: RedisModelSnapshotStore::new(redis.clone()),
        client_keys: PgClientKeyStore::new(pool.clone()),
        usage_records: PgUsageRecordStore::new(pool.clone()),
        ops_errors: PgOpsErrorLogStore::new(pool.clone()),
        account_usage: PgAccountUsageStore::new(pool.clone()),
        request_buckets: PgRequestBucketStore::new(pool),
    };

    let data_dir = ensure_data_dir(&config.runtime.data_directory)?;
    let identity_secret = load_or_create_identity_secret(&data_dir)?;
    let wire_profile = CodexWireProfileState::new(wire_profile_from_config(&config.wire_profile));
    let runtime_config = crate::bootstrap::state::RuntimeConfig::from(&config);
    let services = Services::try_with_identity_secret_and_usage_record_options(
        &config,
        stores,
        wire_profile,
        identity_secret,
        UsageRecordOptions::from_config(&config),
    )?;
    services.initialize_hot_path_state().await?;

    let created_default_admin = services
        .admin_sessions
        .ensure_default_admin(admin_default_password.expose_secret())
        .await?;
    drop(admin_default_password);
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
    let connection_drain = services.connection_drain.clone();
    let router = crate::api::router::router().with_state(app_state).layer(
        axum::middleware::from_fn_with_state(
            connection_drain.clone(),
            crate::api::middleware::connection_drain::drain_response_body,
        ),
    );

    Ok((router, task_coordinator, connection_drain))
}

fn init_logging(config: &AppConfig) -> Result<LogGuard, LogError> {
    const MEBIBYTE: u64 = 1024 * 1024;

    let file = if config.logging.file.enabled {
        let max_file_size_bytes = config
            .logging
            .file
            .max_file_size_mb
            .checked_mul(MEBIBYTE)
            .ok_or(LogError::InvalidConfiguration(
                "logging.file.max_file_size_mb is too large",
            ))?;
        Some(RotationConfig::new(
            &config.logging.file.directory,
            config.logging.file.retention_days,
            max_file_size_bytes,
            config.logging.file.max_files,
        ))
    } else {
        None
    };
    init_tracing(&TracingConfig::new(
        &config.logging.level,
        config.logging.stdout,
        file,
    ))
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
