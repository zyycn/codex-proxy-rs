use std::sync::Arc;

use sqlx::SqlitePool;

use crate::config::AppConfig;
use crate::logs::repository::EventLogRepository;

#[derive(Clone)]
pub struct AppState {
    pub services: Arc<AppServices>,
}

pub struct AppServices {
    pub config: AppConfig,
    pub db: Option<SqlitePool>,
    pub event_logs: Option<EventLogRepository>,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            services: Arc::new(AppServices {
                config,
                db: None,
                event_logs: None,
            }),
        }
    }

    pub fn with_pool(config: AppConfig, pool: SqlitePool) -> Self {
        Self {
            services: Arc::new(AppServices {
                config,
                db: Some(pool.clone()),
                event_logs: Some(EventLogRepository::new(pool)),
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
}
