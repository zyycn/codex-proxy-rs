use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::accounts::{
    pool::{AccountPool, AccountPoolOptions, RotationStrategy},
    repository::AccountRepository,
};
use crate::config::AppConfig;
use crate::cookies::repository::CookieRepository;
use crate::crypto::SecretBox;
use crate::logs::repository::EventLogRepository;

#[derive(Clone)]
pub struct AppState {
    pub services: Arc<AppServices>,
}

pub struct AppServices {
    pub config: AppConfig,
    pub db: Option<SqlitePool>,
    pub event_logs: Option<EventLogRepository>,
    pub secret_box: Option<SecretBox>,
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

    pub fn cookie_repository(&self) -> Option<CookieRepository> {
        Some(CookieRepository::new(
            self.db()?.clone(),
            self.services.secret_box.clone()?,
        ))
    }

    pub fn account_pool(&self) -> Arc<Mutex<AccountPool>> {
        self.services.account_pool.clone()
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
