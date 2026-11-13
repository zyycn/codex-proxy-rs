//! PostgreSQL 保留期投影与单批删除适配器

use crate::{admin_store_error, postgres_unavailable};
use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::{
    model::retention::{RetentionPolicy, RetentionTarget},
    ports::{
        retention::RetentionStore,
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use sqlx::PgPool;
use std::num::NonZeroU32;

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
impl RetentionStore for PgRetentionRepository {
    async fn load_policy(&self) -> AdminStoreResult<RetentionPolicy> {
        let row = sqlx::query_as::<_, (i64, i64, i64)>(
            "select usage_retention_days, ops_event_retention_days, audit_retention_days
             from runtime_settings where id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| unavailable("load retention settings", source))?
        .ok_or_else(|| {
            AdminStoreError::new(
                AdminStoreErrorKind::NotFound,
                "retention settings",
                "runtime settings not found",
            )
        })?;
        let to_days = |value| u32::try_from(value).map_err(|_| invalid_policy());
        RetentionPolicy::try_new(to_days(row.0)?, to_days(row.1)?, to_days(row.2)?)
            .map_err(|_| invalid_policy())
    }

    async fn purge_batch(
        &self,
        target: RetentionTarget,
        now: DateTime<Utc>,
        policy: RetentionPolicy,
        limit: NonZeroU32,
    ) -> AdminStoreResult<u64> {
        let oldest_sql = match target {
            RetentionTarget::ModelRequests => {
                "select completed_at from model_requests where outcome <> 'running' order by completed_at limit 1"
            }
            RetentionTarget::OpsEvents => {
                "select created_at from ops_events where model_request_id is null order by created_at limit 1"
            }
            RetentionTarget::AdminAuditEvents => {
                "select created_at from admin_audit_events order by created_at limit 1"
            }
        };
        let sql = match target {
            RetentionTarget::ModelRequests => {
                "delete from model_requests
                 where ctid in (
                   select ctid from model_requests
                    where outcome <> 'running'
                      and completed_at < $1
                    order by completed_at
                    limit $2
                    for update skip locked
                 )"
            }
            RetentionTarget::OpsEvents => {
                "delete from ops_events
                 where ctid in (
                   select ctid from ops_events
                    where model_request_id is null
                      and created_at < $1
                    order by created_at
                    limit $2
                    for update skip locked
                 )"
            }
            RetentionTarget::AdminAuditEvents => {
                "delete from admin_audit_events
                 where ctid in (
                   select ctid from admin_audit_events
                    where created_at < $1
                    order by created_at
                    limit $2
                    for update skip locked
                 )"
            }
        };
        // 跳过正在写入的父行；级联与投影触发器仍可能等锁，单批需有服务端截止时间
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| unavailable("begin retention batch", source))?;
        sqlx::query("set local statement_timeout = '5s'")
            .execute(&mut *transaction)
            .await
            .map_err(|source| unavailable("set retention statement timeout", source))?;
        sqlx::query("set local lock_timeout = '500ms'")
            .execute(&mut *transaction)
            .await
            .map_err(|source| unavailable("set retention lock timeout", source))?;
        let oldest: Option<DateTime<Utc>> = sqlx::query_scalar(oldest_sql)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| unavailable("read oldest retained record", source))?;
        // 超长窗口覆盖全部现存记录时无需删除，也不向数据库传递越界日期
        let cutoff = now
            .checked_sub_signed(TimeDelta::days(i64::from(policy.days(target))))
            .filter(|cutoff| oldest.is_some_and(|oldest| oldest < *cutoff));
        let deleted = if let Some(cutoff) = cutoff {
            sqlx::query(sql)
                .bind(cutoff)
                .bind(i64::from(limit.get()))
                .execute(&mut *transaction)
                .await
                .map(|result| result.rows_affected())
                .map_err(|source| unavailable("delete expired records", source))?
        } else {
            0
        };
        transaction
            .commit()
            .await
            .map_err(|source| unavailable("commit retention batch", source))?;
        Ok(deleted)
    }
}

fn unavailable(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> AdminStoreError {
    admin_store_error("retention", postgres_unavailable(operation, source))
}

fn invalid_policy() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Invalid,
        "retention settings",
        "invalid retention policy",
    )
}
