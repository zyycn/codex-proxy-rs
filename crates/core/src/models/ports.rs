//! 模型快照存储端口。

use async_trait::async_trait;
use thiserror::Error;

use crate::models::model::ModelPlanSnapshot;

/// 模型快照存储错误。
#[derive(Debug, Error)]
pub enum ModelSnapshotStoreError {
    /// 底层存储失败。
    #[error("model snapshot store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 模型快照存储结果类型。
pub type ModelSnapshotStoreResult<T> = Result<T, ModelSnapshotStoreError>;

/// 模型快照存储端口。
#[async_trait]
pub trait ModelSnapshotStore: Send + Sync + 'static {
    /// 用单个计划快照替换同名快照。
    async fn replace_plan_snapshot(
        &self,
        snapshot: &ModelPlanSnapshot,
    ) -> ModelSnapshotStoreResult<()>;

    /// 列出所有计划快照。
    async fn list_plan_snapshots(&self) -> ModelSnapshotStoreResult<Vec<ModelPlanSnapshot>>;
}
