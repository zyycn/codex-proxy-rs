use std::{path::PathBuf, sync::Arc};

use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::auth::{api_key::ApiKeyHasher, api_key_repository::ClientApiKeyRepository};
use crate::codex::accounts::{
    pool::{AccountPool, AccountPoolOptions, RotationStrategy},
    repository::{AccountRepository, AccountRepositoryResult, AccountUsageRepository},
    service::AccountService,
};
use crate::codex::cookies::repository::CookieRepository;
use crate::codex::models::repository::ModelSnapshotRepository;
use crate::codex::models::service::ModelService;
use crate::codex::oauth::{OAuthClient, PkceSessionStore, TokenRefresher};
use crate::codex::upstream::CodexUpstreamService;
use crate::config::AppConfig;
use crate::logs::repository::EventLogRepository;
use crate::service::{
    admin_auth::AdminAuthService, api_key::ApiKeyService, chat::ChatService, log::LogService,
    responses::ResponsesService, settings::SettingsService, usage::UsageService,
};
use crate::utils::crypto::SecretBox;

#[derive(Clone)]
pub struct AppState {
    pub services: Arc<AppServices>,
}

pub struct AppServices {
    pub config: AppConfig,
    pub admin_auth: AdminAuthService,
    pub accounts: AccountService,
    pub api_keys: ApiKeyService,
    pub logs: LogService,
    pub usage: UsageService,
    pub settings: SettingsService,
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

    pub fn config(&self) -> &AppConfig {
        &self.services.config
    }

    pub async fn reload_account_pool_from_repository(&self) -> AccountRepositoryResult<usize> {
        self.services
            .accounts
            .reload_runtime_accounts_from_repository()
            .await
    }

    fn from_dependencies(config: AppConfig, dependencies: AppStateDependencies) -> Self {
        let AppStateDependencies {
            pool,
            secret_box,
            api_key_hasher,
            token_refresher,
            oauth_client,
            local_config_path,
        } = dependencies;
        let pool_ref = pool.as_ref();
        let secret_box_ref = secret_box.as_ref();
        let account_pool = account_pool_from_config(&config);
        let oauth_sessions = Arc::new(Mutex::new(PkceSessionStore::default()));
        let api_keys = api_key_service(pool_ref, api_key_hasher.as_ref());
        let logs = log_service(pool_ref);
        let usage = usage_service(pool_ref);
        let settings = SettingsService::new(
            config.clone(),
            local_config_path.unwrap_or_else(|| PathBuf::from("local.yaml")),
        );
        let accounts = account_service(
            &config,
            account_repository(pool_ref, secret_box_ref),
            pool_ref,
            secret_box_ref,
            token_refresher.clone(),
            account_pool.clone(),
        );
        let admin_auth = admin_auth_service(
            &config,
            pool_ref,
            secret_box_ref,
            account_pool.clone(),
            oauth_client,
            oauth_sessions,
            accounts.clone(),
        );
        let V1Services {
            chat,
            responses,
            models,
        } = v1_services(
            &config,
            pool_ref,
            secret_box_ref,
            token_refresher,
            account_pool,
        );
        Self {
            services: Arc::new(AppServices {
                config,
                admin_auth,
                accounts,
                api_keys,
                logs,
                usage,
                settings,
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

struct V1Services {
    chat: ChatService,
    responses: ResponsesService,
    models: ModelService,
}

fn v1_services(
    config: &AppConfig,
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    account_pool: Arc<Mutex<AccountPool>>,
) -> V1Services {
    let upstream = codex_upstream_service(
        config,
        pool,
        secret_box,
        token_refresher,
        account_pool.clone(),
    );
    let models = model_service(config, pool, secret_box, account_pool);
    let model_config = config.model.clone();
    V1Services {
        chat: ChatService::new(model_config.clone(), models.clone(), upstream.clone()),
        responses: ResponsesService::new(model_config, models.clone(), upstream),
        models,
    }
}

fn model_service(
    config: &AppConfig,
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
    account_pool: Arc<Mutex<AccountPool>>,
) -> ModelService {
    ModelService::new(
        config.clone(),
        pool.cloned().map(ModelSnapshotRepository::new),
        account_repository(pool, secret_box),
        account_pool,
    )
}

fn admin_auth_service(
    config: &AppConfig,
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
    account_pool: Arc<Mutex<AccountPool>>,
    oauth_client: Option<Arc<dyn OAuthClient>>,
    oauth_sessions: Arc<Mutex<PkceSessionStore>>,
    accounts: AccountService,
) -> AdminAuthService {
    AdminAuthService::new(
        config.clone(),
        pool.cloned(),
        account_repository(pool, secret_box),
        account_pool,
        oauth_client,
        oauth_sessions,
        accounts,
    )
}

fn account_service(
    config: &AppConfig,
    repository: Option<AccountRepository>,
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    account_pool: Arc<Mutex<AccountPool>>,
) -> AccountService {
    AccountService::new(
        config.clone(),
        repository,
        pool.cloned().map(AccountUsageRepository::new),
        cookie_repository(pool, secret_box),
        token_refresher,
        account_pool,
    )
}

fn codex_upstream_service(
    config: &AppConfig,
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    account_pool: Arc<Mutex<AccountPool>>,
) -> CodexUpstreamService {
    CodexUpstreamService::new(
        config.clone(),
        account_pool,
        account_repository(pool, secret_box),
        cookie_repository(pool, secret_box),
        pool.cloned().map(EventLogRepository::new),
        token_refresher,
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

fn log_service(pool: Option<&SqlitePool>) -> LogService {
    LogService::new(pool.cloned().map(EventLogRepository::new))
}

fn usage_service(pool: Option<&SqlitePool>) -> UsageService {
    UsageService::new(pool.cloned().map(AccountUsageRepository::new))
}
