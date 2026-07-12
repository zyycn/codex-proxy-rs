//! PostgreSQL 运维错误事实存储。

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use thiserror::Error;

use crate::{
    infra::json::{NumberedPage, page_offset},
    telemetry::{buckets::store::PgRequestBucketStore, ops::types::OpsErrorLog},
};

const OPS_ERROR_SELECT_SQL: &str = r"select
  id, request_id, client_api_key_id, kind, provider, account_id, route, model,
  status_code, client_status_code, upstream_status_code, transport, attempt_index,
  failure_class, response_id, upstream_request_id, latency_ms, message,
  metadata_json, created_at
from ops_error_logs";

#[derive(Debug, Error)]
pub enum PgOpsErrorLogStoreError {
    #[error("PostgreSQL ops error operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid ops error retention days: {0}")]
    InvalidRetention(i64),
}

pub type PgOpsErrorLogStoreResult<T> = Result<T, PgOpsErrorLogStoreError>;

#[derive(Clone)]
pub struct PgOpsErrorLogStore {
    pool: PgPool,
    buckets: PgRequestBucketStore,
}

impl PgOpsErrorLogStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            buckets: PgRequestBucketStore::new(pool.clone()),
            pool,
        }
    }

    pub async fn append(&self, event: &OpsErrorLog) -> PgOpsErrorLogStoreResult<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r"
insert into ops_error_logs (
  id, request_id, client_api_key_id, kind, provider, account_id, route, model,
  status_code, client_status_code, upstream_status_code, transport, attempt_index,
  failure_class, response_id, upstream_request_id, latency_ms, message,
  metadata_json, created_at
) values (
  $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
  $15, $16, $17, $18, $19, $20
)",
        )
        .bind(&event.id)
        .bind(&event.request_id)
        .bind(&event.client_api_key_id)
        .bind(&event.kind)
        .bind(&event.provider)
        .bind(&event.account_id)
        .bind(&event.route)
        .bind(&event.model)
        .bind(event.status_code)
        .bind(event.client_status_code)
        .bind(event.upstream_status_code)
        .bind(&event.transport)
        .bind(event.attempt_index)
        .bind(&event.failure_class)
        .bind(&event.response_id)
        .bind(&event.upstream_request_id)
        .bind(event.latency_ms)
        .bind(&event.message)
        .bind(sqlx::types::Json(&event.metadata))
        .bind(event.created_at)
        .execute(&mut *tx)
        .await?;
        self.buckets.upsert_error(&mut tx, event).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_page(
        &self,
        filter: OpsErrorFilter,
        page: u32,
        page_size: u32,
    ) -> PgOpsErrorLogStoreResult<NumberedPage<OpsErrorLog>> {
        let page_size = page_size.clamp(1, 200);
        let total = count_ops_errors(&self.pool, &filter).await?;
        let mut builder = QueryBuilder::<Postgres>::new(OPS_ERROR_SELECT_SQL);
        push_filter(&mut builder, &filter);
        builder.push(" order by created_at desc, id desc limit ");
        builder.push_bind(i64::from(page_size));
        builder.push(" offset ");
        builder.push_bind(page_offset(page, page_size).min(i64::MAX as u64) as i64);
        let items = builder
            .build()
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(ops_error_from_row)
            .collect();
        Ok(NumberedPage {
            items,
            total,
            page: page.max(1),
            page_size,
        })
    }

    pub async fn trim_to_retention(&self, now: DateTime<Utc>) -> PgOpsErrorLogStoreResult<u64> {
        let days: i64 = sqlx::query_scalar(
            "select ops_error_retention_days from runtime_settings where id = 1",
        )
        .fetch_one(&self.pool)
        .await?;
        let duration =
            Duration::try_days(days).ok_or(PgOpsErrorLogStoreError::InvalidRetention(days))?;
        let cutoff = now
            .checked_sub_signed(duration)
            .ok_or(PgOpsErrorLogStoreError::InvalidRetention(days))?;
        let result = sqlx::query("delete from ops_error_logs where created_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn clear(&self) -> PgOpsErrorLogStoreResult<u64> {
        let result = sqlx::query("delete from ops_error_logs")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn count(&self) -> PgOpsErrorLogStoreResult<u64> {
        let total: i64 = sqlx::query_scalar("select count(*) from ops_error_logs")
            .fetch_one(&self.pool)
            .await?;
        Ok(total.max(0) as u64)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpsErrorFilter {
    pub kind: Option<String>,
    pub client_api_key_id: Option<String>,
    pub provider: Option<String>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub client_status_code: Option<i64>,
    pub upstream_status_code: Option<i64>,
    pub transport: Option<String>,
    pub attempt_index: Option<i64>,
    pub failure_class: Option<String>,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub search: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
}

fn push_filter(builder: &mut QueryBuilder<Postgres>, filter: &OpsErrorFilter) {
    builder.push(" where true");
    push_optional_text(builder, "kind", filter.kind.as_deref());
    push_optional_text(
        builder,
        "client_api_key_id",
        filter.client_api_key_id.as_deref(),
    );
    push_optional_text(builder, "provider", filter.provider.as_deref());
    push_optional_text(builder, "request_id", filter.request_id.as_deref());
    push_optional_text(builder, "account_id", filter.account_id.as_deref());
    push_optional_text(builder, "route", filter.route.as_deref());
    push_optional_text(builder, "model", filter.model.as_deref());
    push_optional_i64(builder, "status_code", filter.status_code);
    push_optional_i64(builder, "client_status_code", filter.client_status_code);
    push_optional_i64(builder, "upstream_status_code", filter.upstream_status_code);
    push_optional_text(builder, "transport", filter.transport.as_deref());
    push_optional_i64(builder, "attempt_index", filter.attempt_index);
    push_optional_text(builder, "failure_class", filter.failure_class.as_deref());
    push_optional_text(builder, "response_id", filter.response_id.as_deref());
    push_optional_text(
        builder,
        "upstream_request_id",
        filter.upstream_request_id.as_deref(),
    );
    if let Some(search) = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        builder.push(" and (message ilike ");
        builder.push_bind(format!("%{search}%"));
        builder.push(" or request_id = ");
        builder.push_bind(search);
        builder.push(" or response_id = ");
        builder.push_bind(search);
        builder.push(" or upstream_request_id = ");
        builder.push_bind(search);
        builder.push(")");
    }
    if let Some(start_time) = filter.start_time {
        builder.push(" and created_at >= ");
        builder.push_bind(start_time);
    }
    if let Some(end_time) = filter.end_time {
        builder.push(" and created_at <= ");
        builder.push_bind(end_time);
    }
}

fn push_optional_text(builder: &mut QueryBuilder<Postgres>, column: &str, value: Option<&str>) {
    if let Some(value) = value {
        builder.push(" and ");
        builder.push(column);
        builder.push(" = ");
        builder.push_bind(value);
    }
}

fn push_optional_i64(builder: &mut QueryBuilder<Postgres>, column: &str, value: Option<i64>) {
    if let Some(value) = value {
        builder.push(" and ");
        builder.push(column);
        builder.push(" = ");
        builder.push_bind(value);
    }
}

async fn count_ops_errors(pool: &PgPool, filter: &OpsErrorFilter) -> PgOpsErrorLogStoreResult<u64> {
    let mut builder = QueryBuilder::<Postgres>::new("select count(*) from ops_error_logs");
    push_filter(&mut builder, filter);
    let total: i64 = builder.build_query_scalar().fetch_one(pool).await?;
    Ok(total.max(0) as u64)
}

fn ops_error_from_row(row: &sqlx::postgres::PgRow) -> OpsErrorLog {
    OpsErrorLog {
        id: row.get("id"),
        request_id: row.get("request_id"),
        client_api_key_id: row.get("client_api_key_id"),
        kind: row.get("kind"),
        provider: row.get("provider"),
        account_id: row.get("account_id"),
        route: row.get("route"),
        model: row.get("model"),
        status_code: row.get::<Option<i32>, _>("status_code").map(i64::from),
        client_status_code: row
            .get::<Option<i32>, _>("client_status_code")
            .map(i64::from),
        upstream_status_code: row
            .get::<Option<i32>, _>("upstream_status_code")
            .map(i64::from),
        transport: row.get("transport"),
        attempt_index: row.get("attempt_index"),
        failure_class: row.get("failure_class"),
        response_id: row.get("response_id"),
        upstream_request_id: row.get("upstream_request_id"),
        service_tier: None,
        latency_ms: row.get("latency_ms"),
        message: row.get("message"),
        metadata: row
            .get::<sqlx::types::Json<serde_json::Value>, _>("metadata_json")
            .0,
        created_at: row.get("created_at"),
    }
}
