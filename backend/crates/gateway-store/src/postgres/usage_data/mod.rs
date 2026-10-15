//! 复用请求明细和聚合存储的有界维护入口，不管理插件工作流

mod aggregates;
mod records;
pub(crate) use aggregates::AGGREGATE_COLLECTION;

use super::{ObservabilityConnection, ObservabilityQueryBudget};
use crate::{StoreError, StoreResult, postgres_unavailable};
use chrono::{DateTime, Utc};
use gateway_admin::{model::usage_data::*, ports::usage_data::*};
use sqlx::{Connection as _, PgPool, Postgres, Row, Transaction};

#[derive(Clone)]
pub struct PgUsageDataStore {
    pool: PgPool,
    budget: ObservabilityQueryBudget,
}
impl PgUsageDataStore {
    #[must_use]
    pub const fn new(pool: PgPool, budget: ObservabilityQueryBudget) -> Self {
        Self { pool, budget }
    }
}

#[async_trait::async_trait]
impl UsageDataStore for PgUsageDataStore {
    async fn get_record(&self, request: GetUsageRecord) -> UsageDataResult<UsageRecord> {
        self.budget
            .run(&self.pool, "usage record", |mut connection| async move {
                let mut tx = begin(&mut connection).await?;
                records::get(&mut tx, &request.id).await
            })
            .await
            .map_err(public_error)
    }
    async fn list_records(&self, request: ListUsageRecords) -> UsageDataResult<UsageRecordPage> {
        self.budget
            .run(&self.pool, "usage records", |mut connection| async move {
                let mut tx = begin(&mut connection).await?;
                records::list(&mut tx, request).await
            })
            .await
            .map_err(public_error)
    }
    async fn write_records(&self, command: StoredUsageWrite) -> UsageDataResult<UsageWriteResult> {
        self.budget
            .run(
                &self.pool,
                "write usage records",
                |mut connection| async move {
                    let mut tx = begin(&mut connection).await?;
                    let mut changed_ids = Vec::with_capacity(command.changes.len());
                    for (change, event) in command.changes {
                        if records::write(&mut tx, &change).await? {
                            changed_ids.push(change.id().to_owned());
                        }
                        audit(&mut tx, event).await?;
                    }
                    tx.commit().await.map_err(db_error)?;
                    Ok(UsageWriteResult { changed_ids })
                },
            )
            .await
            .map_err(public_error)
    }
    async fn list_aggregates(
        &self,
        request: ListUsageAggregates,
    ) -> UsageDataResult<UsageAggregatePage> {
        self.budget
            .run(
                &self.pool,
                "usage aggregates",
                |mut connection| async move {
                    let mut tx = begin(&mut connection).await?;
                    aggregates::list(&mut tx, request).await
                },
            )
            .await
            .map_err(public_error)
    }
    async fn write_aggregates(
        &self,
        command: StoredAggregateWrite,
    ) -> UsageDataResult<UsageAggregateWriteResult> {
        self.budget.run(&self.pool, "write usage aggregates", |mut connection| async move {
            let mut tx = begin(&mut connection).await?;
            let mut changed=Vec::with_capacity(command.changes.len());
            for (change,event) in command.changes {
                changed.push(aggregates::write(&mut tx,change).await?);
                audit(&mut tx,event).await?;
            }
            // 沿用现有统计发布代次，让读缓存能观察到直接维护的聚合值
            sqlx::query("update request_analytics_build_state set generation=generation+1,updated_at=clock_timestamp() where singleton").execute(&mut *tx).await.map_err(db_error)?;
            tx.commit().await.map_err(db_error)?;
            Ok(UsageAggregateWriteResult { changed })
        }).await.map_err(public_error)
    }
}

async fn begin(connection: &mut ObservabilityConnection) -> StoreResult<Transaction<'_, Postgres>> {
    let timeout = connection.query_timeout();
    let mut tx = connection.begin().await.map_err(db_error)?;
    sqlx::query(
        "select set_config('statement_timeout',$1,true),set_config('lock_timeout',$1,true)",
    )
    .bind(format!("{}ms", timeout.as_millis()))
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    Ok(tx)
}
fn at(ms: i64) -> StoreResult<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms).ok_or_else(|| invalid("invalid usage time"))
}
fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "usage data",
        message: message.into(),
        source: None,
    }
}
fn db_error(error: sqlx::Error) -> StoreError {
    if error
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|code| code.starts_with("22") || code.starts_with("23"))
    {
        return invalid("usage value violates storage constraints").with_source(error);
    }
    postgres_unavailable("usage storage operation", error)
}
fn public_error(error: StoreError) -> UsageDataError {
    let kind = match error {
        StoreError::InvalidData { .. } => UsageErrorKind::InvalidInput,
        StoreError::NotFound { .. } => UsageErrorKind::NotFound,
        StoreError::Conflict { .. } => UsageErrorKind::Conflict,
        StoreError::Unavailable { .. } => UsageErrorKind::Unavailable,
    };
    tracing::debug!(%error,"usage operation failed");
    UsageDataError::new(
        kind,
        match kind {
            UsageErrorKind::InvalidInput => "数据不符合存储格式或数值范围",
            UsageErrorKind::NotFound => "记录不存在",
            UsageErrorKind::Conflict => "请求仍在运行，无法维护",
            _ => "存储操作失败或超出资源预算",
        },
    )
}
async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    event: gateway_admin::model::auth::AdminAuditEvent,
) -> StoreResult<()> {
    super::append_admin_audit_event_in_transaction(
        tx,
        super::AdminAuditEvent {
            id: event.id,
            actor_kind: event.actor_kind.into(),
            actor_admin_user_id: event.actor_admin_user_id,
            actor_ref: event.actor_ref,
            admin_request_id: event.request_id,
            action: event.action,
            entity_kind: event.entity_kind,
            entity_ref: event.entity_ref,
            config_revision: None,
            changed_fields: event.changed_fields,
            created_at: event.occurred_at,
        },
        None,
    )
    .await
}
