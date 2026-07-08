//! 运维错误事件存储实现（SQLite）。

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::SqlitePool;
use thiserror::Error;

use crate::{
    admin::monitoring::{
        ops_error_model::OpsErrorLog,
        usage_record_model::{metadata_i64, metadata_service_tier, metadata_string},
        usage_record_store::USAGE_RECORD_RETENTION_DAYS,
    },
    infra::time::china_quarter_hour_start,
};

/// SQLite 运维错误事件错误。
#[derive(Debug, Error)]
pub enum SqliteOpsErrorLogStoreError {
    /// 数据库错误。
    #[error("sqlite ops error log database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// SQLite 运维错误事件结果。
pub type SqliteOpsErrorLogStoreResult<T> = Result<T, SqliteOpsErrorLogStoreError>;

/// SQLite 运维错误事件存储。
#[derive(Clone)]
pub struct SqliteOpsErrorLogStore {
    pool: SqlitePool,
}

impl SqliteOpsErrorLogStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 写入运维错误事件。
    pub async fn append(&self, event: &OpsErrorLog) -> SqliteOpsErrorLogStoreResult<()> {
        append_event(&self.pool, event).await
    }

    /// 按保留期清理运维错误事件。
    pub async fn trim_to_retention(&self, now: DateTime<Utc>) -> SqliteOpsErrorLogStoreResult<u64> {
        trim_to_retention(&self.pool, now).await
    }

    /// 清空运维错误事件。
    pub async fn clear(&self) -> SqliteOpsErrorLogStoreResult<u64> {
        let result = sqlx::query("delete from ops_error_logs")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 读取错误事件数量。
    pub async fn count(&self) -> SqliteOpsErrorLogStoreResult<u64> {
        let (total,): (i64,) = sqlx::query_as("select count(*) from ops_error_logs")
            .fetch_one(&self.pool)
            .await?;
        Ok(total.max(0) as u64)
    }
}

#[derive(Debug, Clone)]
struct OpsErrorSummaryFields {
    client_status_code: Option<i64>,
    upstream_status_code: Option<i64>,
    transport: Option<String>,
    attempt_index: Option<i64>,
    failure_class: Option<String>,
    response_id: Option<String>,
    upstream_request_id: Option<String>,
}

fn ops_error_summary_fields(event: &OpsErrorLog) -> OpsErrorSummaryFields {
    OpsErrorSummaryFields {
        client_status_code: event.client_status_code.or_else(|| {
            metadata_i64(
                &event.metadata,
                &["clientStatusCode", "client_status_code", "clientStatus"],
            )
        }),
        upstream_status_code: event.upstream_status_code.or_else(|| {
            metadata_i64(
                &event.metadata,
                &[
                    "upstreamStatus",
                    "upstreamStatusCode",
                    "upstream_status_code",
                ],
            )
        }),
        transport: event
            .transport
            .clone()
            .or_else(|| metadata_string(&event.metadata, &["transport"])),
        attempt_index: event
            .attempt_index
            .or_else(|| metadata_i64(&event.metadata, &["attemptIndex", "attempt_index"])),
        failure_class: event
            .failure_class
            .clone()
            .or_else(|| metadata_string(&event.metadata, &["failureClass", "failure_class"])),
        response_id: event
            .response_id
            .clone()
            .or_else(|| metadata_string(&event.metadata, &["responseId", "response_id"])),
        upstream_request_id: event.upstream_request_id.clone().or_else(|| {
            metadata_string(
                &event.metadata,
                &[
                    "upstreamRequestId",
                    "upstream_request_id",
                    "openaiRequestId",
                ],
            )
        }),
    }
}

async fn append_event(pool: &SqlitePool, event: &OpsErrorLog) -> SqliteOpsErrorLogStoreResult<()> {
    let summary = ops_error_summary_fields(event);
    sqlx::query(
        "insert into ops_error_logs (id, request_id, kind, account_id, route, model, status_code, client_status_code, upstream_status_code, transport, attempt_index, failure_class, response_id, upstream_request_id, latency_ms, message, metadata_json, created_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event.id)
    .bind(&event.request_id)
    .bind(&event.kind)
    .bind(&event.account_id)
    .bind(&event.route)
    .bind(&event.model)
    .bind(event.status_code)
    .bind(summary.client_status_code)
    .bind(summary.upstream_status_code)
    .bind(summary.transport)
    .bind(summary.attempt_index)
    .bind(summary.failure_class)
    .bind(summary.response_id)
    .bind(summary.upstream_request_id)
    .bind(event.latency_ms)
    .bind(&event.message)
    .bind(event.metadata.to_string())
    .bind(event.created_at.to_rfc3339())
    .execute(pool)
    .await?;
    append_error_time_bucket(pool, event).await?;
    Ok(())
}

async fn append_error_time_bucket(
    pool: &SqlitePool,
    event: &OpsErrorLog,
) -> SqliteOpsErrorLogStoreResult<()> {
    let bucket_start = china_quarter_hour_start(event.created_at);
    let input_tokens = metadata_usage_i64(&event.metadata, "inputTokens");
    let output_tokens = metadata_usage_i64(&event.metadata, "outputTokens");
    let cached_tokens = metadata_usage_i64(&event.metadata, "cachedTokens");
    let first_token_latency = metadata_first_token_latency(&event.metadata).unwrap_or(0);
    let first_token_latency_count = i64::from(first_token_latency > 0);
    let latency = event.latency_ms.filter(|value| *value > 0).unwrap_or(0);
    let latency_count = i64::from(latency > 0);
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        r"
insert into usage_time_buckets (
  bucket_start,
  account_id,
  model,
  service_tier,
  request_count,
  error_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  first_token_latency_sum,
  first_token_latency_count,
  latency_sum,
  latency_count,
  max_latency_ms,
  min_latency_ms,
  updated_at
) values (?, ?, ?, ?, 1, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
on conflict(bucket_start, account_id, model, service_tier) do update set
  request_count = request_count + excluded.request_count,
  error_count = error_count + excluded.error_count,
  input_tokens = input_tokens + excluded.input_tokens,
  output_tokens = output_tokens + excluded.output_tokens,
  cached_tokens = cached_tokens + excluded.cached_tokens,
  first_token_latency_sum = first_token_latency_sum + excluded.first_token_latency_sum,
  first_token_latency_count = first_token_latency_count + excluded.first_token_latency_count,
  latency_sum = latency_sum + excluded.latency_sum,
  latency_count = latency_count + excluded.latency_count,
  max_latency_ms = max(max_latency_ms, excluded.max_latency_ms),
  min_latency_ms = case
    when min_latency_ms = 0 then excluded.min_latency_ms
    when excluded.min_latency_ms = 0 then min_latency_ms
    else min(min_latency_ms, excluded.min_latency_ms)
  end,
  updated_at = excluded.updated_at",
    )
    .bind(bucket_start.to_rfc3339())
    .bind(event.account_id.as_deref().unwrap_or_default())
    .bind(event.model.as_deref().unwrap_or_default())
    .bind(metadata_service_tier(&event.metadata).unwrap_or_default())
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cached_tokens)
    .bind(first_token_latency)
    .bind(first_token_latency_count)
    .bind(latency)
    .bind(latency_count)
    .bind(latency)
    .bind(latency)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

fn metadata_usage_i64(metadata: &Value, field: &str) -> i64 {
    metadata
        .get("usage")
        .and_then(|usage| usage.get(field))
        .or_else(|| metadata.get(field))
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(0)
}

fn metadata_first_token_latency(metadata: &Value) -> Option<i64> {
    [
        "firstTokenMs",
        "first_token_ms",
        "firstTokenLatencyMs",
        "first_token_latency_ms",
    ]
    .into_iter()
    .find_map(|field| {
        metadata
            .get(field)
            .or_else(|| metadata.get("usage").and_then(|usage| usage.get(field)))
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
    })
}

async fn trim_to_retention(
    pool: &SqlitePool,
    now: DateTime<Utc>,
) -> SqliteOpsErrorLogStoreResult<u64> {
    let cutoff = now - Duration::days(USAGE_RECORD_RETENTION_DAYS);
    let result = sqlx::query("delete from ops_error_logs where created_at < ?")
        .bind(cutoff.to_rfc3339())
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
