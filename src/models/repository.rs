use chrono::Utc;
use sqlx::{Row, SqlitePool};
use thiserror::Error;

use crate::models::catalog::{CodexModelInfo, ModelPlanSnapshot};

#[derive(Debug, Error)]
pub enum ModelSnapshotRepositoryError {
    #[error("model snapshot database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("model snapshot json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type ModelSnapshotRepositoryResult<T> = Result<T, ModelSnapshotRepositoryError>;

#[derive(Debug, Clone)]
pub struct ModelSnapshotRepository {
    pool: SqlitePool,
}

impl ModelSnapshotRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn replace_plan_snapshot(
        &self,
        snapshot: &ModelPlanSnapshot,
    ) -> ModelSnapshotRepositoryResult<()> {
        let models_json = serde_json::to_string(&snapshot.models)?;
        let fetched_at = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into model_plan_snapshots (plan_type, models_json, fetched_at) values (?, ?, ?) \
             on conflict(plan_type) do update set models_json = excluded.models_json, fetched_at = excluded.fetched_at",
        )
        .bind(&snapshot.plan_type)
        .bind(models_json)
        .bind(fetched_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_plan_snapshots(
        &self,
    ) -> ModelSnapshotRepositoryResult<Vec<ModelPlanSnapshot>> {
        let rows = sqlx::query(
            "select plan_type, models_json from model_plan_snapshots order by plan_type asc",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let plan_type: String = row.get("plan_type");
                let models_json: String = row.get("models_json");
                let models = serde_json::from_str::<Vec<CodexModelInfo>>(&models_json)?;
                Ok(ModelPlanSnapshot { plan_type, models })
            })
            .collect()
    }
}
