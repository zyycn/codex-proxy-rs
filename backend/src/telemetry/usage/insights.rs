//! 管理端请求观测聚合。

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::{AssertSqlSafe, PgPool, Row};

use crate::telemetry::billing::{LONG_CONTEXT_THRESHOLD, calculate_aggregate_billing};

use super::store::PgUsageRecordStoreResult;

const DEFAULT_RANGE_DAYS: i64 = 7;

// 一个请求可能写入多个错误事实。查询时优先把存在成功事实的 request_id 视为成功，
// 否则只保留该 request_id 的最新错误；缺少 request_id 时退化到事实 id。
const TERMINAL_REQUESTS_CTE: &str = r#"
success_terminals as (
  select distinct on (request_key)
    request_key, id, request_id, terminal_at, true as is_success,
    client_api_key_id, provider, account_id, transport, model_name,
    null::text as failure_class, false as is_client_cancelled,
    false as is_caller_error, latency_ms, first_token_ms, billing_model,
    service_tier, input_tokens, output_tokens, cached_tokens
  from (
    select
      case when request_id is null then 'event:' || id else 'request:' || request_id end as request_key,
      id, request_id, created_at as terminal_at, client_api_key_id, provider,
      account_id, transport,
      coalesce(nullif(requested_model, ''), model) || ' → ' ||
        coalesce(nullif(upstream_model, ''), model) as model_name,
      latency_ms, first_token_ms,
      coalesce(nullif(upstream_model, ''), model) as billing_model,
      service_tier, input_tokens, output_tokens, cached_tokens
    from usage_records
    where created_at >= $1 and created_at < $2
  ) candidates
  order by request_key, terminal_at desc, id desc
),
error_terminals as (
  select distinct on (request_key)
    request_key, id, request_id, terminal_at, false as is_success,
    client_api_key_id, provider, account_id, transport, model_name,
    failure_class, is_client_cancelled, is_caller_error,
    latency_ms, null::bigint as first_token_ms,
    null::text as billing_model, null::text as service_tier,
    null::bigint as input_tokens, null::bigint as output_tokens,
    null::bigint as cached_tokens
  from (
    select
      case when request_id is null then 'event:' || id else 'request:' || request_id end as request_key,
      id, request_id, created_at as terminal_at, client_api_key_id, provider,
      account_id, transport, coalesce(nullif(model, ''), '未知模型') as model_name,
      coalesce(
        nullif(failure_class, ''),
        nullif(metadata_json->>'failureClass', ''),
        nullif(metadata_json->>'upstreamCode', ''),
        nullif(metadata_json->>'terminal', ''),
        '未分类错误'
      ) as failure_class,
      coalesce((
        coalesce(
          nullif(failure_class, ''),
          nullif(metadata_json->>'failureClass', ''),
          nullif(metadata_json->>'upstreamCode', ''),
          nullif(metadata_json->>'terminal', ''),
          ''
        ) in ('cancelled', 'downstream_closed', 'client_cancelled', 'consumer_dropped')
        or (
          (status_code = 499 or client_status_code = 499)
          and (
            metadata_json->>'cancelled' = 'true'
            or message = 'v1 responses stream cancelled'
          )
        )
      ), false) as is_client_cancelled,
      coalesce(metadata_json->>'failureSource' = 'client', false) as is_caller_error,
      latency_ms
    from ops_error_logs
    where created_at >= $1 and created_at < $2
  ) candidates
  order by request_key, terminal_at desc, id desc
),
final_errors as (
  select errors.*
  from error_terminals errors
  where not exists (
    select 1
    from usage_records successes
    where
      (errors.request_id is not null and successes.request_id = errors.request_id)
      or (errors.request_id is null and successes.id = errors.id)
  )
),
terminal_requests as (
  select * from success_terminals
  union all
  select * from final_errors
)
"#;

/// 观测时间粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightsGranularity {
    QuarterHour,
    Hour,
    Day,
}

impl InsightsGranularity {
    pub fn for_range(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        let range = end.signed_duration_since(start);
        if range <= Duration::days(2) {
            Self::QuarterHour
        } else if range <= Duration::days(14) {
            Self::Hour
        } else {
            Self::Day
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::QuarterHour => "15m",
            Self::Hour => "1h",
            Self::Day => "1d",
        }
    }

    fn step_seconds(self) -> i64 {
        match self {
            Self::QuarterHour => 15 * 60,
            Self::Hour => 60 * 60,
            Self::Day => 24 * 60 * 60,
        }
    }

    fn bucket_expression(self, column: &str) -> String {
        let interval = match self {
            Self::QuarterHour => "15 minutes",
            Self::Hour => "1 hour",
            Self::Day => "1 day",
        };
        let origin = if self == Self::Day {
            "1970-01-01 16:00:00+00"
        } else {
            "1970-01-01 00:00:00+00"
        };
        format!("date_bin(interval '{interval}', {column}, timestamptz '{origin}')")
    }
}

/// 热点诊断维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageDiagnosticsDimension {
    Model,
    Account,
    ApiKey,
    Provider,
    Transport,
    FailureClass,
}

impl UsageDiagnosticsDimension {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "model" => Some(Self::Model),
            "account" => Some(Self::Account),
            "apiKey" | "api_key" => Some(Self::ApiKey),
            "provider" => Some(Self::Provider),
            "transport" => Some(Self::Transport),
            "failureClass" | "failure_class" => Some(Self::FailureClass),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Account => "account",
            Self::ApiKey => "apiKey",
            Self::Provider => "provider",
            Self::Transport => "transport",
            Self::FailureClass => "failureClass",
        }
    }

    fn expression(self) -> &'static str {
        match self {
            Self::Model => "terminal_requests.model_name",
            Self::Account => {
                "coalesce(nullif(concat_ws(' → ', nullif(diagnostic_accounts.email, ''), nullif(terminal_requests.account_id, '')), ''), '未知账号')"
            }
            Self::ApiKey => "coalesce(nullif(diagnostic_api_keys.name, ''), '未知密钥')",
            Self::Provider => "coalesce(nullif(terminal_requests.provider, ''), '未知 Provider')",
            Self::Transport => "coalesce(nullif(terminal_requests.transport, ''), '未知传输')",
            Self::FailureClass => "terminal_requests.failure_class",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::Account => {
                "terminal_requests left join accounts diagnostic_accounts on diagnostic_accounts.id = terminal_requests.account_id"
            }
            Self::ApiKey => {
                "terminal_requests left join client_api_keys diagnostic_api_keys on diagnostic_api_keys.id = terminal_requests.client_api_key_id"
            }
            Self::Model | Self::Provider | Self::Transport | Self::FailureClass => {
                "terminal_requests"
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageInsightsOverview {
    granularity: String,
    health: UsageHealthInsights,
    performance: UsagePerformanceInsights,
    cost: UsageCostInsights,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageHealthInsights {
    total_requests: u64,
    success_requests: u64,
    failed_requests: u64,
    cancelled_requests: u64,
    caller_error_requests: u64,
    success_rate: f64,
    request_change_rate: Option<f64>,
    success_rate_change: Option<f64>,
    points: Vec<UsageHealthPoint>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageHealthPoint {
    bucket: DateTime<Utc>,
    label: String,
    success_requests: u64,
    failed_requests: u64,
    cancelled_requests: u64,
    caller_error_requests: u64,
    error_rate: f64,
}

/// Dashboard 请求健康时间桶，仅统计每个请求的最终终态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHealthTimeBucket {
    pub bucket_start: DateTime<Utc>,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub caller_error_requests: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsagePerformanceInsights {
    latency_p50_ms: Option<f64>,
    latency_p95_ms: Option<f64>,
    latency_p99_ms: Option<f64>,
    ttft_p50_ms: Option<f64>,
    ttft_p95_ms: Option<f64>,
    ttft_p99_ms: Option<f64>,
    latency_coverage: f64,
    ttft_coverage: f64,
    points: Vec<UsagePerformancePoint>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsagePerformancePoint {
    bucket: DateTime<Utc>,
    label: String,
    latency_p50_ms: Option<f64>,
    latency_p95_ms: Option<f64>,
    latency_p99_ms: Option<f64>,
    ttft_p50_ms: Option<f64>,
    ttft_p95_ms: Option<f64>,
    ttft_p99_ms: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageCostInsights {
    estimated_cost: f64,
    standard_cost: f64,
    cost_per_request: f64,
    tokens_per_request: f64,
    cached_token_rate: f64,
    cache_hit_request_rate: f64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    total_tokens: u64,
    points: Vec<UsageCostPoint>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageCostPoint {
    bucket: DateTime<Utc>,
    label: String,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    total_tokens: u64,
    estimated_cost: f64,
    standard_cost: f64,
    cached_token_rate: f64,
    cache_hit_request_rate: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDiagnosticsInsights {
    dimension: String,
    items: Vec<UsageDiagnosticsItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageDiagnosticsItem {
    name: String,
    request_count: u64,
    success_count: u64,
    error_count: u64,
    error_rate: f64,
    request_share: f64,
    latency_p95_ms: Option<f64>,
    estimated_cost: f64,
}

pub async fn overview(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> PgUsageRecordStoreResult<UsageInsightsOverview> {
    let granularity = InsightsGranularity::for_range(start, end);
    let (health, performance, cost) = tokio::try_join!(
        load_health(pool, start, end, granularity),
        load_performance(pool, start, end, granularity),
        load_cost(pool, start, end, granularity),
    )?;
    Ok(UsageInsightsOverview {
        granularity: granularity.as_str().to_string(),
        health,
        performance,
        cost,
    })
}

pub async fn diagnostics(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    dimension: UsageDiagnosticsDimension,
) -> PgUsageRecordStoreResult<UsageDiagnosticsInsights> {
    let expression = dimension.expression();
    let source = dimension.source();
    let failure_filter = if dimension == UsageDiagnosticsDimension::FailureClass {
        "where not terminal_requests.is_success"
    } else {
        ""
    };
    let sql = format!(
        "with {TERMINAL_REQUESTS_CTE}\n\
         select {expression} as name,\n\
           count(*)::bigint as request_count,\n\
           count(*) filter (where is_success)::bigint as success_count,\n\
           count(*) filter (where not is_success)::bigint as error_count,\n\
           percentile_cont(0.95) within group (order by latency_ms)\n\
             filter (where is_success and latency_ms is not null) as latency_p95_ms,\n\
           (select count(*) from terminal_requests)::bigint as total_requests\n\
         from {source} {failure_filter}\n\
         group by name\n\
         order by request_count desc, name asc\n\
         limit 8"
    );
    // SAFETY: SQL fragments come only from closed enums and module constants; values remain bound.
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;

    let costs = if dimension == UsageDiagnosticsDimension::FailureClass {
        HashMap::new()
    } else {
        load_diagnostic_costs(pool, start, end, dimension).await?
    };
    let items = rows
        .into_iter()
        .map(|row| {
            let name: String = row.get("name");
            let request_count = nonnegative(row.get("request_count"));
            let success_count = nonnegative(row.get("success_count"));
            let error_count = nonnegative(row.get("error_count"));
            let total_requests = nonnegative(row.get("total_requests"));
            UsageDiagnosticsItem {
                estimated_cost: costs.get(&name).copied().unwrap_or_default(),
                name,
                request_count,
                success_count,
                error_count,
                error_rate: rate(error_count, request_count),
                request_share: rate(request_count, total_requests),
                latency_p95_ms: row.get("latency_p95_ms"),
            }
        })
        .collect();
    Ok(UsageDiagnosticsInsights {
        dimension: dimension.as_str().to_string(),
        items,
    })
}

/// 按 15 分钟聚合最终请求终态；客户端取消与调用方错误保持独立，不污染可用性。
pub async fn health_timeline(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> PgUsageRecordStoreResult<Vec<RequestHealthTimeBucket>> {
    let bucket = InsightsGranularity::QuarterHour.bucket_expression("terminal_at");
    let sql = format!(
        "with {TERMINAL_REQUESTS_CTE}\n\
         select {bucket} as bucket_start,\n\
           count(*) filter (where is_success)::bigint as success_requests,\n\
           count(*) filter (\n\
             where not is_success and not is_client_cancelled and not is_caller_error\n\
           )::bigint as failed_requests,\n\
           count(*) filter (where is_client_cancelled)::bigint as cancelled_requests,\n\
           count(*) filter (\n\
             where is_caller_error and not is_client_cancelled\n\
           )::bigint as caller_error_requests\n\
         from terminal_requests\n\
         group by bucket_start\n\
         order by bucket_start"
    );
    // SAFETY: 时间桶表达式与 CTE 都是模块内固定 SQL，时间范围仍使用绑定参数。
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|row| RequestHealthTimeBucket {
            bucket_start: row.get("bucket_start"),
            success_requests: nonnegative(row.get("success_requests")),
            failed_requests: nonnegative(row.get("failed_requests")),
            cancelled_requests: nonnegative(row.get("cancelled_requests")),
            caller_error_requests: nonnegative(row.get("caller_error_requests")),
        })
        .collect())
}

async fn load_health(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    granularity: InsightsGranularity,
) -> PgUsageRecordStoreResult<UsageHealthInsights> {
    let range = end.signed_duration_since(start);
    let previous_start = start.checked_sub_signed(range).unwrap_or(start);
    let bucket = granularity.bucket_expression("terminal_at");
    let sql = format!(
        "with {TERMINAL_REQUESTS_CTE}\n\
         select case when terminal_at >= $3 then 'current' else 'previous' end as period,\n\
           {bucket} as bucket_start,\n\
           count(*) filter (where is_success)::bigint as success_requests,\n\
           count(*) filter (\n\
             where not is_success and not is_client_cancelled and not is_caller_error\n\
           )::bigint as failed_requests,\n\
           count(*) filter (where is_client_cancelled)::bigint as cancelled_requests,\n\
           count(*) filter (\n\
             where is_caller_error and not is_client_cancelled\n\
           )::bigint as caller_error_requests\n\
         from terminal_requests\n\
         group by period, bucket_start\n\
         order by bucket_start"
    );
    // SAFETY: granularity selects one of three module-owned date_bin expressions.
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(previous_start)
        .bind(end)
        .bind(start)
        .fetch_all(pool)
        .await?;

    let mut current = HashMap::new();
    let mut current_success = 0_u64;
    let mut current_failed = 0_u64;
    let mut current_cancelled = 0_u64;
    let mut current_caller_error = 0_u64;
    let mut previous_success = 0_u64;
    let mut previous_failed = 0_u64;
    for row in rows {
        let success = nonnegative(row.get("success_requests"));
        let failed = nonnegative(row.get("failed_requests"));
        let cancelled = nonnegative(row.get("cancelled_requests"));
        let caller_error = nonnegative(row.get("caller_error_requests"));
        if row.get::<String, _>("period") == "current" {
            current_success = current_success.saturating_add(success);
            current_failed = current_failed.saturating_add(failed);
            current_cancelled = current_cancelled.saturating_add(cancelled);
            current_caller_error = current_caller_error.saturating_add(caller_error);
            current.insert(
                row.get::<DateTime<Utc>, _>("bucket_start"),
                (success, failed, cancelled, caller_error),
            );
        } else {
            previous_success = previous_success.saturating_add(success);
            previous_failed = previous_failed.saturating_add(failed);
        }
    }
    let total = current_success.saturating_add(current_failed);
    let previous_total = previous_success.saturating_add(previous_failed);
    let success_rate = rate(current_success, total);
    let previous_success_rate = rate(previous_success, previous_total);
    let points = bucket_starts(start, end, granularity)
        .into_iter()
        .map(|bucket| {
            let (success_requests, failed_requests, cancelled_requests, caller_error_requests) =
                current.get(&bucket).copied().unwrap_or_default();
            let point_total = success_requests.saturating_add(failed_requests);
            UsageHealthPoint {
                bucket,
                label: bucket_label(bucket, granularity),
                success_requests,
                failed_requests,
                cancelled_requests,
                caller_error_requests,
                error_rate: rate(failed_requests, point_total),
            }
        })
        .collect();
    Ok(UsageHealthInsights {
        total_requests: total,
        success_requests: current_success,
        failed_requests: current_failed,
        cancelled_requests: current_cancelled,
        caller_error_requests: current_caller_error,
        success_rate,
        request_change_rate: relative_change(total, previous_total),
        success_rate_change: (previous_total > 0).then_some(success_rate - previous_success_rate),
        points,
    })
}

async fn load_performance(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    granularity: InsightsGranularity,
) -> PgUsageRecordStoreResult<UsagePerformanceInsights> {
    let summary = sqlx::query(
        r#"
select
  count(*)::bigint as request_count,
  count(latency_ms)::bigint as latency_count,
  count(first_token_ms)::bigint as ttft_count,
  percentile_cont(0.50) within group (order by latency_ms) as latency_p50_ms,
  percentile_cont(0.95) within group (order by latency_ms) as latency_p95_ms,
  percentile_cont(0.99) within group (order by latency_ms) as latency_p99_ms,
  percentile_cont(0.50) within group (order by first_token_ms) as ttft_p50_ms,
  percentile_cont(0.95) within group (order by first_token_ms) as ttft_p95_ms,
  percentile_cont(0.99) within group (order by first_token_ms) as ttft_p99_ms
from usage_records
where created_at >= $1 and created_at < $2
"#,
    )
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await?;
    let bucket = granularity.bucket_expression("created_at");
    let point_sql = format!(
        "select {bucket} as bucket_start,\n\
           percentile_cont(0.50) within group (order by latency_ms) as latency_p50_ms,\n\
           percentile_cont(0.95) within group (order by latency_ms) as latency_p95_ms,\n\
           percentile_cont(0.99) within group (order by latency_ms) as latency_p99_ms,\n\
           percentile_cont(0.50) within group (order by first_token_ms) as ttft_p50_ms,\n\
           percentile_cont(0.95) within group (order by first_token_ms) as ttft_p95_ms,\n\
           percentile_cont(0.99) within group (order by first_token_ms) as ttft_p99_ms\n\
         from usage_records\n\
         where created_at >= $1 and created_at < $2\n\
         group by bucket_start order by bucket_start"
    );
    // SAFETY: granularity selects one of three module-owned date_bin expressions.
    let rows = sqlx::query(AssertSqlSafe(point_sql))
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;
    let mut by_bucket = HashMap::new();
    for row in rows {
        let bucket: DateTime<Utc> = row.get("bucket_start");
        by_bucket.insert(
            bucket,
            UsagePerformancePoint {
                bucket,
                label: bucket_label(bucket, granularity),
                latency_p50_ms: row.get("latency_p50_ms"),
                latency_p95_ms: row.get("latency_p95_ms"),
                latency_p99_ms: row.get("latency_p99_ms"),
                ttft_p50_ms: row.get("ttft_p50_ms"),
                ttft_p95_ms: row.get("ttft_p95_ms"),
                ttft_p99_ms: row.get("ttft_p99_ms"),
            },
        );
    }
    let points = bucket_starts(start, end, granularity)
        .into_iter()
        .map(|bucket| {
            by_bucket
                .remove(&bucket)
                .unwrap_or_else(|| UsagePerformancePoint {
                    bucket,
                    label: bucket_label(bucket, granularity),
                    ..Default::default()
                })
        })
        .collect();
    let request_count = nonnegative(summary.get("request_count"));
    Ok(UsagePerformanceInsights {
        latency_p50_ms: summary.get("latency_p50_ms"),
        latency_p95_ms: summary.get("latency_p95_ms"),
        latency_p99_ms: summary.get("latency_p99_ms"),
        ttft_p50_ms: summary.get("ttft_p50_ms"),
        ttft_p95_ms: summary.get("ttft_p95_ms"),
        ttft_p99_ms: summary.get("ttft_p99_ms"),
        latency_coverage: rate(nonnegative(summary.get("latency_count")), request_count),
        ttft_coverage: rate(nonnegative(summary.get("ttft_count")), request_count),
        points,
    })
}

async fn load_cost(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    granularity: InsightsGranularity,
) -> PgUsageRecordStoreResult<UsageCostInsights> {
    let bucket = granularity.bucket_expression("created_at");
    let sql = format!(
        "select {bucket} as bucket_start,\n\
           coalesce(nullif(upstream_model, ''), model) as billing_model,\n\
           service_tier, coalesce(input_tokens, 0) > $3 as is_long,\n\
           count(*)::bigint as request_count,\n\
           count(*) filter (where coalesce(input_tokens, 0) > 0)::bigint as cache_eligible_count,\n\
           count(*) filter (\n\
             where coalesce(input_tokens, 0) > 0 and coalesce(cached_tokens, 0) > 0\n\
           )::bigint as cache_hit_count,\n\
           coalesce(sum(input_tokens), 0)::bigint as input_tokens,\n\
           coalesce(sum(output_tokens), 0)::bigint as output_tokens,\n\
           coalesce(sum(least(coalesce(cached_tokens, 0), coalesce(input_tokens, 0))), 0)::bigint as cached_tokens\n\
         from usage_records\n\
         where created_at >= $1 and created_at < $2\n\
         group by bucket_start, billing_model, service_tier, is_long\n\
         order by bucket_start"
    );
    // SAFETY: granularity selects one of three module-owned date_bin expressions.
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(start)
        .bind(end)
        .bind(i64::try_from(LONG_CONTEXT_THRESHOLD).unwrap_or(i64::MAX))
        .fetch_all(pool)
        .await?;
    let mut total = CostAccumulator::default();
    let mut by_bucket = BTreeMap::<DateTime<Utc>, CostAccumulator>::new();
    for row in rows {
        let sample = cost_sample(&row);
        total.push(sample);
        by_bucket
            .entry(row.get("bucket_start"))
            .or_default()
            .push(sample);
    }
    let points = bucket_starts(start, end, granularity)
        .into_iter()
        .map(|bucket| {
            let item = by_bucket.remove(&bucket).unwrap_or_default();
            UsageCostPoint {
                bucket,
                label: bucket_label(bucket, granularity),
                input_tokens: item.input_tokens,
                output_tokens: item.output_tokens,
                cached_tokens: item.cached_tokens,
                total_tokens: item.total_tokens(),
                estimated_cost: item.estimated_cost,
                standard_cost: item.standard_cost,
                cached_token_rate: rate(item.cached_tokens, item.input_tokens),
                cache_hit_request_rate: rate(item.cache_hit_count, item.cache_eligible_count),
            }
        })
        .collect();
    Ok(UsageCostInsights {
        estimated_cost: total.estimated_cost,
        standard_cost: total.standard_cost,
        cost_per_request: amount_per(total.estimated_cost, total.request_count),
        tokens_per_request: amount_per(total.total_tokens() as f64, total.request_count),
        cached_token_rate: rate(total.cached_tokens, total.input_tokens),
        cache_hit_request_rate: rate(total.cache_hit_count, total.cache_eligible_count),
        input_tokens: total.input_tokens,
        output_tokens: total.output_tokens,
        cached_tokens: total.cached_tokens,
        total_tokens: total.total_tokens(),
        points,
    })
}

async fn load_diagnostic_costs(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    dimension: UsageDiagnosticsDimension,
) -> PgUsageRecordStoreResult<HashMap<String, f64>> {
    let expression = dimension.expression();
    let source = dimension.source();
    let sql = format!(
        "with {TERMINAL_REQUESTS_CTE}\n\
         select {expression} as name, billing_model, service_tier,\n\
           coalesce(input_tokens, 0) > $3 as is_long,\n\
           count(*)::bigint as request_count,\n\
           count(*) filter (where coalesce(input_tokens, 0) > 0)::bigint as cache_eligible_count,\n\
           count(*) filter (\n\
             where coalesce(input_tokens, 0) > 0 and coalesce(cached_tokens, 0) > 0\n\
           )::bigint as cache_hit_count,\n\
           coalesce(sum(input_tokens), 0)::bigint as input_tokens,\n\
           coalesce(sum(output_tokens), 0)::bigint as output_tokens,\n\
           coalesce(sum(least(coalesce(cached_tokens, 0), coalesce(input_tokens, 0))), 0)::bigint as cached_tokens\n\
         from {source}\n\
         where terminal_requests.is_success\n\
         group by name, billing_model, service_tier, is_long"
    );
    // SAFETY: the grouping expression comes from the closed diagnostics dimension enum.
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(start)
        .bind(end)
        .bind(i64::try_from(LONG_CONTEXT_THRESHOLD).unwrap_or(i64::MAX))
        .fetch_all(pool)
        .await?;
    let mut costs = HashMap::new();
    for row in rows {
        let name: String = row.get("name");
        let sample = cost_sample(&row);
        *costs.entry(name).or_insert(0.0) += sample.estimated_cost;
    }
    Ok(costs)
}

#[derive(Debug, Clone, Copy, Default)]
struct CostSample {
    request_count: u64,
    cache_eligible_count: u64,
    cache_hit_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    estimated_cost: f64,
    standard_cost: f64,
}

#[derive(Debug, Default)]
struct CostAccumulator {
    request_count: u64,
    cache_eligible_count: u64,
    cache_hit_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    estimated_cost: f64,
    standard_cost: f64,
}

impl CostAccumulator {
    fn push(&mut self, sample: CostSample) {
        self.request_count = self.request_count.saturating_add(sample.request_count);
        self.cache_eligible_count = self
            .cache_eligible_count
            .saturating_add(sample.cache_eligible_count);
        self.cache_hit_count = self.cache_hit_count.saturating_add(sample.cache_hit_count);
        self.input_tokens = self.input_tokens.saturating_add(sample.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(sample.output_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(sample.cached_tokens);
        self.estimated_cost += sample.estimated_cost;
        self.standard_cost += sample.standard_cost;
    }

    fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

fn cost_sample(row: &sqlx::postgres::PgRow) -> CostSample {
    let input_tokens = nonnegative(row.get("input_tokens"));
    let output_tokens = nonnegative(row.get("output_tokens"));
    let cached_tokens = nonnegative(row.get("cached_tokens")).min(input_tokens);
    let billing_model: String = row.get("billing_model");
    let service_tier: Option<String> = row.get("service_tier");
    let billing = calculate_aggregate_billing(
        input_tokens,
        output_tokens,
        cached_tokens,
        &billing_model,
        service_tier.as_deref(),
        row.get("is_long"),
    );
    CostSample {
        request_count: nonnegative(row.get("request_count")),
        cache_eligible_count: nonnegative(row.get("cache_eligible_count")),
        cache_hit_count: nonnegative(row.get("cache_hit_count")),
        input_tokens,
        output_tokens,
        cached_tokens,
        estimated_cost: billing.total_amount,
        standard_cost: billing.standard_amount,
    }
}

fn bucket_starts(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    granularity: InsightsGranularity,
) -> Vec<DateTime<Utc>> {
    let step = granularity.step_seconds();
    let offset = if granularity == InsightsGranularity::Day {
        8 * 60 * 60
    } else {
        0
    };
    let aligned = (start.timestamp() + offset).div_euclid(step) * step - offset;
    let Some(mut cursor) = DateTime::<Utc>::from_timestamp(aligned, 0) else {
        return Vec::new();
    };
    let mut points = Vec::new();
    while cursor < end {
        points.push(cursor);
        let Some(next) = cursor.checked_add_signed(Duration::seconds(step)) else {
            break;
        };
        cursor = next;
    }
    points
}

fn bucket_label(bucket: DateTime<Utc>, granularity: InsightsGranularity) -> String {
    let china = bucket + Duration::hours(8);
    match granularity {
        InsightsGranularity::Day => china.format("%Y-%m-%d").to_string(),
        InsightsGranularity::QuarterHour | InsightsGranularity::Hour => {
            china.format("%m-%d %H:%M").to_string()
        }
    }
}

fn nonnegative(value: Option<i64>) -> u64 {
    value.unwrap_or_default().max(0) as u64
}

fn rate(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn amount_per(amount: f64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        amount / count as f64
    }
}

fn relative_change(current: u64, previous: u64) -> Option<f64> {
    (previous > 0).then(|| (current as f64 - previous as f64) / previous as f64)
}

/// 返回缺省观测时间范围。
pub fn default_time_range(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    (now - Duration::days(DEFAULT_RANGE_DAYS), now)
}
