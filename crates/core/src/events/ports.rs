//! 事件日志端口。

use async_trait::async_trait;
use thiserror::Error;

use crate::events::model::EventLog;

/// 事件日志存储错误。
#[derive(Debug, Error)]
pub enum EventLogStoreError {
    /// 底层存储失败。
    #[error("event log store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 事件日志存储结果。
pub type EventLogStoreResult<T> = Result<T, EventLogStoreError>;

/// 事件日志存储端口。
#[async_trait]
pub trait EventLogStore: Send + Sync + 'static {
    /// 写入事件日志。
    async fn append(&self, event: &EventLog) -> EventLogStoreResult<()>;
}
