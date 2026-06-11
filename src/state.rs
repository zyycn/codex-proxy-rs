use std::sync::Arc;

use crate::config::AppConfig;

#[derive(Clone)]
pub struct AppState {
    pub services: Arc<AppServices>,
}

pub struct AppServices {
    pub config: AppConfig,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            services: Arc::new(AppServices { config }),
        }
    }

    pub fn config(&self) -> &AppConfig {
        &self.services.config
    }
}
