//! `request_time_buckets` 可重建范围重算。

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use thiserror::Error;

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
    Database(#[from] sqlx::Error),
}

/// 删除两类事实都仍在保留期内的桶，并从成功/失败事实重新聚合。
pub async fn rebuild_buckets(pool: &PgPool) -> Result<RebuildBucketsReport, RebuildBucketsError> {
    let mut transaction = pool.begin().await?;
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
  success_count, error_count, input_tokens, output_tokens, cached_tokens,
  first_token_latency_sum, first_token_latency_count, latency_sum, latency_count,
  max_latency_ms, min_latency_ms, updated_at
)
select
  bucket_start, provider, account_id, model, service_tier,
  success_count, error_count, input_tokens, output_tokens, cached_tokens,
  first_token_latency_sum, first_token_latency_count, latency_sum, latency_count,
  max_latency_ms, min_latency_ms, now()
from aggregated",
    )
    .bind(cutoff)
    .execute(&mut *transaction)
    .await?
    .rows_affected();

    transaction.commit().await?;
    Ok(RebuildBucketsReport {
        cutoff,
        deleted_rows,
        rebuilt_rows,
    })
}
