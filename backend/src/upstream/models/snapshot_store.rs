//! 模型计划快照存储。

use async_trait::async_trait;
use sqlx::Row;
use thiserror::Error;

use super::{info::CodexModelInfo, snapshot::ModelPlanSnapshot};

/// 模型快照存储错误。
#[derive(Debug, Error, PartialEq, Eq)]
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

/// SQLite 模型快照存储。
#[derive(Clone)]
pub struct SqliteModelSnapshotStore {
    pool: sqlx::SqlitePool,
}

impl SqliteModelSnapshotStore {
    /// 使用给定连接池构造存储。
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ModelSnapshotStore for SqliteModelSnapshotStore {
    async fn replace_plan_snapshot(
        &self,
        snapshot: &ModelPlanSnapshot,
    ) -> ModelSnapshotStoreResult<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let models_json = serde_json::to_string(&snapshot.models).map_err(|e| {
            ModelSnapshotStoreError::OperationFailed {
                message: e.to_string(),
            }
        })?;
        sqlx::query(
            "insert or replace into model_plan_snapshots (plan_type, models_json, fetched_at) values (?, ?, ?)",
        )
        .bind(&snapshot.plan_type)
        .bind(&models_json)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| ModelSnapshotStoreError::OperationFailed {
            message: e.to_string(),
        })?;
        Ok(())
    }

    async fn list_plan_snapshots(&self) -> ModelSnapshotStoreResult<Vec<ModelPlanSnapshot>> {
        let rows = sqlx::query("select plan_type, models_json from model_plan_snapshots")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ModelSnapshotStoreError::OperationFailed {
                message: e.to_string(),
            })?;
        let mut snapshots = Vec::new();
        for row in rows {
            let plan_type: String = row.get("plan_type");
            let models_json: String = row.get("models_json");
            let models: Vec<CodexModelInfo> = serde_json::from_str(&models_json).map_err(|e| {
                ModelSnapshotStoreError::OperationFailed {
                    message: e.to_string(),
                }
            })?;
            snapshots.push(ModelPlanSnapshot { plan_type, models });
        }
        Ok(snapshots)
    }
}
