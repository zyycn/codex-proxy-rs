//! 用量存储端口。

use async_trait::async_trait;
use thiserror::Error;

use crate::usage::model::UsageSnapshot;

/// 用量存储错误。
#[derive(Debug, Error)]
pub enum UsageStoreError {
    /// 底层存储失败。
    #[error("usage store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 用量存储结果。
pub type UsageStoreResult<T> = Result<T, UsageStoreError>;

/// 用量存储端口。
#[async_trait]
pub trait UsageStore: Send + Sync + 'static {
    /// 写入用量快照。
    async fn record_snapshot(&self, snapshot: &UsageSnapshot) -> UsageStoreResult<()>;
}
