//! PostgreSQL 请求时间桶聚合查询。

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use thiserror::Error;

use crate::infra::time::china_quarter_hour_start;

const LIST_USAGE_TIME_BUCKETS_SQL: &str = r"
select
  bucket_start,
  model,
  nullif(service_tier, '__unknown__') as service_tier,
  coalesce(sum(success_count + error_count), 0)::bigint as request_count,
  coalesce(sum(error_count), 0)::bigint as error_count,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
  coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens,
  coalesce(sum(first_token_latency_sum), 0)::bigint as first_token_latency_sum,
  coalesce(sum(first_token_latency_count), 0)::bigint as first_token_latency_count,
  coalesce(sum(latency_sum), 0)::bigint as latency_sum,
  coalesce(sum(latency_count), 0)::bigint as latency_count,
  coalesce(max(max_latency_ms), 0) as max_latency_ms,
  coalesce(min(min_latency_ms), 0) as min_latency_ms
from request_time_buckets
where bucket_start >= $1 and bucket_start <= $2
group by bucket_start, model, service_tier
order by bucket_start asc, model asc, service_tier asc";

const RETAINED_USAGE_BUCKETS_SQL: &str = r"
select
  model,
  nullif(service_tier, '__unknown__') as service_tier,
  coalesce(sum(success_count + error_count), 0)::bigint as request_count,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
  coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens
from request_time_buckets
group by bucket_start, model, service_tier";

/// 请求时间桶查询错误。
#[derive(Debug, Error)]
pub enum PgRequestBucketQueryError {
    /// 数据库查询失败。
    #[error("PostgreSQL request bucket query failed: {0}")]
    Database(#[from] sqlx::Error),
}

/// 时间桶聚合用量记录。
#[derive(Debug, Clone)]
pub struct UsageTimeBucketRecord {
    pub bucket_start: DateTime<Utc>,
    pub model: String,
    pub service_tier: Option<String>,
    pub request_count: i64,
    pub error_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: i64,
    pub first_token_latency_sum: i64,
    pub first_token_latency_count: i64,
    pub latency_sum: i64,
    pub latency_count: i64,
    pub max_latency_ms: i64,
    pub min_latency_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageBucketWindow {
    pub account_id: String,
    pub key: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageBucketTotals {
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: i64,
}

/// 保留期内单个计费桶的用量事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedUsageBucket {
    pub model: String,
    pub service_tier: Option<String>,
    pub totals: UsageBucketTotals,
}

impl UsageBucketTotals {
    pub(crate) fn add(&mut self, other: Self) {
        self.request_count = self.request_count.saturating_add(other.request_count);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(other.cached_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelUsageWindow {
    pub account_id: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelBucketUsage {
    pub account_id: String,
    pub model: String,
    pub service_tier: Option<String>,
    pub request_count: i64,
    pub error_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: i64,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// 请求时间桶聚合查询。
#[derive(Clone)]
pub struct PgRequestBucketQuery {
    pool: PgPool,
}

impl PgRequestBucketQuery {
    /// 构造请求时间桶查询。
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 列出指定时间范围内的时间桶聚合用量。
    pub async fn list(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<UsageTimeBucketRecord>, PgRequestBucketQueryError> {
        let rows = sqlx::query(LIST_USAGE_TIME_BUCKETS_SQL)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(usage_time_bucket_from_row).collect())
    }

    /// 列出请求时间桶保留期内的全局计费桶，不受当前账号集合影响。
    pub async fn retained_usage_buckets(
        &self,
    ) -> Result<Vec<RetainedUsageBucket>, PgRequestBucketQueryError> {
        let rows = sqlx::query(RETAINED_USAGE_BUCKETS_SQL)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|row| RetainedUsageBucket {
                model: row.get("model"),
                service_tier: row.get("service_tier"),
                totals: UsageBucketTotals {
                    request_count: row.get("request_count"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    cached_tokens: row.get("cached_tokens"),
                    cache_write_tokens: row.get("cache_write_tokens"),
                },
            })
            .collect())
    }

    pub async fn usage_by_windows(
        &self,
        windows: &[UsageBucketWindow],
    ) -> Result<HashMap<String, HashMap<String, UsageBucketTotals>>, PgRequestBucketQueryError>
    {
        let Some(min_start) = windows.iter().map(|window| window.start).min() else {
            return Ok(HashMap::new());
        };
        let Some(max_end) = windows.iter().map(|window| window.end).max() else {
            return Ok(HashMap::new());
        };
        let account_ids = windows
            .iter()
            .map(|window| window.account_id.as_str())
            .collect::<HashSet<_>>();
        let mut windows_by_account = HashMap::<&str, Vec<&UsageBucketWindow>>::new();
        for window in windows {
            windows_by_account
                .entry(window.account_id.as_str())
                .or_default()
                .push(window);
        }

        let mut builder = QueryBuilder::<Postgres>::new(
            "select
              account_id,
              bucket_start,
              coalesce(sum(success_count + error_count), 0)::bigint as request_count,
              coalesce(sum(input_tokens), 0)::bigint as input_tokens,
              coalesce(sum(output_tokens), 0)::bigint as output_tokens,
              coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
              coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens
            from request_time_buckets
            where account_id in (",
        );
        let mut separated = builder.separated(", ");
        for account_id in &account_ids {
            separated.push_bind(*account_id);
        }
        separated.push_unseparated(")");
        builder.push(" and bucket_start >= ");
        builder.push_bind(china_quarter_hour_start(min_start));
        builder.push(" and bucket_start <= ");
        builder.push_bind(max_end);
        builder.push(" group by account_id, bucket_start");

        let rows = builder.build().fetch_all(&self.pool).await?;
        let mut usage_by_account = HashMap::<String, HashMap<String, UsageBucketTotals>>::new();
        for row in rows {
            let account_id: String = row.get("account_id");
            let bucket_start = row.get::<DateTime<Utc>, _>("bucket_start");
            let bucket_usage = UsageBucketTotals {
                request_count: row.get("request_count"),
                input_tokens: row.get("input_tokens"),
                output_tokens: row.get("output_tokens"),
                cached_tokens: row.get("cached_tokens"),
                cache_write_tokens: row.get("cache_write_tokens"),
            };
            let Some(account_windows) = windows_by_account.get(account_id.as_str()) else {
                continue;
            };
            for window in account_windows.iter().filter(|window| {
                bucket_start >= china_quarter_hour_start(window.start) && bucket_start <= window.end
            }) {
                usage_by_account
                    .entry(account_id.clone())
                    .or_default()
                    .entry(window.key.clone())
                    .or_default()
                    .add(bucket_usage);
            }
        }
        Ok(usage_by_account)
    }

    pub async fn model_usage_by_windows(
        &self,
        windows: &[ModelUsageWindow],
    ) -> Result<Vec<ModelBucketUsage>, PgRequestBucketQueryError> {
        if windows.is_empty() {
            return Ok(Vec::new());
        }
        let mut builder = QueryBuilder::<Postgres>::new(
            "select
              account_id,
              model,
              service_tier,
              coalesce(sum(success_count + error_count), 0)::bigint as request_count,
              coalesce(sum(error_count), 0)::bigint as error_count,
              coalesce(sum(input_tokens), 0)::bigint as input_tokens,
              coalesce(sum(output_tokens), 0)::bigint as output_tokens,
              coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
              coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens,
              max(bucket_start) as last_used_at
            from request_time_buckets
            where model != '__unknown__' and (",
        );
        for (index, window) in windows.iter().enumerate() {
            if index > 0 {
                builder.push(" or ");
            }
            builder.push("(account_id = ");
            builder.push_bind(&window.account_id);
            builder.push(" and bucket_start >= ");
            builder.push_bind(china_quarter_hour_start(window.start));
            builder.push(" and bucket_start <= ");
            builder.push_bind(window.end);
            builder.push(")");
        }
        builder.push(") group by account_id, model, service_tier");

        let rows = builder.build().fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|row| {
                let service_tier = row.get::<String, _>("service_tier");
                ModelBucketUsage {
                    account_id: row.get("account_id"),
                    model: row.get("model"),
                    service_tier: (service_tier != "__unknown__").then_some(service_tier),
                    request_count: row.get("request_count"),
                    error_count: row.get("error_count"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    cached_tokens: row.get("cached_tokens"),
                    cache_write_tokens: row.get("cache_write_tokens"),
                    last_used_at: row.get("last_used_at"),
                }
            })
            .collect())
    }
}

fn usage_time_bucket_from_row(row: &sqlx::postgres::PgRow) -> UsageTimeBucketRecord {
    UsageTimeBucketRecord {
        bucket_start: row.get("bucket_start"),
        model: row.get("model"),
        service_tier: row.get("service_tier"),
        request_count: row.get("request_count"),
        error_count: row.get("error_count"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        cache_write_tokens: row.get("cache_write_tokens"),
        first_token_latency_sum: row.get("first_token_latency_sum"),
        first_token_latency_count: row.get("first_token_latency_count"),
        latency_sum: row.get("latency_sum"),
        latency_count: row.get("latency_count"),
        max_latency_ms: row.get("max_latency_ms"),
        min_latency_ms: row.get("min_latency_ms"),
    }
}
