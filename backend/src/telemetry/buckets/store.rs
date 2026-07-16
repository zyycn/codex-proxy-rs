//! PostgreSQL 请求时间桶存储。

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres};
use thiserror::Error;

use crate::{
    infra::time::china_quarter_hour_start,
    telemetry::{ops::types::OpsErrorLog, usage::types::UsageRecord},
};

const UNKNOWN_DIMENSION: &str = "__unknown__";

/// PostgreSQL 请求时间桶存储错误。
#[derive(Debug, Error)]
pub enum PgRequestBucketStoreError {
    /// 数据库操作失败。
    #[error("PostgreSQL request bucket operation failed: {0}")]
    Database(#[from] sqlx::Error),
    /// 保留期配置非法。
    #[error("invalid request bucket retention days: {0}")]
    InvalidRetention(i64),
}

/// PostgreSQL 请求时间桶存储。
#[derive(Clone)]
pub struct PgRequestBucketStore {
    pool: PgPool,
}

/// 可由事实表重建的请求时间桶范围结果。
pub(crate) struct RebuiltRequestBuckets {
    pub cutoff: DateTime<Utc>,
    pub deleted_rows: u64,
    pub rebuilt_rows: u64,
}

impl PgRequestBucketStore {
    /// 构造请求时间桶存储。
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 在成功事实事务内累计请求时间桶。
    pub async fn upsert_success(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        event: &UsageRecord,
    ) -> Result<(), sqlx::Error> {
        let latency = event.latency_ms;
        let first_token = event.first_token_ms;
        sqlx::query(
            r"
insert into request_time_buckets (
  bucket_start, provider, account_id, model, service_tier, success_count,
  input_tokens, output_tokens, cached_tokens, cache_write_tokens, first_token_latency_sum,
  first_token_latency_count, latency_sum, latency_count, max_latency_ms,
  min_latency_ms, updated_at
) values ($1, $2, $3, $4, $5, 1, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
on conflict (bucket_start, provider, account_id, model, service_tier) do update set
  success_count = request_time_buckets.success_count + 1,
  input_tokens = request_time_buckets.input_tokens + excluded.input_tokens,
  output_tokens = request_time_buckets.output_tokens + excluded.output_tokens,
  cached_tokens = request_time_buckets.cached_tokens + excluded.cached_tokens,
  cache_write_tokens = request_time_buckets.cache_write_tokens + excluded.cache_write_tokens,
  first_token_latency_sum = request_time_buckets.first_token_latency_sum + excluded.first_token_latency_sum,
  first_token_latency_count = request_time_buckets.first_token_latency_count + excluded.first_token_latency_count,
  latency_sum = request_time_buckets.latency_sum + excluded.latency_sum,
  latency_count = request_time_buckets.latency_count + excluded.latency_count,
  max_latency_ms = greatest(request_time_buckets.max_latency_ms, excluded.max_latency_ms),
  min_latency_ms = case
    when request_time_buckets.min_latency_ms is null then excluded.min_latency_ms
    when excluded.min_latency_ms is null then request_time_buckets.min_latency_ms
    else least(request_time_buckets.min_latency_ms, excluded.min_latency_ms)
  end,
  updated_at = excluded.updated_at",
        )
        .bind(china_quarter_hour_start(event.created_at))
        .bind(dimension(Some(&event.provider)))
        .bind(dimension(Some(&event.account_id)))
        .bind(dimension(Some(&event.model)))
        .bind(dimension(event.service_tier.as_deref()))
        .bind(event.input_tokens.unwrap_or(0))
        .bind(event.output_tokens.unwrap_or(0))
        .bind(event.cached_tokens.unwrap_or(0))
        .bind(event.cache_write_tokens.unwrap_or(0))
        .bind(first_token.unwrap_or(0))
        .bind(i64::from(first_token.is_some()))
        .bind(latency.unwrap_or(0))
        .bind(i64::from(latency.is_some()))
        .bind(latency.unwrap_or(0))
        .bind(latency)
        .bind(Utc::now())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// 在错误事实事务内累计请求时间桶。
    pub async fn upsert_error(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        event: &OpsErrorLog,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r"
insert into request_time_buckets (
  bucket_start, provider, account_id, model, service_tier, error_count, updated_at
) values ($1, $2, $3, $4, $5, 1, $6)
on conflict (bucket_start, provider, account_id, model, service_tier) do update set
  error_count = request_time_buckets.error_count + 1,
  updated_at = excluded.updated_at",
        )
        .bind(china_quarter_hour_start(event.created_at))
        .bind(dimension(event.provider.as_deref()))
        .bind(dimension(event.account_id.as_deref()))
        .bind(dimension(event.model.as_deref()))
        .bind(dimension(None))
        .bind(Utc::now())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// 按运行时配置清理过期请求时间桶。
    pub async fn trim_to_retention(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, PgRequestBucketStoreError> {
        let days: i64 =
            sqlx::query_scalar("select bucket_retention_days from runtime_settings where id = 1")
                .fetch_one(&self.pool)
                .await?;
        let duration =
            Duration::try_days(days).ok_or(PgRequestBucketStoreError::InvalidRetention(days))?;
        let cutoff = now
            .checked_sub_signed(duration)
            .ok_or(PgRequestBucketStoreError::InvalidRetention(days))?;
        let result = sqlx::query("delete from request_time_buckets where bucket_start < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 在单个事务内重建两类事实都仍处于保留期内的请求时间桶。
    pub(crate) async fn rebuild_reconstructible_range(
        &self,
    ) -> Result<RebuiltRequestBuckets, PgRequestBucketStoreError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "lock table usage_records, ops_error_logs, request_time_buckets
             in share row exclusive mode",
        )
        .execute(&mut *transaction)
        .await?;

        let retention_days: i64 = sqlx::query_scalar(
            "select least(usage_retention_days, ops_error_retention_days)
             from runtime_settings where id = 1",
        )
        .fetch_one(&mut *transaction)
        .await?;
        let cutoff: DateTime<Utc> = sqlx::query_scalar(
            "select to_timestamp(
               floor(extract(epoch from now() - ($1::bigint * interval '1 day')) / 900) * 900
             )",
        )
        .bind(retention_days)
        .fetch_one(&mut *transaction)
        .await?;

        let deleted_rows = sqlx::query("delete from request_time_buckets where bucket_start >= $1")
            .bind(cutoff)
            .execute(&mut *transaction)
            .await?
            .rows_affected();

        let rebuilt_rows = sqlx::query(
            r"
with facts as (
  select
    to_timestamp(floor(extract(epoch from created_at) / 900) * 900) as bucket_start,
    coalesce(nullif(trim(provider), ''), '__unknown__') as provider,
    coalesce(nullif(trim(account_id), ''), '__unknown__') as account_id,
    coalesce(nullif(trim(model), ''), '__unknown__') as model,
    coalesce(nullif(trim(service_tier), ''), '__unknown__') as service_tier,
    1::bigint as success_count,
    0::bigint as error_count,
    coalesce(input_tokens, 0)::bigint as input_tokens,
    coalesce(output_tokens, 0)::bigint as output_tokens,
    coalesce(cached_tokens, 0)::bigint as cached_tokens,
    coalesce(cache_write_tokens, 0)::bigint as cache_write_tokens,
    coalesce(first_token_ms, 0)::bigint as first_token_latency_sum,
    (first_token_ms is not null)::integer::bigint as first_token_latency_count,
    coalesce(latency_ms, 0)::bigint as latency_sum,
    (latency_ms is not null)::integer::bigint as latency_count,
    coalesce(latency_ms, 0)::bigint as max_latency_ms,
    latency_ms::bigint as min_latency_ms
  from usage_records
  where created_at >= $1

  union all

  select
    to_timestamp(floor(extract(epoch from created_at) / 900) * 900),
    coalesce(nullif(trim(provider), ''), '__unknown__'),
    coalesce(nullif(trim(account_id), ''), '__unknown__'),
    coalesce(nullif(trim(model), ''), '__unknown__'),
    '__unknown__',
    0::bigint,
    1::bigint,
    0::bigint,
    0::bigint,
    0::bigint,
    0::bigint,
    0::bigint,
    0::bigint,
    0::bigint,
    0::bigint,
    0::bigint,
    null::bigint
  from ops_error_logs
  where created_at >= $1
), aggregated as (
  select
    bucket_start, provider, account_id, model, service_tier,
    sum(success_count) as success_count,
    sum(error_count) as error_count,
    sum(input_tokens) as input_tokens,
    sum(output_tokens) as output_tokens,
    sum(cached_tokens) as cached_tokens,
    sum(cache_write_tokens) as cache_write_tokens,
    sum(first_token_latency_sum) as first_token_latency_sum,
    sum(first_token_latency_count) as first_token_latency_count,
    sum(latency_sum) as latency_sum,
    sum(latency_count) as latency_count,
    max(max_latency_ms) as max_latency_ms,
    min(min_latency_ms) as min_latency_ms
  from facts
  group by bucket_start, provider, account_id, model, service_tier
)
insert into request_time_buckets (
  bucket_start, provider, account_id, model, service_tier,
  success_count, error_count, input_tokens, output_tokens, cached_tokens, cache_write_tokens,
  first_token_latency_sum, first_token_latency_count, latency_sum, latency_count,
  max_latency_ms, min_latency_ms, updated_at
)
select
  bucket_start, provider, account_id, model, service_tier,
  success_count, error_count, input_tokens, output_tokens, cached_tokens, cache_write_tokens,
  first_token_latency_sum, first_token_latency_count, latency_sum, latency_count,
  max_latency_ms, min_latency_ms, now()
from aggregated",
        )
        .bind(cutoff)
        .execute(&mut *transaction)
        .await?
        .rows_affected();

        transaction.commit().await?;
        Ok(RebuiltRequestBuckets {
            cutoff,
            deleted_rows,
            rebuilt_rows,
        })
    }
}

fn dimension(value: Option<&str>) -> &str {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(UNKNOWN_DIMENSION)
}
