use crate::{
    config::LoggingConfig,
    logs::{event::EventLog, repository::EventLogRepository},
    utils::pagination::Page,
};

#[derive(Clone)]
pub struct LogService {
    config: LoggingConfig,
    repository: Option<EventLogRepository>,
}

#[derive(Debug)]
pub enum LogServiceError {
    RepositoryUnavailable,
    List,
    Get,
    Count,
    Clear,
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

impl LogService {
    pub fn new(config: LoggingConfig, repository: Option<EventLogRepository>) -> Self {
        Self { config, repository }
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
        Ok(LogState {
            enabled: self.config.enabled,
            capacity: self.config.capacity,
            capture_body: self.config.capture_body,
            stored_count,
        })
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
