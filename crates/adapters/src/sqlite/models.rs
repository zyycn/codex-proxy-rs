//! 模型快照 SQLite 仓储。

use async_trait::async_trait;
use chrono::Utc;
use sqlx::{Row, SqlitePool};
use thiserror::Error;

use codex_proxy_core::models::{
    model::ModelPlanSnapshot,
    ports::{ModelSnapshotStore, ModelSnapshotStoreError, ModelSnapshotStoreResult},
};

/// 模型快照仓储实现。
#[derive(Debug, Clone)]
pub struct ModelSnapshotRepository {
    pool: SqlitePool,
}

/// 模型快照仓储错误。
#[derive(Debug, Error)]
pub enum ModelSnapshotRepositoryError {
    /// 存储层错误。
    #[error("model snapshot store error: {0}")]
    Store(#[from] ModelSnapshotStoreError),
    /// 数据库错误。
    #[error("model snapshot database error: {0}")]
    Database(#[from] sqlx::Error),
    /// JSON 错误。
    #[error("model snapshot json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl ModelSnapshotRepository {
    /// 构造仓储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 直接写入单个计划快照。
    pub async fn replace_plan_snapshot(
        &self,
        snapshot: &ModelPlanSnapshot,
    ) -> ModelSnapshotStoreResult<()> {
        let models_json = serde_json::to_string(&snapshot.models).map_err(|error| {
            ModelSnapshotStoreError::OperationFailed {
                message: error.to_string(),
            }
        })?;
        let fetched_at = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into model_plan_snapshots (plan_type, models_json, fetched_at) values (?, ?, ?) \
             on conflict(plan_type) do update set models_json = excluded.models_json, fetched_at = excluded.fetched_at",
        )
        .bind(&snapshot.plan_type)
        .bind(models_json)
        .bind(fetched_at)
        .execute(&self.pool)
        .await
        .map_err(|error| ModelSnapshotStoreError::OperationFailed {
            message: error.to_string(),
        })?;
        Ok(())
    }

    /// 列出所有快照。
    pub async fn list_plan_snapshots(&self) -> ModelSnapshotStoreResult<Vec<ModelPlanSnapshot>> {
        let rows = sqlx::query(
            "select plan_type, models_json from model_plan_snapshots order by plan_type asc",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| ModelSnapshotStoreError::OperationFailed {
            message: error.to_string(),
        })?;
        rows.into_iter()
            .map(|row| {
                let plan_type: String = row.get("plan_type");
                let models_json: String = row.get("models_json");
                let models = serde_json::from_str(&models_json).map_err(|error| {
                    ModelSnapshotStoreError::OperationFailed {
                        message: error.to_string(),
                    }
                })?;
                Ok(ModelPlanSnapshot { plan_type, models })
            })
            .collect()
    }
}

#[async_trait]
impl ModelSnapshotStore for ModelSnapshotRepository {
    async fn replace_plan_snapshot(
        &self,
        snapshot: &ModelPlanSnapshot,
    ) -> ModelSnapshotStoreResult<()> {
        Self::replace_plan_snapshot(self, snapshot).await
    }

    async fn list_plan_snapshots(&self) -> ModelSnapshotStoreResult<Vec<ModelPlanSnapshot>> {
        Self::list_plan_snapshots(self).await
    }
}
