use crate::{
    logs::{event::EventLog, repository::EventLogRepository},
    utils::pagination::Page,
};

#[derive(Clone)]
pub struct LogService {
    repository: Option<EventLogRepository>,
}

#[derive(Debug)]
pub enum LogServiceError {
    RepositoryUnavailable,
    List,
}

impl LogService {
    pub fn new(repository: Option<EventLogRepository>) -> Self {
        Self { repository }
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

    fn repository(&self) -> Result<&EventLogRepository, LogServiceError> {
        self.repository
            .as_ref()
            .ok_or(LogServiceError::RepositoryUnavailable)
    }
}
