//! PostgreSQL 成功使用事实存储。

use super::query::{
    count_usage_records, usage_account_usage, usage_breakdown, usage_record_account_email_map,
    usage_record_from_row, usage_summary, usage_trend, UsageRecordAccountUsage,
    UsageRecordBreakdown, UsageRecordEndpointSource, UsageRecordModelSource, UsageRecordSummary,
    UsageRecordTrendPoint,
};

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};
use thiserror::Error;

use crate::{
    infra::json::{page_offset, NumberedPage},
    telemetry::{buckets::store::PgRequestBucketStore, usage::types::UsageRecord},
};

pub const USAGE_RECORD_RETENTION_DAYS: i64 = 30;
pub const DEFAULT_USAGE_RECORD_CAPTURE_BODY: bool = false;

const USAGE_RECORD_SELECT_SQL: &str = r"
select
  id, request_id, client_api_key_id, kind, route, provider, account_id, model,
  requested_model, upstream_model, service_tier, status_code, transport, attempt_index,
  response_id, upstream_request_id, latency_ms, first_token_ms, input_tokens,
  output_tokens, cached_tokens, reasoning_tokens, message, metadata_json, created_at
from usage_records";

#[derive(Debug, Error)]
pub enum PgUsageRecordStoreError {
    #[error("PostgreSQL usage record operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid usage retention days: {0}")]
    InvalidRetention(i64),
}

pub type PgUsageRecordStoreResult<T> = Result<T, PgUsageRecordStoreError>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageRecordFilter {
    pub kind: Option<String>,
    pub client_api_key_id: Option<String>,
    pub provider: Option<String>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub transport: Option<String>,
    pub attempt_index: Option<i64>,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub search: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct PgUsageRecordStore {
    pool: PgPool,
    buckets: PgRequestBucketStore,
}

impl PgUsageRecordStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            buckets: PgRequestBucketStore::new(pool.clone()),
            pool,
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn append(&self, event: &UsageRecord) -> PgUsageRecordStoreResult<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r"
insert into usage_records (
  id, request_id, client_api_key_id, kind, route, provider, account_id, model,
  requested_model, upstream_model, service_tier, status_code, transport, attempt_index,
  response_id, upstream_request_id, latency_ms, first_token_ms, input_tokens,
  output_tokens, cached_tokens, reasoning_tokens, message, metadata_json, created_at
) values (
  $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
  $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25
)",
        )
        .bind(&event.id)
        .bind(&event.request_id)
        .bind(&event.client_api_key_id)
        .bind(&event.kind)
        .bind(&event.route)
        .bind(&event.provider)
        .bind(&event.account_id)
        .bind(&event.model)
        .bind(&event.requested_model)
        .bind(&event.upstream_model)
        .bind(&event.service_tier)
        .bind(event.status_code)
        .bind(&event.transport)
        .bind(event.attempt_index)
        .bind(&event.response_id)
        .bind(&event.upstream_request_id)
        .bind(event.latency_ms)
        .bind(event.first_token_ms)
        .bind(event.input_tokens)
        .bind(event.output_tokens)
        .bind(event.cached_tokens)
        .bind(event.reasoning_tokens)
        .bind(&event.message)
        .bind(sqlx::types::Json(&event.metadata))
        .bind(event.created_at)
        .execute(&mut *tx)
        .await?;
        self.buckets.upsert_success(&mut tx, event).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn trim_to_retention(&self, now: DateTime<Utc>) -> PgUsageRecordStoreResult<u64> {
        let days: i64 =
            sqlx::query_scalar("select usage_retention_days from runtime_settings where id = 1")
                .fetch_one(&self.pool)
                .await?;
        let duration =
            Duration::try_days(days).ok_or(PgUsageRecordStoreError::InvalidRetention(days))?;
        let cutoff = now
            .checked_sub_signed(duration)
            .ok_or(PgUsageRecordStoreError::InvalidRetention(days))?;
        let result = sqlx::query("delete from usage_records where created_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn list_recent(
        &self,
        filter: UsageRecordFilter,
        limit: u32,
    ) -> PgUsageRecordStoreResult<Vec<UsageRecord>> {
        let limit = limit.clamp(1, 200);
        let mut builder = QueryBuilder::<Postgres>::new(USAGE_RECORD_SELECT_SQL);
        push_filter(&mut builder, &filter);
        builder.push(" order by created_at desc, id desc limit ");
        builder.push_bind(i64::from(limit));
        let rows = builder.build().fetch_all(&self.pool).await?;
        Ok(rows.iter().map(usage_record_from_row).collect())
    }

    pub async fn list_page(
        &self,
        filter: UsageRecordFilter,
        page: u32,
        page_size: u32,
    ) -> PgUsageRecordStoreResult<NumberedPage<UsageRecord>> {
        let page_size = page_size.clamp(1, 200);
        let total = count_usage_records(&self.pool, &filter).await?;
        let mut builder = QueryBuilder::<Postgres>::new(USAGE_RECORD_SELECT_SQL);
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
            .map(usage_record_from_row)
            .collect();
        Ok(NumberedPage {
            items,
            total,
            page: page.max(1),
            page_size,
        })
    }

    pub async fn get(&self, id: &str) -> PgUsageRecordStoreResult<Option<UsageRecord>> {
        let mut builder = QueryBuilder::<Postgres>::new(USAGE_RECORD_SELECT_SQL);
        builder.push(" where id = ");
        builder.push_bind(id);
        Ok(builder
            .build()
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(usage_record_from_row))
    }

    pub async fn account_email_map(
        &self,
        items: &[UsageRecord],
    ) -> PgUsageRecordStoreResult<HashMap<String, String>> {
        usage_record_account_email_map(&self.pool, items).await
    }

    pub async fn summary(
        &self,
        filter: UsageRecordFilter,
    ) -> PgUsageRecordStoreResult<UsageRecordSummary> {
        usage_summary(&self.pool, &filter).await
    }

    pub async fn account_usage(
        &self,
        filter: UsageRecordFilter,
        limit: u32,
    ) -> PgUsageRecordStoreResult<Vec<UsageRecordAccountUsage>> {
        usage_account_usage(&self.pool, &filter, limit).await
    }

    pub async fn model_distribution(
        &self,
        filter: UsageRecordFilter,
        source: UsageRecordModelSource,
    ) -> PgUsageRecordStoreResult<Vec<UsageRecordBreakdown>> {
        let expression = match source {
            UsageRecordModelSource::Requested => {
                "coalesce(nullif(requested_model, ''), model)"
            }
            UsageRecordModelSource::Upstream => {
                "coalesce(nullif(upstream_model, ''), model)"
            }
            UsageRecordModelSource::Mapping => {
                "coalesce(nullif(requested_model, ''), model) || ' -> ' || coalesce(nullif(upstream_model, ''), model)"
            }
        };
        usage_breakdown(&self.pool, &filter, expression, 8).await
    }

    pub async fn endpoint_distribution(
        &self,
        filter: UsageRecordFilter,
        source: UsageRecordEndpointSource,
    ) -> PgUsageRecordStoreResult<Vec<UsageRecordBreakdown>> {
        let expression = match source {
            UsageRecordEndpointSource::Inbound => "coalesce(nullif(route, ''), '未知端点')",
            UsageRecordEndpointSource::Upstream => "provider",
            UsageRecordEndpointSource::Path => {
                "coalesce(nullif(route, ''), '未知端点') || ' -> ' || provider"
            }
        };
        usage_breakdown(&self.pool, &filter, expression, 8).await
    }

    pub async fn trend(
        &self,
        filter: UsageRecordFilter,
    ) -> PgUsageRecordStoreResult<Vec<UsageRecordTrendPoint>> {
        usage_trend(&self.pool, &filter).await
    }

    pub async fn clear(&self) -> PgUsageRecordStoreResult<u64> {
        let result = sqlx::query("delete from usage_records")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

pub(super) fn push_filter(builder: &mut QueryBuilder<Postgres>, filter: &UsageRecordFilter) {
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
    push_optional_text(builder, "transport", filter.transport.as_deref());
    push_optional_i64(builder, "attempt_index", filter.attempt_index);
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
