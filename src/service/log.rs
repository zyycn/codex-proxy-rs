use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{
    config::LoggingConfig,
    logs::{event::EventLog, repository::EventLogRepository},
    utils::pagination::Page,
};

#[derive(Clone)]
pub struct LogService {
    config: Arc<RwLock<LoggingConfig>>,
    repository: Option<EventLogRepository>,
}

#[derive(Debug)]
pub enum LogServiceError {
    RepositoryUnavailable,
    List,
    Get,
    Count,
    Clear,
    InvalidCapacity,
}

#[derive(Debug)]
pub struct LogState {
    pub enabled: bool,
    pub capacity: u32,
    pub capture_body: bool,
    pub stored_count: u64,
}

#[derive(Debug)]
pub struct ClearLogs {
    pub cleared: u64,
}

#[derive(Debug, Default)]
pub struct LogStateUpdate {
    pub enabled: Option<bool>,
    pub capacity: Option<u32>,
    pub capture_body: Option<bool>,
}

impl LogService {
    pub fn new(config: LoggingConfig, repository: Option<EventLogRepository>) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            repository,
        }
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<EventLog>, LogServiceError> {
        self.repository()?
            .list(cursor, limit)
            .await
            .map_err(|_| LogServiceError::List)
    }

    pub async fn state(&self) -> Result<LogState, LogServiceError> {
        let stored_count = self
            .repository()?
            .count()
            .await
            .map_err(|_| LogServiceError::Count)?;
        let config = self.config.read().await;
        Ok(LogState {
            enabled: config.enabled,
            capacity: config.capacity,
            capture_body: config.capture_body,
            stored_count,
        })
    }

    pub async fn update_state(&self, update: LogStateUpdate) -> Result<LogState, LogServiceError> {
        if matches!(update.capacity, Some(0)) {
            return Err(LogServiceError::InvalidCapacity);
        }

        {
            let mut config = self.config.write().await;
            if let Some(enabled) = update.enabled {
                config.enabled = enabled;
            }
            if let Some(capacity) = update.capacity {
                config.capacity = capacity;
            }
            if let Some(capture_body) = update.capture_body {
                config.capture_body = capture_body;
            }
        }

        self.state().await
    }

    pub async fn get(&self, id: &str) -> Result<Option<EventLog>, LogServiceError> {
        self.repository()?
            .get(id)
            .await
            .map_err(|_| LogServiceError::Get)
    }

    pub async fn clear(&self) -> Result<ClearLogs, LogServiceError> {
        self.repository()?
            .clear()
            .await
            .map(|cleared| ClearLogs { cleared })
            .map_err(|_| LogServiceError::Clear)
    }

    fn repository(&self) -> Result<&EventLogRepository, LogServiceError> {
        self.repository
            .as_ref()
            .ok_or(LogServiceError::RepositoryUnavailable)
    }
}
