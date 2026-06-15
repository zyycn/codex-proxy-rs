use std::{path::PathBuf, sync::Arc, time::Duration};

use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::admin::{
    client_keys::service::ApiKeyService,
    session::{
        repository::AdminAuthRepository,
        service::{AdminAuthDependencies, AdminAuthService},
    },
    settings::SettingsService,
};
use crate::codex::accounts::cookies::repository::CookieRepository;
use crate::codex::accounts::{
    pool::{AccountPool, AccountPoolOptions, RotationStrategy},
    repository::{AccountRepository, AccountRepositoryResult, AccountUsageRepository},
    service::{AccountService, AccountServiceDependencies},
};
use crate::codex::gateway::fingerprint::model::Fingerprint;
use crate::codex::gateway::oauth::{OAuthClient, OAuthConfig, PkceSessionStore, TokenRefresher};
use crate::codex::gateway::transport::websocket::{CodexWebSocketPool, CodexWebSocketPoolConfig};
use crate::codex::models::repository::ModelSnapshotRepository;
use crate::codex::models::service::ModelService;
use crate::codex::serving::dispatch::affinity::{
    SessionAffinityRepository, SessionAffinityRepositoryResult,
};
use crate::codex::serving::dispatch::{CodexUpstreamRepositories, CodexUpstreamService};
use crate::codex::serving::{
    chat::ChatService, diagnostics::DiagnosticsService, responses::ResponsesService,
};
use crate::codex::usage::service::UsageService;
use crate::codex::{events::repository::EventLogRepository, events::service::LogService};
use crate::config::AppConfig;
use crate::platform::crypto::SecretBox;
use crate::platform::identity::{
    client_key::ApiKeyHasher, client_key_repository::ClientApiKeyRepository,
};

#[derive(Clone)]
pub struct AppState {
    pub services: Arc<AppServices>,
}

pub struct AppServices {
    pub config: Arc<AppConfig>,
    pub admin_auth: AdminAuthService,
    pub accounts: AccountService,
    pub api_keys: ApiKeyService,
    pub logs: LogService,
    pub usage: UsageService,
    pub settings: SettingsService,
    pub diagnostics: DiagnosticsService,
    pub chat: ChatService,
    pub responses: ResponsesService,
    pub models: ModelService,
}

#[derive(Default)]
struct AppStateDependencies {
    pool: Option<SqlitePool>,
    secret_box: Option<SecretBox>,
    api_key_hasher: Option<ApiKeyHasher>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    oauth_client: Option<Arc<dyn OAuthClient>>,
    local_config_path: Option<PathBuf>,
    fingerprint: Option<Fingerprint>,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self::from_dependencies(config, AppStateDependencies::default())
    }

    pub fn with_pool(config: AppConfig, pool: SqlitePool) -> Self {
        Self::from_dependencies(
            config,
            AppStateDependencies {
                pool: Some(pool),
                ..AppStateDependencies::default()
            },
        )
    }

    pub fn with_pool_and_local_config_path(
        config: AppConfig,
        pool: SqlitePool,
        local_config_path: impl Into<PathBuf>,
    ) -> Self {
        Self::from_dependencies(
            config,
            AppStateDependencies {
                pool: Some(pool),
                local_config_path: Some(local_config_path.into()),
                ..AppStateDependencies::default()
            },
        )
    }

    pub fn with_pool_and_secret_box(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
    ) -> Self {
        Self::from_dependencies(
            config,
            AppStateDependencies {
                pool: Some(pool),
                secret_box: Some(secret_box),
                ..AppStateDependencies::default()
            },
        )
    }

    pub fn with_pool_secret_and_api_key_hasher(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        api_key_hasher: ApiKeyHasher,
    ) -> Self {
        Self::from_dependencies(
            config,
            AppStateDependencies {
                pool: Some(pool),
                secret_box: Some(secret_box),
                api_key_hasher: Some(api_key_hasher),
                ..AppStateDependencies::default()
            },
        )
    }

    pub fn with_pool_secret_api_key_hasher_and_token_refresher<C>(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        api_key_hasher: ApiKeyHasher,
        token_refresher: C,
    ) -> Self
    where
        C: TokenRefresher,
    {
        let token_refresher: Arc<dyn TokenRefresher> = Arc::new(token_refresher);
        Self::from_dependencies(
            config,
            AppStateDependencies {
                pool: Some(pool),
                secret_box: Some(secret_box),
                api_key_hasher: Some(api_key_hasher),
                token_refresher: Some(token_refresher),
                ..AppStateDependencies::default()
            },
        )
    }

    pub fn with_pool_secret_api_key_hasher_and_oauth_client<C>(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        api_key_hasher: ApiKeyHasher,
        oauth_client: C,
    ) -> Self
    where
        C: OAuthClient + TokenRefresher,
    {
        let oauth_client = Arc::new(oauth_client);
        let token_refresher: Arc<dyn TokenRefresher> = oauth_client.clone();
        let oauth_client: Arc<dyn OAuthClient> = oauth_client;
        Self::from_dependencies(
            config,
            AppStateDependencies {
                pool: Some(pool),
                secret_box: Some(secret_box),
                api_key_hasher: Some(api_key_hasher),
                token_refresher: Some(token_refresher),
                oauth_client: Some(oauth_client),
                ..AppStateDependencies::default()
            },
        )
    }

    pub fn with_pool_secret_api_key_hasher_and_fingerprint(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        api_key_hasher: ApiKeyHasher,
        fingerprint: Fingerprint,
    ) -> Self {
        Self::from_dependencies(
            config,
            AppStateDependencies {
                pool: Some(pool),
                secret_box: Some(secret_box),
                api_key_hasher: Some(api_key_hasher),
                fingerprint: Some(fingerprint),
                ..AppStateDependencies::default()
            },
        )
    }

    pub fn with_pool_secret_api_key_hasher_oauth_client_and_fingerprint<C>(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        api_key_hasher: ApiKeyHasher,
        oauth_client: C,
        fingerprint: Fingerprint,
    ) -> Self
    where
        C: OAuthClient + TokenRefresher,
    {
        let oauth_client = Arc::new(oauth_client);
        let token_refresher: Arc<dyn TokenRefresher> = oauth_client.clone();
        let oauth_client: Arc<dyn OAuthClient> = oauth_client;
        Self::from_dependencies(
            config,
            AppStateDependencies {
                pool: Some(pool),
                secret_box: Some(secret_box),
                api_key_hasher: Some(api_key_hasher),
                token_refresher: Some(token_refresher),
                oauth_client: Some(oauth_client),
                fingerprint: Some(fingerprint),
                ..AppStateDependencies::default()
            },
        )
    }

    pub fn config(&self) -> &AppConfig {
        &self.services.config
    }

    pub async fn reload_account_pool_from_repository(&self) -> AccountRepositoryResult<usize> {
        self.services
            .accounts
            .reload_runtime_accounts_from_repository()
            .await
    }

    pub async fn reload_session_affinity_from_repository(
        &self,
    ) -> SessionAffinityRepositoryResult<usize> {
        self.services
            .responses
            .reload_session_affinity_from_repository()
            .await
    }

    /// 获取 AccountService 的引用（用于调度器）
    pub fn account_service(&self) -> Arc<AccountService> {
        Arc::new(self.services.accounts.clone())
    }

    /// 获取 ModelService 的引用（用于调度器）
    pub fn model_service(&self) -> Arc<ModelService> {
        Arc::new(self.services.models.clone())
    }

    fn from_dependencies(config: AppConfig, dependencies: AppStateDependencies) -> Self {
        let AppStateDependencies {
            pool,
            secret_box,
            api_key_hasher,
            token_refresher,
            oauth_client,
            local_config_path,
            fingerprint,
        } = dependencies;
        let pool_ref = pool.as_ref();
        let secret_box_ref = secret_box.as_ref();
        let account_pool = account_pool_from_config(&config);
        let websocket_pool = websocket_pool_from_config(&config);
        let oauth_sessions = Arc::new(Mutex::new(PkceSessionStore::default()));
        let api_keys = api_key_service(pool_ref, api_key_hasher.as_ref());
        let config = Arc::new(config);
        let logs = log_service(&config, pool_ref);
        let usage = usage_service(pool_ref);
        let fingerprint = fingerprint.unwrap_or_else(Fingerprint::default_codex_desktop);
        let settings = SettingsService::new(
            config.clone(),
            local_config_path.unwrap_or_else(|| PathBuf::from("local.yaml")),
        );
        let diagnostics = DiagnosticsService::new(config.clone(), fingerprint.clone());
        let service_context = ServiceBuildContext {
            config: config.clone(),
            pool: pool_ref,
            secret_box: secret_box_ref,
            account_pool,
            websocket_pool,
        };
        let accounts = account_service(
            &service_context,
            token_refresher.clone(),
            fingerprint.clone(),
        );
        let admin_auth = admin_auth_service(
            &service_context,
            oauth_client,
            oauth_sessions,
            accounts.clone(),
        );
        let V1Services {
            chat,
            responses,
            models,
        } = v1_services(service_context, logs.clone(), fingerprint);
        Self {
            services: Arc::new(AppServices {
                config,
                admin_auth,
                accounts,
                api_keys,
                logs,
                usage,
                settings,
                diagnostics,
                chat,
                responses,
                models,
            }),
        }
    }
}

fn account_pool_from_config(config: &AppConfig) -> Arc<Mutex<AccountPool>> {
    Arc::new(Mutex::new(AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: config.auth.max_concurrent_per_account,
        stale_slot_ttl: chrono::Duration::minutes(5),
        rotation_strategy: match config.auth.rotation_strategy.as_str() {
            "round_robin" => RotationStrategy::RoundRobin,
            "sticky" => RotationStrategy::Sticky,
            _ => RotationStrategy::LeastUsed,
        },
        skip_quota_limited: config.quota.skip_exhausted,
        tier_priority: config.auth.tier_priority.clone(),
        model_plan_allowlist: std::collections::BTreeMap::new(),
    })))
}

fn websocket_pool_from_config(config: &AppConfig) -> Arc<CodexWebSocketPool> {
    Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        enabled: config.ws_pool.enabled,
        max_age: Duration::from_millis(config.ws_pool.max_age_ms),
        max_per_account: config.ws_pool.max_per_account,
        ..CodexWebSocketPoolConfig::default()
    }))
}

struct V1Services {
    chat: ChatService,
    responses: ResponsesService,
    models: ModelService,
}

#[derive(Clone)]
struct ServiceBuildContext<'a> {
    config: Arc<AppConfig>,
    pool: Option<&'a SqlitePool>,
    secret_box: Option<&'a SecretBox>,
    account_pool: Arc<Mutex<AccountPool>>,
    websocket_pool: Arc<CodexWebSocketPool>,
}

fn v1_services(
    context: ServiceBuildContext<'_>,
    logs: LogService,
    fingerprint: Fingerprint,
) -> V1Services {
    let upstream = codex_upstream_service(&context, logs, fingerprint.clone());
    let models = model_service(
        context.config.clone(),
        context.pool,
        context.secret_box,
        context.account_pool,
        fingerprint,
    );
    let model_config = context.config.model.clone();
    V1Services {
        chat: ChatService::new(model_config.clone(), models.clone(), upstream.clone()),
        responses: ResponsesService::new(model_config, models.clone(), upstream),
        models,
    }
}

fn model_service(
    config: Arc<AppConfig>,
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
    account_pool: Arc<Mutex<AccountPool>>,
    fingerprint: Fingerprint,
) -> ModelService {
    ModelService::new(
        config,
        pool.cloned().map(ModelSnapshotRepository::new),
        account_repository(pool, secret_box),
        account_pool,
        fingerprint,
    )
}

fn admin_auth_service(
    context: &ServiceBuildContext<'_>,
    oauth_client: Option<Arc<dyn OAuthClient>>,
    oauth_sessions: Arc<Mutex<PkceSessionStore>>,
    accounts: AccountService,
) -> AdminAuthService {
    let config = context.config.clone();
    AdminAuthService::new(
        config,
        AdminAuthDependencies {
            auth_repository: context.pool.cloned().map(AdminAuthRepository::new),
            account_repository: account_repository(context.pool, context.secret_box),
            account_pool: context.account_pool.clone(),
            oauth_config: OAuthConfig::from_auth_config(&context.config.auth),
            oauth_client,
            oauth_sessions,
            accounts,
        },
    )
}

fn account_service(
    context: &ServiceBuildContext<'_>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    fingerprint: Fingerprint,
) -> AccountService {
    AccountService::new(
        context.config.clone(),
        AccountServiceDependencies {
            repository: account_repository(context.pool, context.secret_box),
            usage_repository: context.pool.cloned().map(AccountUsageRepository::new),
            cookie_repository: cookie_repository(context.pool, context.secret_box),
            token_refresher,
            account_pool: context.account_pool.clone(),
            websocket_pool: context.websocket_pool.clone(),
            fingerprint,
        },
    )
}

fn codex_upstream_service(
    context: &ServiceBuildContext<'_>,
    logs: LogService,
    fingerprint: Fingerprint,
) -> CodexUpstreamService {
    CodexUpstreamService::new(
        context.config.clone(),
        context.account_pool.clone(),
        CodexUpstreamRepositories {
            account: account_repository(context.pool, context.secret_box),
            cookie: cookie_repository(context.pool, context.secret_box),
            session_affinity: context.pool.cloned().map(SessionAffinityRepository::new),
        },
        logs,
        fingerprint,
        context.websocket_pool.clone(),
    )
}

fn account_repository(
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
) -> Option<AccountRepository> {
    Some(AccountRepository::new(pool?.clone(), secret_box?.clone()))
}

fn cookie_repository(
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
) -> Option<CookieRepository> {
    Some(CookieRepository::new(pool?.clone(), secret_box?.clone()))
}

fn api_key_service(pool: Option<&SqlitePool>, hasher: Option<&ApiKeyHasher>) -> ApiKeyService {
    ApiKeyService::new(
        pool.cloned().map(ClientApiKeyRepository::new),
        hasher.cloned(),
    )
}

fn log_service(config: &AppConfig, pool: Option<&SqlitePool>) -> LogService {
    LogService::new(
        config.logging.clone(),
        pool.cloned().map(EventLogRepository::new),
    )
}

fn usage_service(pool: Option<&SqlitePool>) -> UsageService {
    UsageService::new(pool.cloned().map(AccountUsageRepository::new))
}
