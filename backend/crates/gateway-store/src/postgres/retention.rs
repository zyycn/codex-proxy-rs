//! 终态请求、后台事件和审计事件的保留期清理。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{StoreError, StoreResult, postgres_unavailable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeRetentionSettings {
    pub usage_retention_days: u32,
    pub ops_event_retention_days: u32,
    pub audit_retention_days: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionReport {
    pub model_requests: u64,
    pub ops_events: u64,
    pub admin_audit_events: u64,
}

#[async_trait]
pub trait RetentionRepository: Send + Sync {
    async fn load_retention_settings(&self) -> StoreResult<RuntimeRetentionSettings>;
    async fn apply_retention(
        &self,
        now: DateTime<Utc>,
        settings: RuntimeRetentionSettings,
    ) -> StoreResult<RetentionReport>;
}

#[derive(Clone)]
pub struct PgRetentionRepository {
    pool: PgPool,
}

impl PgRetentionRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RetentionRepository for PgRetentionRepository {
    async fn load_retention_settings(&self) -> StoreResult<RuntimeRetentionSettings> {
        let row = sqlx::query_as::<_, (i64, i64, i64)>(
            "select usage_retention_days, ops_event_retention_days, audit_retention_days
             from runtime_settings where id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("load retention settings"))?
        .ok_or_else(|| StoreError::NotFound {
            entity: "runtime settings",
            id: "1".to_owned(),
        })?;
        Ok(RuntimeRetentionSettings {
            usage_retention_days: to_u32(row.0)?,
            ops_event_retention_days: to_u32(row.1)?,
            audit_retention_days: to_u32(row.2)?,
        })
    }

    async fn apply_retention(
        &self,
        now: DateTime<Utc>,
        settings: RuntimeRetentionSettings,
    ) -> StoreResult<RetentionReport> {
        if settings.usage_retention_days < 31
            || settings.ops_event_retention_days == 0
            || settings.audit_retention_days == 0
        {
            return Err(StoreError::InvalidData {
                entity: "retention settings",
                message: "retention values violate the frozen constraints".to_owned(),
            });
        }

        // 分批删除，每批独立提交：清理是幂等的，无需跨表原子性；
        // 有界批量避免百万行单事务的长锁与 WAL 峰值。
        let model_requests = purge_in_batches(
            &self.pool,
            "delete from model_requests
             where ctid in (
               select ctid from model_requests
                where outcome <> 'running'
                  and completed_at < $1 - ($2 * interval '1 day')
                limit $3
             )",
            now,
            settings.usage_retention_days,
            "delete expired model requests",
        )
        .await?;
        let ops_events = purge_in_batches(
            &self.pool,
            "delete from ops_events
             where ctid in (
               select ctid from ops_events
                where model_request_id is null
                  and created_at < $1 - ($2 * interval '1 day')
                limit $3
             )",
            now,
            settings.ops_event_retention_days,
            "delete expired ops events",
        )
        .await?;
        let admin_audit_events = purge_in_batches(
            &self.pool,
            "delete from admin_audit_events
             where ctid in (
               select ctid from admin_audit_events
                where created_at < $1 - ($2 * interval '1 day')
                limit $3
             )",
            now,
            settings.audit_retention_days,
            "delete expired admin audit events",
        )
        .await?;
        Ok(RetentionReport {
            model_requests,
            ops_events,
            admin_audit_events,
        })
    }
}

const RETENTION_DELETE_BATCH_ROWS: i64 = 10_000;

async fn purge_in_batches(
    pool: &PgPool,
    delete_sql: &'static str,
    now: DateTime<Utc>,
    retention_days: u32,
    label: &'static str,
) -> StoreResult<u64> {
    let mut total = 0u64;
    loop {
        let deleted = sqlx::query(delete_sql)
            .bind(now)
            .bind(i64::from(retention_days))
            .bind(RETENTION_DELETE_BATCH_ROWS)
            .execute(pool)
            .await
            .map_err(|_| postgres_unavailable(label))?
            .rows_affected();
        total += deleted;
        if deleted < RETENTION_DELETE_BATCH_ROWS.unsigned_abs() {
            return Ok(total);
        }
    }
}

fn to_u32(value: i64) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| StoreError::InvalidData {
        entity: "retention settings",
        message: "retention days are outside the supported range".to_owned(),
    })
}
