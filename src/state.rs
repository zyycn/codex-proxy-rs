use std::sync::Arc;

use sqlx::SqlitePool;

use crate::accounts::repository::AccountRepository;
use crate::config::AppConfig;
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
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            services: Arc::new(AppServices {
                config,
                db: None,
                event_logs: None,
                secret_box: None,
            }),
        }
    }

    pub fn with_pool(config: AppConfig, pool: SqlitePool) -> Self {
        Self {
            services: Arc::new(AppServices {
                config,
                db: Some(pool.clone()),
                event_logs: Some(EventLogRepository::new(pool)),
                secret_box: None,
            }),
        }
    }

    pub fn with_pool_and_secret_box(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
    ) -> Self {
        Self {
            services: Arc::new(AppServices {
                config,
                db: Some(pool.clone()),
                event_logs: Some(EventLogRepository::new(pool)),
                secret_box: Some(secret_box),
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
}
