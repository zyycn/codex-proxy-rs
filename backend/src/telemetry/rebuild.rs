//! `request_time_buckets` 可重建范围重算。

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use thiserror::Error;

use crate::telemetry::buckets::store::{PgRequestBucketStore, PgRequestBucketStoreError};

/// 桶重算结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildBucketsReport {
    pub cutoff: DateTime<Utc>,
    pub deleted_rows: u64,
    pub rebuilt_rows: u64,
}

/// 桶重算错误。
#[derive(Debug, Error)]
pub enum RebuildBucketsError {
    #[error("failed to rebuild request time buckets: {0}")]
    Store(#[from] PgRequestBucketStoreError),
}

/// 删除两类事实都仍在保留期内的桶，并从成功/失败事实重新聚合。
pub async fn rebuild_buckets(pool: &PgPool) -> Result<RebuildBucketsReport, RebuildBucketsError> {
    let rebuilt = PgRequestBucketStore::new(pool.clone())
        .rebuild_reconstructible_range()
        .await?;
    Ok(RebuildBucketsReport {
        cutoff: rebuilt.cutoff,
        deleted_rows: rebuilt.deleted_rows,
        rebuilt_rows: rebuilt.rebuilt_rows,
    })
}
