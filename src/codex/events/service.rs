use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{
    codex::events::{
        event::EventLog,
        repository::{EventLogFilters, EventLogRepository},
    },
    config::LoggingConfig,
    utils::pagination::Page,
};

#[derive(Clone)]
pub struct LogService {
    config: Arc<RwLock<LoggingConfig>>,
    repository: Option<EventLogRepository>,
}

#[derive(Debug, thiserror::Error)]
pub enum LogServiceError {
    #[error("事件日志仓储未初始化")]
    RepositoryUnavailable,
    #[error("查询事件日志失败")]
    List,
    #[error("读取事件日志失败")]
    Get,
    #[error("统计事件日志失败")]
    Count,
    #[error("清空事件日志失败")]
    Clear,
    #[error("写入事件日志失败")]
    Write,
    #[error("日志容量必须大于 0")]
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

#[derive(Debug, Default)]
pub struct LogListFilter {
    pub kind: Option<String>,
    pub level: Option<String>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub search: Option<String>,
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
        filter: LogListFilter,
    ) -> Result<Page<EventLog>, LogServiceError> {
        self.repository()?
            .list_filtered(EventLogFilters::from(filter), cursor, limit)
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
        if let Some(capacity) = update.capacity {
            self.repository()?
                .trim_to_capacity(capacity)
                .await
                .map_err(|_| LogServiceError::Write)?;
        }

        self.state().await
    }

    pub async fn record(&self, mut event: EventLog) -> Result<bool, LogServiceError> {
        let config = self.config.read().await.clone();
        if !config.enabled {
            return Ok(false);
        }
        apply_capture_body_policy(&mut event, config.capture_body);
        let repository = self.repository()?;
        repository
            .insert(event)
            .await
            .map_err(|_| LogServiceError::Write)?;
        repository
            .trim_to_capacity(config.capacity)
            .await
            .map_err(|_| LogServiceError::Write)?;
        Ok(true)
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

fn apply_capture_body_policy(event: &mut EventLog, capture_body: bool) {
    if capture_body {
        return;
    }
    let Some(metadata) = event.metadata.as_object_mut() else {
        return;
    };
    for key in [
        "body",
        "rawBody",
        "requestBody",
        "responseBody",
        "upstreamBody",
    ] {
        metadata.remove(key);
    }
}

impl From<LogListFilter> for EventLogFilters {
    fn from(filter: LogListFilter) -> Self {
        Self {
            kind: filter.kind,
            level: filter.level,
            request_id: filter.request_id,
            account_id: filter.account_id,
            route: filter.route,
            model: filter.model,
            status_code: filter.status_code,
            search: filter.search,
        }
    }
}
