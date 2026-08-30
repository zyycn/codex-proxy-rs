//! 终态请求、后台事件和审计事件的保留期清理。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::time::{Duration, Instant};

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
    pub batches: u32,
    pub budget_exhausted: bool,
}

/// 一次 retention cycle 可占用的删除预算；不从部署配置暴露低价值开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionCycleBudget {
    batch_rows: u32,
    max_batches: u32,
    max_duration: Duration,
    batch_pause: Duration,
}

impl RetentionCycleBudget {
    pub fn try_new(
        batch_rows: u32,
        max_batches: u32,
        max_duration: Duration,
        batch_pause: Duration,
    ) -> StoreResult<Self> {
        if batch_rows == 0 || max_batches == 0 || max_duration.is_zero() {
            return Err(StoreError::InvalidData {
                entity: "retention cycle budget",
                message: "row, batch, and duration budgets must be positive".to_owned(),
            });
        }
        Ok(Self {
            batch_rows,
            max_batches,
            max_duration,
            batch_pause,
        })
    }

    const fn production() -> Self {
        Self {
            batch_rows: 5_000,
            max_batches: 12,
            max_duration: Duration::from_secs(30),
            batch_pause: Duration::from_millis(50),
        }
    }
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
    cycle_budget: RetentionCycleBudget,
}

impl PgRetentionRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pool,
            cycle_budget: RetentionCycleBudget::production(),
        }
    }

    #[must_use]
    pub const fn with_cycle_budget(pool: PgPool, cycle_budget: RetentionCycleBudget) -> Self {
        Self { pool, cycle_budget }
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

        // 各表轮转执行单批独立事务；行数、批数和 wall-clock 同时有界。
        let mut targets = [
            RetentionTarget::new(
                "delete from model_requests
                 where ctid in (
                   select ctid from model_requests
                    where outcome <> 'running'
                      and completed_at < $1 - ($2 * interval '1 day')
                    limit $3
                 )",
                settings.usage_retention_days,
                "delete expired model requests",
            ),
            RetentionTarget::new(
                "delete from ops_events
                 where ctid in (
                   select ctid from ops_events
                    where model_request_id is null
                      and created_at < $1 - ($2 * interval '1 day')
                    limit $3
                 )",
                settings.ops_event_retention_days,
                "delete expired ops events",
            ),
            RetentionTarget::new(
                "delete from admin_audit_events
                 where ctid in (
                   select ctid from admin_audit_events
                    where created_at < $1 - ($2 * interval '1 day')
                    limit $3
                 )",
                settings.audit_retention_days,
                "delete expired admin audit events",
            ),
        ];
        let started_at = Instant::now();
        let mut batches = 0_u32;
        'cycles: loop {
            let mut attempted = false;
            for target in &mut targets {
                if target.complete {
                    continue;
                }
                if batches >= self.cycle_budget.max_batches
                    || started_at.elapsed() >= self.cycle_budget.max_duration
                {
                    break 'cycles;
                }
                attempted = true;
                let deleted =
                    purge_batch(&self.pool, target, now, self.cycle_budget.batch_rows).await?;
                batches = batches.saturating_add(1);
                target.deleted = target.deleted.saturating_add(deleted);
                target.complete = deleted < u64::from(self.cycle_budget.batch_rows);
                if !target.complete
                    && batches < self.cycle_budget.max_batches
                    && started_at.elapsed() < self.cycle_budget.max_duration
                    && !self.cycle_budget.batch_pause.is_zero()
                {
                    tokio::time::sleep(self.cycle_budget.batch_pause).await;
                }
            }
            if !attempted || targets.iter().all(|target| target.complete) {
                break;
            }
        }
        Ok(RetentionReport {
            model_requests: targets[0].deleted,
            ops_events: targets[1].deleted,
            admin_audit_events: targets[2].deleted,
            batches,
            budget_exhausted: targets.iter().any(|target| !target.complete),
        })
    }
}

struct RetentionTarget {
    delete_sql: &'static str,
    retention_days: u32,
    label: &'static str,
    deleted: u64,
    complete: bool,
}

impl RetentionTarget {
    const fn new(delete_sql: &'static str, retention_days: u32, label: &'static str) -> Self {
        Self {
            delete_sql,
            retention_days,
            label,
            deleted: 0,
            complete: false,
        }
    }
}

async fn purge_batch(
    pool: &PgPool,
    target: &RetentionTarget,
    now: DateTime<Utc>,
    batch_rows: u32,
) -> StoreResult<u64> {
    sqlx::query(target.delete_sql)
        .bind(now)
        .bind(i64::from(target.retention_days))
        .bind(i64::from(batch_rows))
        .execute(pool)
        .await
        .map_err(|_| postgres_unavailable(target.label))
        .map(|result| result.rows_affected())
}

fn to_u32(value: i64) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| StoreError::InvalidData {
        entity: "retention settings",
        message: "retention days are outside the supported range".to_owned(),
    })
}
