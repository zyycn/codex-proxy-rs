//! 模型计划快照存储。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::infra::redis::RedisConnection;

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

/// Redis 模型快照存储。
#[derive(Clone)]
pub struct RedisModelSnapshotStore {
    redis: RedisConnection,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPlanSnapshot {
    models: Vec<CodexModelInfo>,
    fetched_at: DateTime<Utc>,
}

impl RedisModelSnapshotStore {
    /// 使用给定连接池构造存储。
    pub fn new(redis: RedisConnection) -> Self {
        Self { redis }
    }
}

#[async_trait]
impl ModelSnapshotStore for RedisModelSnapshotStore {
    async fn replace_plan_snapshot(
        &self,
        snapshot: &ModelPlanSnapshot,
    ) -> ModelSnapshotStoreResult<()> {
        let value = serde_json::to_string(&StoredPlanSnapshot {
            models: snapshot.models.clone(),
            fetched_at: Utc::now(),
        })
        .map_err(store_error)?;
        let mut connection = self.redis.manager();
        let _: usize = connection
            .hset(
                self.redis.key("models:plan_snapshots"),
                &snapshot.plan_type,
                value,
            )
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn list_plan_snapshots(&self) -> ModelSnapshotStoreResult<Vec<ModelPlanSnapshot>> {
        let mut connection = self.redis.manager();
        let mut rows: Vec<(String, String)> = connection
            .hgetall(self.redis.key("models:plan_snapshots"))
            .await
            .map_err(store_error)?;
        rows.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let mut snapshots = Vec::with_capacity(rows.len());
        for (plan_type, value) in rows {
            let stored: StoredPlanSnapshot = serde_json::from_str(&value).map_err(store_error)?;
            snapshots.push(ModelPlanSnapshot {
                plan_type,
                models: stored.models,
            });
        }
        Ok(snapshots)
    }
}

fn store_error(error: impl std::fmt::Display) -> ModelSnapshotStoreError {
    ModelSnapshotStoreError::OperationFailed {
        message: error.to_string(),
    }
}
