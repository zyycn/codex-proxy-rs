use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::accounts::{
    pool::{AccountPool, AccountPoolOptions, RotationStrategy},
    repository::{AccountRepository, AccountRepositoryResult, AccountUsageRepository},
};
use crate::auth::{
    api_key::ApiKeyHasher, api_key_repository::ClientApiKeyRepository, oauth::OAuthClient,
    refresh::TokenRefresher,
};
use crate::config::AppConfig;
use crate::cookies::repository::CookieRepository;
use crate::crypto::SecretBox;
use crate::logs::repository::EventLogRepository;
use crate::models::repository::ModelSnapshotRepository;

#[derive(Clone)]
pub struct AppState {
    pub services: Arc<AppServices>,
}

pub struct AppServices {
    pub config: AppConfig,
    pub db: Option<SqlitePool>,
    pub event_logs: Option<EventLogRepository>,
    pub secret_box: Option<SecretBox>,
    pub api_key_hasher: Option<ApiKeyHasher>,
    pub token_refresher: Option<Arc<dyn TokenRefresher>>,
    pub oauth_client: Option<Arc<dyn OAuthClient>>,
    pub account_pool: Arc<Mutex<AccountPool>>,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        let account_pool = account_pool_from_config(&config);
        Self {
            services: Arc::new(AppServices {
                config,
                db: None,
                event_logs: None,
                secret_box: None,
                api_key_hasher: None,
                token_refresher: None,
                oauth_client: None,
                account_pool,
            }),
        }
    }

    pub fn with_pool(config: AppConfig, pool: SqlitePool) -> Self {
        let account_pool = account_pool_from_config(&config);
        Self {
            services: Arc::new(AppServices {
                config,
                db: Some(pool.clone()),
                event_logs: Some(EventLogRepository::new(pool)),
                secret_box: None,
                api_key_hasher: None,
                token_refresher: None,
                oauth_client: None,
                account_pool,
            }),
        }
    }

    pub fn with_pool_and_secret_box(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
    ) -> Self {
        let account_pool = account_pool_from_config(&config);
        Self {
            services: Arc::new(AppServices {
                config,
                db: Some(pool.clone()),
                event_logs: Some(EventLogRepository::new(pool)),
                secret_box: Some(secret_box),
                api_key_hasher: None,
                token_refresher: None,
                oauth_client: None,
                account_pool,
            }),
        }
    }

    pub fn with_pool_secret_and_api_key_hasher(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        api_key_hasher: ApiKeyHasher,
    ) -> Self {
        let account_pool = account_pool_from_config(&config);
        Self {
            services: Arc::new(AppServices {
                config,
                db: Some(pool.clone()),
                event_logs: Some(EventLogRepository::new(pool)),
                secret_box: Some(secret_box),
                api_key_hasher: Some(api_key_hasher),
                token_refresher: None,
                oauth_client: None,
                account_pool,
            }),
        }
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
        let account_pool = account_pool_from_config(&config);
        Self {
            services: Arc::new(AppServices {
                config,
                db: Some(pool.clone()),
                event_logs: Some(EventLogRepository::new(pool)),
                secret_box: Some(secret_box),
                api_key_hasher: Some(api_key_hasher),
                token_refresher: Some(Arc::new(token_refresher)),
                oauth_client: None,
                account_pool,
            }),
        }
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
        let account_pool = account_pool_from_config(&config);
        let oauth_client = Arc::new(oauth_client);
        let token_refresher: Arc<dyn TokenRefresher> = oauth_client.clone();
        let oauth_client: Arc<dyn OAuthClient> = oauth_client;
        Self {
            services: Arc::new(AppServices {
                config,
                db: Some(pool.clone()),
                event_logs: Some(EventLogRepository::new(pool)),
                secret_box: Some(secret_box),
                api_key_hasher: Some(api_key_hasher),
                token_refresher: Some(token_refresher),
                oauth_client: Some(oauth_client),
                account_pool,
            }),
        }
    }

    pub fn config(&self) -> &AppConfig {
        &self.services.config
    }

    pub fn db(&self) -> Option<&SqlitePool> {
        self.services.db.as_ref()
    }

    pub fn event_logs(&self) -> Option<&EventLogRepository> {
        self.services.event_logs.as_ref()
    }

    pub fn account_repository(&self) -> Option<AccountRepository> {
        Some(AccountRepository::new(
            self.db()?.clone(),
            self.services.secret_box.clone()?,
        ))
    }

    pub fn account_usage_repository(&self) -> Option<AccountUsageRepository> {
        Some(AccountUsageRepository::new(self.db()?.clone()))
    }

    pub fn cookie_repository(&self) -> Option<CookieRepository> {
        Some(CookieRepository::new(
            self.db()?.clone(),
            self.services.secret_box.clone()?,
        ))
    }

    pub fn account_pool(&self) -> Arc<Mutex<AccountPool>> {
        self.services.account_pool.clone()
    }

    pub fn client_api_key_repository(&self) -> Option<ClientApiKeyRepository> {
        Some(ClientApiKeyRepository::new(self.db()?.clone()))
    }

    pub fn model_snapshot_repository(&self) -> Option<ModelSnapshotRepository> {
        Some(ModelSnapshotRepository::new(self.db()?.clone()))
    }

    pub fn api_key_hasher(&self) -> Option<&ApiKeyHasher> {
        self.services.api_key_hasher.as_ref()
    }

    pub fn token_refresher(&self) -> Option<Arc<dyn TokenRefresher>> {
        self.services.token_refresher.clone()
    }

    pub fn oauth_client(&self) -> Option<Arc<dyn OAuthClient>> {
        self.services.oauth_client.clone()
    }

    pub async fn reload_account_pool_from_repository(&self) -> AccountRepositoryResult<usize> {
        let Some(repo) = self.account_repository() else {
            return Ok(0);
        };
        let accounts = repo.list_pool_accounts().await?;
        let restored = accounts.len();
        let mut pool = self.services.account_pool.lock().await;
        for account in accounts {
            pool.insert(account);
        }
        Ok(restored)
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
