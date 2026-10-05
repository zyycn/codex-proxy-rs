//! PostgreSQL 保留期投影与单批删除适配器

use crate::{admin_store_error, postgres_unavailable};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
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
        .map_err(|_| unavailable("load retention settings"))?
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
        let sql = match target {
            RetentionTarget::ModelRequests => {
                "delete from model_requests
                 where ctid in (
                   select ctid from model_requests
                    where outcome <> 'running'
                      and completed_at < $1 - ($2 * interval '1 day')
                    limit $3
                 )"
            }
            RetentionTarget::OpsEvents => {
                "delete from ops_events
                 where ctid in (
                   select ctid from ops_events
                    where model_request_id is null
                      and created_at < $1 - ($2 * interval '1 day')
                    limit $3
                 )"
            }
            RetentionTarget::AdminAuditEvents => {
                "delete from admin_audit_events
                 where ctid in (
                   select ctid from admin_audit_events
                    where created_at < $1 - ($2 * interval '1 day')
                    limit $3
                 )"
            }
        };
        sqlx::query(sql)
            .bind(now)
            .bind(i64::from(policy.days(target)))
            .bind(i64::from(limit.get()))
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
            .map_err(|_| unavailable("delete expired records"))
    }
}

fn unavailable(operation: &'static str) -> AdminStoreError {
    admin_store_error("retention", postgres_unavailable(operation))
}

fn invalid_policy() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Invalid,
        "retention settings",
        "invalid retention policy",
    )
}
