//! 使用记录存储实现（SQLite）。

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use thiserror::Error;

use crate::admin::monitoring::{
    billing,
    usage_record_model::{
        metadata_i64, metadata_service_tier, metadata_string, UsageRecord, UsageRecordLevel,
    },
};
use crate::infra::{
    format::optional_nonnegative_i64_to_u64,
    json::{decode_cursor, encode_cursor, page_offset, NumberedPage, Page},
    time::{china_quarter_hour_start, parse_rfc3339_utc as parse_rfc3339},
};

/// 使用记录保留天数。
pub const USAGE_RECORD_RETENTION_DAYS: i64 = 30;
/// 默认不保存请求/响应体，避免把用户内容写入诊断日志。
pub const DEFAULT_USAGE_RECORD_CAPTURE_BODY: bool = false;

/// SQLite 使用记录错误。
#[derive(Debug, Error)]
pub enum SqliteUsageRecordStoreError {
    /// 数据库错误。
    #[error("sqlite usage record database error: {0}")]
    Database(#[from] sqlx::Error),
    /// JSON 错误。
    #[error("sqlite usage record json error: {0}")]
    Json(#[from] serde_json::Error),
    /// 时间格式错误。
    #[error("sqlite usage record timestamp error: {0}")]
    Timestamp(#[from] chrono::ParseError),
    /// 事件等级非法。
    #[error("invalid event level: {0}")]
    InvalidLevel(String),
    /// 分页游标非法。
    #[error("invalid usage record pagination cursor")]
    InvalidCursor,
}

/// SQLite 使用记录结果。
pub type SqliteUsageRecordStoreResult<T> = Result<T, SqliteUsageRecordStoreError>;

const USAGE_RECORD_SERVICE_TIER_SQL: &str =
    "nullif(trim(json_extract(metadata_json, '$.serviceTier')), '')";
const USAGE_RECORD_SELECT_SQL: &str = "select id, request_id, kind, level, account_id, route, model, status_code, transport, attempt_index, upstream_status_code, failure_class, response_id, upstream_request_id, latency_ms, message, metadata_json, created_at from usage_records";

/// 使用记录查询过滤器。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageRecordFilter {
    /// 事件类别。
    pub kind: Option<String>,
    /// 事件等级。
    pub level: Option<UsageRecordLevel>,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 账号 ID。
    pub account_id: Option<String>,
    /// 路由。
    pub route: Option<String>,
    /// 模型。
    pub model: Option<String>,
    /// HTTP 状态码。
    pub status_code: Option<i64>,
    /// 上游传输方式。
    pub transport: Option<String>,
    /// 同一请求内的上游尝试序号。
    pub attempt_index: Option<i64>,
    /// 上游 HTTP 状态码。
    pub upstream_status_code: Option<i64>,
    /// 失败分类。
    pub failure_class: Option<String>,
    /// 上游响应 ID。
    pub response_id: Option<String>,
    /// 上游请求 ID。
    pub upstream_request_id: Option<String>,
    /// 搜索关键词。
    pub search: Option<String>,
    /// 起始时间。
    pub start_time: Option<DateTime<Utc>>,
    /// 结束时间。
    pub end_time: Option<DateTime<Utc>>,
}

/// SQLite 使用记录存储。
#[derive(Clone)]
pub struct SqliteUsageRecordStore {
    pool: SqlitePool,
}

impl SqliteUsageRecordStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 写入使用记录。
    pub async fn append(&self, event: &UsageRecord) -> SqliteUsageRecordStoreResult<()> {
        append_event(&self.pool, event).await
    }

    /// 按保留期清理使用记录。
    pub async fn trim_to_retention(&self, now: DateTime<Utc>) -> SqliteUsageRecordStoreResult<u64> {
        trim_to_retention(&self.pool, now).await
    }

    /// 分页查询使用记录。
    pub async fn list(
        &self,
        filter: UsageRecordFilter,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteUsageRecordStoreResult<Page<UsageRecord>> {
        let fetch_limit = i64::from(limit) + 1;
        let mut builder = QueryBuilder::<Sqlite>::new(USAGE_RECORD_SELECT_SQL);
        push_filter(&mut builder, &filter, cursor.as_deref())?;
        builder.push(" order by created_at desc, id desc limit ");
        builder.push_bind(fetch_limit);

        let rows = builder.build().fetch_all(&self.pool).await?;
        let has_next = rows.len() > limit as usize;
        let take_count = rows.len().min(limit as usize);
        let items = rows
            .into_iter()
            .take(take_count)
            .map(|row| usage_record_from_row(&row))
            .collect::<SqliteUsageRecordStoreResult<Vec<_>>>()?;
        let next_cursor = if has_next {
            items
                .last()
                .map(|event| encode_cursor(&event.created_at.to_rfc3339(), &event.id))
        } else {
            None
        };

        Ok(Page { items, next_cursor })
    }

    /// 按页码查询使用记录。
    pub async fn list_page(
        &self,
        filter: UsageRecordFilter,
        page: u32,
        page_size: u32,
    ) -> SqliteUsageRecordStoreResult<NumberedPage<UsageRecord>> {
        let page_size = page_size.clamp(1, 200);
        let total = count_usage_records(&self.pool, &filter).await?;
        let offset = page_offset(page, page_size);
        let mut builder = QueryBuilder::<Sqlite>::new(USAGE_RECORD_SELECT_SQL);
        push_filter(&mut builder, &filter, None)?;
        builder.push(" order by created_at desc, id desc limit ");
        builder.push_bind(i64::from(page_size));
        builder.push(" offset ");
        builder.push_bind(offset.min(i64::MAX as u64) as i64);

        let rows = builder.build().fetch_all(&self.pool).await?;
        let items = rows
            .iter()
            .map(usage_record_from_row)
            .collect::<SqliteUsageRecordStoreResult<Vec<_>>>()?;

        Ok(NumberedPage {
            items,
            total,
            page: page.max(1),
            page_size,
        })
    }

    /// 按 ID 读取使用记录。
    pub async fn get(&self, id: &str) -> SqliteUsageRecordStoreResult<Option<UsageRecord>> {
        let mut builder = QueryBuilder::<Sqlite>::new(USAGE_RECORD_SELECT_SQL);
        builder.push(" where id = ");
        builder.push_bind(id);
        let row = builder.build().fetch_optional(&self.pool).await?;
        row.map(|row| usage_record_from_row(&row)).transpose()
    }

    /// 读取使用记录关联账号的邮箱映射。
    pub async fn account_email_map(
        &self,
        items: &[UsageRecord],
    ) -> SqliteUsageRecordStoreResult<HashMap<String, String>> {
        usage_record_account_email_map(&self.pool, items).await
    }

    /// 汇总事件使用记录。
    pub async fn summary(
        &self,
        filter: UsageRecordFilter,
    ) -> SqliteUsageRecordStoreResult<UsageRecordSummary> {
        usage_summary(&self.pool, &filter).await
    }

    /// 按账号聚合使用记录。
    pub async fn account_usage(
        &self,
        filter: UsageRecordFilter,
        limit: u32,
    ) -> SqliteUsageRecordStoreResult<Vec<UsageRecordAccountUsage>> {
        usage_account_usage(&self.pool, &filter, limit).await
    }

    /// 按模型来源聚合使用记录分布。
    pub async fn model_distribution(
        &self,
        filter: UsageRecordFilter,
        source: UsageRecordModelSource,
    ) -> SqliteUsageRecordStoreResult<Vec<UsageRecordBreakdown>> {
        usage_model_distribution(&self.pool, &filter, source, 8).await
    }

    /// 按端点来源聚合使用记录分布。
    pub async fn endpoint_distribution(
        &self,
        filter: UsageRecordFilter,
        source: UsageRecordEndpointSource,
    ) -> SqliteUsageRecordStoreResult<Vec<UsageRecordBreakdown>> {
        usage_endpoint_distribution(&self.pool, &filter, source, 8).await
    }

    /// 聚合使用记录趋势。
    pub async fn trend(
        &self,
        filter: UsageRecordFilter,
    ) -> SqliteUsageRecordStoreResult<Vec<UsageRecordTrendPoint>> {
        usage_trend(&self.pool, &filter).await
    }

    /// 清空使用记录。
    pub async fn clear(&self) -> SqliteUsageRecordStoreResult<u64> {
        let result = sqlx::query("delete from usage_records")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[derive(Debug, Clone)]
struct UsageRecordSummaryFields {
    transport: Option<String>,
    attempt_index: Option<i64>,
    upstream_status_code: Option<i64>,
    failure_class: Option<String>,
    response_id: Option<String>,
    upstream_request_id: Option<String>,
}

fn usage_record_summary_fields(event: &UsageRecord) -> UsageRecordSummaryFields {
    UsageRecordSummaryFields {
        transport: event
            .transport
            .clone()
            .or_else(|| metadata_string(&event.metadata, &["transport"])),
        attempt_index: event
            .attempt_index
            .or_else(|| metadata_i64(&event.metadata, &["attemptIndex", "attempt_index"])),
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

async fn append_event(pool: &SqlitePool, event: &UsageRecord) -> SqliteUsageRecordStoreResult<()> {
    let summary = usage_record_summary_fields(event);
    sqlx::query(
        "insert into usage_records (id, request_id, kind, level, account_id, route, model, status_code, transport, attempt_index, upstream_status_code, failure_class, response_id, upstream_request_id, latency_ms, message, metadata_json, created_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event.id)
    .bind(&event.request_id)
    .bind(&event.kind)
    .bind(level_to_db(event.level))
    .bind(&event.account_id)
    .bind(&event.route)
    .bind(&event.model)
    .bind(event.status_code)
    .bind(summary.transport)
    .bind(summary.attempt_index)
    .bind(summary.upstream_status_code)
    .bind(summary.failure_class)
    .bind(summary.response_id)
    .bind(summary.upstream_request_id)
    .bind(event.latency_ms)
    .bind(&event.message)
    .bind(event.metadata.to_string())
    .bind(event.created_at.to_rfc3339())
    .execute(pool)
    .await?;
    append_usage_time_bucket(pool, event).await?;
    Ok(())
}

async fn append_usage_time_bucket(
    pool: &SqlitePool,
    event: &UsageRecord,
) -> SqliteUsageRecordStoreResult<()> {
    let bucket_start = china_quarter_hour_start(event.created_at);
    let error_count = i64::from(is_error_usage_record(event));
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
) values (?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
    .bind(error_count)
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

fn is_error_usage_record(event: &UsageRecord) -> bool {
    event.level == UsageRecordLevel::Error || event.status_code.is_some_and(|status| status >= 400)
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
) -> SqliteUsageRecordStoreResult<u64> {
    let cutoff = now - Duration::days(USAGE_RECORD_RETENTION_DAYS);
    let result = sqlx::query("delete from usage_records where created_at < ?")
        .bind(cutoff.to_rfc3339())
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

async fn count_usage_records(
    pool: &SqlitePool,
    filter: &UsageRecordFilter,
) -> SqliteUsageRecordStoreResult<u64> {
    let mut builder = QueryBuilder::<Sqlite>::new("select count(*) from usage_records");
    push_filter(&mut builder, filter, None)?;
    let (total,): (i64,) = builder.build_query_as().fetch_one(pool).await?;
    Ok(total.max(0).cast_unsigned())
}

async fn usage_summary(
    pool: &SqlitePool,
    filter: &UsageRecordFilter,
) -> SqliteUsageRecordStoreResult<UsageRecordSummary> {
    let mut builder = QueryBuilder::<Sqlite>::new(
        "select
            count(*) as total_requests,
            sum(case when level = 'error' or coalesce(status_code, 0) >= 400 then 1 else 0 end) as error_requests,
            sum(coalesce(cast(json_extract(metadata_json, '$.usage.inputTokens') as integer), cast(json_extract(metadata_json, '$.inputTokens') as integer), 0)) as input_tokens,
            sum(coalesce(cast(json_extract(metadata_json, '$.usage.outputTokens') as integer), cast(json_extract(metadata_json, '$.outputTokens') as integer), 0)) as output_tokens,
            sum(coalesce(cast(json_extract(metadata_json, '$.usage.cachedTokens') as integer), cast(json_extract(metadata_json, '$.cachedTokens') as integer), 0)) as cached_tokens,
            avg(case when latency_ms > 0 then latency_ms else null end) as average_latency_ms
        from usage_records",
    );
    push_filter(&mut builder, filter, None)?;

    let row = builder.build().fetch_one(pool).await?;
    let input_tokens = optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("input_tokens"));
    let output_tokens = optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("output_tokens"));
    let cached_tokens = optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("cached_tokens"));

    Ok(UsageRecordSummary {
        total_requests: optional_nonnegative_i64_to_u64(
            row.get::<Option<i64>, _>("total_requests"),
        ),
        error_requests: optional_nonnegative_i64_to_u64(
            row.get::<Option<i64>, _>("error_requests"),
        ),
        input_tokens,
        output_tokens,
        cached_tokens,
        total_tokens: input_tokens + output_tokens,
        average_latency_ms: row.get("average_latency_ms"),
    })
}

async fn usage_account_usage(
    pool: &SqlitePool,
    filter: &UsageRecordFilter,
    limit: u32,
) -> SqliteUsageRecordStoreResult<Vec<UsageRecordAccountUsage>> {
    let mut builder = QueryBuilder::<Sqlite>::new(
        "select
            account_id,
            sum(coalesce(cast(json_extract(metadata_json, '$.usage.inputTokens') as integer), cast(json_extract(metadata_json, '$.inputTokens') as integer), 0)) as input_tokens,
            sum(coalesce(cast(json_extract(metadata_json, '$.usage.outputTokens') as integer), cast(json_extract(metadata_json, '$.outputTokens') as integer), 0)) as output_tokens,
            max(created_at) as last_used_at
        from usage_records",
    );
    push_filter(&mut builder, filter, None)?;
    builder.push(" and account_id is not null and trim(account_id) <> ''");
    builder.push(" group by account_id order by last_used_at desc, account_id asc limit ");
    builder.push_bind(i64::from(limit.clamp(1, 50)));

    builder
        .build()
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| {
            let input_tokens =
                optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("input_tokens"));
            let output_tokens =
                optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("output_tokens"));
            Ok(UsageRecordAccountUsage {
                account_id: row.get("account_id"),
                input_tokens,
                output_tokens,
                total_tokens: input_tokens + output_tokens,
                last_used_at: parse_rfc3339(&row.get::<String, _>("last_used_at"))?,
            })
        })
        .collect()
}

async fn usage_record_account_email_map(
    pool: &SqlitePool,
    items: &[UsageRecord],
) -> SqliteUsageRecordStoreResult<HashMap<String, String>> {
    let mut account_ids = items
        .iter()
        .filter_map(|item| item.account_id.as_deref())
        .map(str::to_string)
        .collect::<Vec<_>>();
    account_ids.sort_unstable();
    account_ids.dedup();

    if account_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut builder = QueryBuilder::<Sqlite>::new("select id, email from accounts where id in (");
    let mut separated = builder.separated(", ");
    for account_id in &account_ids {
        separated.push_bind(account_id);
    }
    separated.push_unseparated(")");

    let rows = builder.build().fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.get::<Option<String>, _>("email")
                .map(|email| email.trim().to_string())
                .filter(|email| !email.is_empty())
                .map(|email| (row.get::<String, _>("id"), email))
        })
        .collect())
}

async fn usage_model_distribution(
    pool: &SqlitePool,
    filter: &UsageRecordFilter,
    source: UsageRecordModelSource,
    limit: u32,
) -> SqliteUsageRecordStoreResult<Vec<UsageRecordBreakdown>> {
    let (group_expr, group_alias) = match source {
        UsageRecordModelSource::Requested => (
            "coalesce(nullif(trim(json_extract(metadata_json, '$.requestedModel')), ''), nullif(trim(usage_records.model), ''), '未知模型')",
            "requested_model",
        ),
        UsageRecordModelSource::Upstream => (
            "coalesce(
                nullif(trim(json_extract(usage_records.metadata_json, '$.upstreamModel')), ''),
                nullif(trim(json_extract(usage_records.metadata_json, '$.requestedModel')), ''),
                nullif(trim(usage_records.model), ''),
                '未知模型'
            )",
            "model",
        ),
        UsageRecordModelSource::Mapping => (
            "case
                when coalesce(nullif(trim(json_extract(metadata_json, '$.requestedModel')), ''), nullif(trim(usage_records.model), '')) is not null
                then coalesce(nullif(trim(json_extract(metadata_json, '$.requestedModel')), ''), nullif(trim(usage_records.model), ''), '未知模型')
                  || ' -> ' ||
                  coalesce(nullif(trim(json_extract(metadata_json, '$.upstreamModel')), ''), nullif(trim(json_extract(metadata_json, '$.requestedModel')), ''), nullif(trim(usage_records.model), ''), '未知模型')
                else '未知模型'
            end",
            "model_mapping",
        ),
    };
    usage_breakdown(pool, filter, group_expr, group_alias, limit).await
}

async fn usage_endpoint_distribution(
    pool: &SqlitePool,
    filter: &UsageRecordFilter,
    source: UsageRecordEndpointSource,
    limit: u32,
) -> SqliteUsageRecordStoreResult<Vec<UsageRecordBreakdown>> {
    let (group_expr, group_alias) = match source {
        UsageRecordEndpointSource::Inbound => {
            ("coalesce(nullif(route, ''), '未知端点')", "endpoint")
        }
        UsageRecordEndpointSource::Upstream => (
            "coalesce(nullif(trim(json_extract(metadata_json, '$.upstreamEndpoint')), ''), nullif(trim(json_extract(metadata_json, '$.upstreamRoute')), ''), nullif(trim(json_extract(metadata_json, '$.upstreamPath')), ''), nullif(route, ''), '未知端点')",
            "upstream_endpoint",
        ),
        UsageRecordEndpointSource::Path => (
            "coalesce(nullif(route, ''), '未知端点') || ' -> ' || coalesce(nullif(trim(json_extract(metadata_json, '$.upstreamEndpoint')), ''), nullif(trim(json_extract(metadata_json, '$.upstreamRoute')), ''), nullif(trim(json_extract(metadata_json, '$.upstreamPath')), ''), nullif(route, ''), '未知端点')",
            "endpoint_path",
        ),
    };
    usage_breakdown(pool, filter, group_expr, group_alias, limit).await
}

async fn usage_breakdown(
    pool: &SqlitePool,
    filter: &UsageRecordFilter,
    group_expr: &str,
    group_alias: &str,
    limit: u32,
) -> SqliteUsageRecordStoreResult<Vec<UsageRecordBreakdown>> {
    let mut builder = QueryBuilder::<Sqlite>::new("select ");
    builder.push(group_expr);
    builder.push(" as ");
    builder.push(group_alias);
    builder.push(
        ", count(*) as request_count,
        sum(coalesce(cast(json_extract(metadata_json, '$.usage.inputTokens') as integer), cast(json_extract(metadata_json, '$.inputTokens') as integer), 0)) as input_tokens,
        sum(coalesce(cast(json_extract(metadata_json, '$.usage.outputTokens') as integer), cast(json_extract(metadata_json, '$.outputTokens') as integer), 0)) as output_tokens,
        sum(coalesce(cast(json_extract(metadata_json, '$.usage.cachedTokens') as integer), cast(json_extract(metadata_json, '$.cachedTokens') as integer), 0)) as cached_tokens,
        coalesce(
          nullif(trim(json_extract(metadata_json, '$.upstreamModel')), ''),
          nullif(trim(json_extract(metadata_json, '$.requestedModel')), ''),
          nullif(trim(usage_records.model), ''),
          '未知模型'
        ) as billing_model, ",
    );
    builder.push(USAGE_RECORD_SERVICE_TIER_SQL);
    builder.push(
        " as service_tier,
        avg(case when latency_ms > 0 then latency_ms else null end) as average_latency_ms
        from usage_records",
    );
    push_filter(&mut builder, filter, None)?;
    builder.push(" group by ");
    builder.push(group_alias);
    builder.push(", billing_model, service_tier");
    builder.push(" order by request_count desc, ");
    builder.push(group_alias);
    builder.push(" asc limit ");
    builder.push_bind(i64::from(limit.clamp(1, 50) * 8));

    let rows = builder.build().fetch_all(pool).await?;
    let mut items = Vec::<UsageRecordBreakdown>::new();
    for row in rows {
        let name: String = row.get(group_alias);
        let input_tokens =
            optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("input_tokens"));
        let output_tokens =
            optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("output_tokens"));
        let cached_tokens =
            optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("cached_tokens"));
        let cost = usage_breakdown_cost(
            input_tokens,
            output_tokens,
            cached_tokens,
            &row.get::<String, _>("billing_model"),
            row.get::<Option<String>, _>("service_tier").as_deref(),
        );
        let request_count =
            optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("request_count"));
        let average_latency_ms = row.get::<Option<f64>, _>("average_latency_ms");

        if let Some(item) = items.iter_mut().find(|item| item.name == name) {
            let previous_latency_count = item.request_count as f64;
            let next_latency_count = request_count as f64;
            item.request_count += request_count;
            item.input_tokens += input_tokens;
            item.output_tokens += output_tokens;
            item.cached_tokens += cached_tokens;
            item.total_tokens += input_tokens + output_tokens;
            item.cost += cost;
            item.actual_cost += cost;
            item.account_cost += cost;
            item.average_latency_ms = weighted_average_latency(
                item.average_latency_ms,
                previous_latency_count,
                average_latency_ms,
                next_latency_count,
            );
        } else {
            items.push(UsageRecordBreakdown {
                name,
                request_count,
                input_tokens,
                output_tokens,
                cached_tokens,
                total_tokens: input_tokens + output_tokens,
                cost,
                actual_cost: cost,
                account_cost: cost,
                average_latency_ms,
            });
        }
    }

    items.sort_by(|left, right| {
        right
            .request_count
            .cmp(&left.request_count)
            .then_with(|| left.name.cmp(&right.name))
    });
    items.truncate(limit.clamp(1, 50) as usize);
    Ok(items)
}

async fn usage_trend(
    pool: &SqlitePool,
    filter: &UsageRecordFilter,
) -> SqliteUsageRecordStoreResult<Vec<UsageRecordTrendPoint>> {
    let mut builder = QueryBuilder::<Sqlite>::new(
        "select
            substr(datetime(created_at, '+8 hours'), 1, 10) as date,
            coalesce(cast(json_extract(metadata_json, '$.usage.inputTokens') as integer), cast(json_extract(metadata_json, '$.inputTokens') as integer), 0) as input_tokens,
            coalesce(cast(json_extract(metadata_json, '$.usage.outputTokens') as integer), cast(json_extract(metadata_json, '$.outputTokens') as integer), 0) as output_tokens,
            coalesce(cast(json_extract(metadata_json, '$.usage.cacheCreationTokens') as integer), cast(json_extract(metadata_json, '$.cacheCreationTokens') as integer), cast(json_extract(metadata_json, '$.usage.cache_creation_tokens') as integer), cast(json_extract(metadata_json, '$.cache_creation_tokens') as integer), 0) as cache_creation_tokens,
            coalesce(cast(json_extract(metadata_json, '$.usage.cachedTokens') as integer), cast(json_extract(metadata_json, '$.cachedTokens') as integer), 0) as cached_tokens,
            coalesce(
              nullif(trim(json_extract(metadata_json, '$.upstreamModel')), ''),
              nullif(trim(json_extract(metadata_json, '$.requestedModel')), ''),
              nullif(trim(usage_records.model), ''),
              '未知模型'
            ) as billing_model, ",
    );
    builder.push(USAGE_RECORD_SERVICE_TIER_SQL);
    builder.push(
        " as service_tier,
            latency_ms
        from usage_records",
    );
    push_filter(&mut builder, filter, None)?;
    builder.push(" order by date asc, created_at asc");

    let rows = builder.build().fetch_all(pool).await?;
    let mut days = BTreeMap::<String, UsageTrendAccumulator>::new();

    for row in rows {
        let date: String = row.get("date");
        let input_tokens =
            optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("input_tokens"));
        let output_tokens =
            optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("output_tokens"));
        let cache_creation_tokens =
            optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("cache_creation_tokens"));
        let cached_tokens =
            optional_nonnegative_i64_to_u64(row.get::<Option<i64>, _>("cached_tokens"));
        let cost = usage_breakdown_cost(
            input_tokens,
            output_tokens,
            cached_tokens,
            &row.get::<String, _>("billing_model"),
            row.get::<Option<String>, _>("service_tier").as_deref(),
        );

        days.entry(date.clone())
            .or_insert_with(|| UsageTrendAccumulator::new(date))
            .push(
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cached_tokens,
                cost,
                row.get("latency_ms"),
            );
    }

    let mut points = days
        .into_values()
        .map(UsageTrendAccumulator::into_point)
        .collect::<Vec<_>>();
    if points.len() > 60 {
        points = points.split_off(points.len() - 60);
    }
    Ok(points)
}

#[derive(Debug)]
struct UsageTrendAccumulator {
    point: UsageRecordTrendPoint,
    latency_sum: f64,
    latency_count: u64,
}

impl UsageTrendAccumulator {
    fn new(date: String) -> Self {
        Self {
            point: UsageRecordTrendPoint {
                date,
                ..UsageRecordTrendPoint::default()
            },
            latency_sum: 0.0,
            latency_count: 0,
        }
    }

    fn push(
        &mut self,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cached_tokens: u64,
        cost: f64,
        latency_ms: Option<i64>,
    ) {
        self.point.input_tokens += input_tokens;
        self.point.output_tokens += output_tokens;
        self.point.cache_creation_tokens += cache_creation_tokens;
        self.point.cached_tokens += cached_tokens;
        self.point.total_tokens += input_tokens + output_tokens;
        self.point.cost += cost;
        self.point.actual_cost += cost;

        if let Some(latency_ms) = latency_ms.filter(|latency_ms| *latency_ms > 0) {
            self.latency_sum += latency_ms as f64;
            self.latency_count += 1;
        }
    }

    fn into_point(mut self) -> UsageRecordTrendPoint {
        self.point.average_latency_ms = if self.latency_count > 0 {
            Some(self.latency_sum / self.latency_count as f64)
        } else {
            None
        };
        self.point
    }
}

fn usage_breakdown_cost(
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    model: &str,
    service_tier: Option<&str>,
) -> f64 {
    billing::calculate_cost(
        input_tokens,
        output_tokens,
        cached_tokens,
        model,
        service_tier,
    )
}

fn weighted_average_latency(
    current_average: Option<f64>,
    current_count: f64,
    next_average: Option<f64>,
    next_count: f64,
) -> Option<f64> {
    match (current_average, next_average) {
        (Some(current), Some(next)) if current_count > 0.0 && next_count > 0.0 => {
            Some((current * current_count + next * next_count) / (current_count + next_count))
        }
        (Some(current), _) => Some(current),
        (_, Some(next)) => Some(next),
        _ => None,
    }
}

fn push_filter(
    builder: &mut QueryBuilder<Sqlite>,
    filter: &UsageRecordFilter,
    cursor: Option<&str>,
) -> SqliteUsageRecordStoreResult<()> {
    let mut separated = builder.separated(" and ");
    separated.push(" where 1 = 1");

    if let Some(kind) = filter.kind.as_deref() {
        separated.push("kind = ");
        separated.push_bind_unseparated(kind);
    }
    if let Some(level) = filter.level {
        separated.push("level = ");
        separated.push_bind_unseparated(level_to_db(level));
    }
    if let Some(request_id) = filter.request_id.as_deref() {
        separated.push("request_id = ");
        separated.push_bind_unseparated(request_id);
    }
    if let Some(account_id) = filter.account_id.as_deref() {
        separated.push("account_id = ");
        separated.push_bind_unseparated(account_id);
    }
    if let Some(route) = filter.route.as_deref() {
        separated.push("route = ");
        separated.push_bind_unseparated(route);
    }
    if let Some(model) = filter.model.as_deref() {
        separated.push("model = ");
        separated.push_bind_unseparated(model);
    }
    if let Some(status_code) = filter.status_code {
        separated.push("status_code = ");
        separated.push_bind_unseparated(status_code);
    }
    if let Some(transport) = filter.transport.as_deref() {
        separated.push("transport = ");
        separated.push_bind_unseparated(transport);
    }
    if let Some(attempt_index) = filter.attempt_index {
        separated.push("attempt_index = ");
        separated.push_bind_unseparated(attempt_index);
    }
    if let Some(upstream_status_code) = filter.upstream_status_code {
        separated.push("upstream_status_code = ");
        separated.push_bind_unseparated(upstream_status_code);
    }
    if let Some(failure_class) = filter.failure_class.as_deref() {
        separated.push("failure_class = ");
        separated.push_bind_unseparated(failure_class);
    }
    if let Some(response_id) = filter.response_id.as_deref() {
        separated.push("response_id = ");
        separated.push_bind_unseparated(response_id);
    }
    if let Some(upstream_request_id) = filter.upstream_request_id.as_deref() {
        separated.push("upstream_request_id = ");
        separated.push_bind_unseparated(upstream_request_id);
    }
    if let Some(search) = filter.search.as_deref() {
        let pattern = format!("%{search}%");
        separated.push("(message like ");
        separated.push_bind_unseparated(pattern.clone());
        separated.push_unseparated(" or metadata_json like ");
        separated.push_bind_unseparated(pattern);
        separated.push_unseparated(")");
    }
    if let Some(start_time) = filter.start_time {
        separated.push("created_at >= ");
        separated.push_bind_unseparated(start_time.to_rfc3339());
    }
    if let Some(end_time) = filter.end_time {
        separated.push("created_at <= ");
        separated.push_bind_unseparated(end_time.to_rfc3339());
    }
    if let Some(cursor) = cursor {
        let (created_at, id) =
            decode_cursor(cursor).ok_or(SqliteUsageRecordStoreError::InvalidCursor)?;
        separated.push("(created_at < ");
        separated.push_bind_unseparated(created_at.clone());
        separated.push_unseparated(" or (created_at = ");
        separated.push_bind_unseparated(created_at);
        separated.push_unseparated(" and id < ");
        separated.push_bind_unseparated(id);
        separated.push_unseparated("))");
    }

    Ok(())
}

fn usage_record_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> SqliteUsageRecordStoreResult<UsageRecord> {
    Ok(UsageRecord {
        id: row.get("id"),
        request_id: row.get("request_id"),
        kind: row.get("kind"),
        level: level_from_db(&row.get::<String, _>("level"))?,
        account_id: row.get("account_id"),
        route: row.get("route"),
        model: row.get("model"),
        status_code: row.get("status_code"),
        transport: row.get("transport"),
        attempt_index: row.get("attempt_index"),
        upstream_status_code: row.get("upstream_status_code"),
        failure_class: row.get("failure_class"),
        response_id: row.get("response_id"),
        upstream_request_id: row.get("upstream_request_id"),
        latency_ms: row.get("latency_ms"),
        message: row.get("message"),
        metadata: serde_json::from_str(&row.get::<String, _>("metadata_json"))?,
        created_at: parse_rfc3339(&row.get::<String, _>("created_at"))?,
    })
}

fn level_to_db(level: UsageRecordLevel) -> &'static str {
    match level {
        UsageRecordLevel::Debug => "debug",
        UsageRecordLevel::Info => "info",
        UsageRecordLevel::Warn => "warn",
        UsageRecordLevel::Error => "error",
    }
}

fn level_from_db(value: &str) -> SqliteUsageRecordStoreResult<UsageRecordLevel> {
    match value {
        "debug" => Ok(UsageRecordLevel::Debug),
        "info" => Ok(UsageRecordLevel::Info),
        "warn" => Ok(UsageRecordLevel::Warn),
        "error" => Ok(UsageRecordLevel::Error),
        other => Err(SqliteUsageRecordStoreError::InvalidLevel(other.to_string())),
    }
}

/// 使用记录汇总。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageRecordSummary {
    /// 请求总数。
    pub total_requests: u64,
    /// 错误请求数。
    pub error_requests: u64,
    /// 输入 Token。
    pub input_tokens: u64,
    /// 输出 Token。
    pub output_tokens: u64,
    /// 缓存命中 Token。
    pub cached_tokens: u64,
    /// 总 Token。
    pub total_tokens: u64,
    /// 平均耗时。
    pub average_latency_ms: Option<f64>,
}

/// 账号使用记录聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecordAccountUsage {
    /// 账号 ID。
    pub account_id: String,
    /// 输入 Token。
    pub input_tokens: u64,
    /// 输出 Token。
    pub output_tokens: u64,
    /// 总 Token。
    pub total_tokens: u64,
    /// 最近使用时间。
    pub last_used_at: DateTime<Utc>,
}

/// 模型分布来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRecordModelSource {
    /// 客户端请求模型。
    Requested,
    /// 实际上游模型。
    Upstream,
    /// 请求模型到上游模型的映射。
    Mapping,
}

/// 端点分布来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRecordEndpointSource {
    /// 入站端点。
    Inbound,
    /// 上游端点。
    Upstream,
    /// 入站到上游路径映射。
    Path,
}

/// 使用记录分布项。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageRecordBreakdown {
    /// 名称。
    pub name: String,
    /// 请求数。
    pub request_count: u64,
    /// 输入 Token。
    pub input_tokens: u64,
    /// 输出 Token。
    pub output_tokens: u64,
    /// 缓存命中 Token。
    pub cached_tokens: u64,
    /// 总 Token。
    pub total_tokens: u64,
    /// 标准成本。
    pub cost: f64,
    /// 实际成本。
    pub actual_cost: f64,
    /// 账号成本。
    pub account_cost: f64,
    /// 平均耗时。
    pub average_latency_ms: Option<f64>,
}

/// 使用记录趋势点。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageRecordTrendPoint {
    /// 日期。
    pub date: String,
    /// 输入 Token。
    pub input_tokens: u64,
    /// 输出 Token。
    pub output_tokens: u64,
    /// 缓存创建 Token。
    pub cache_creation_tokens: u64,
    /// 缓存命中 Token。
    pub cached_tokens: u64,
    /// 总 Token。
    pub total_tokens: u64,
    /// 标准成本。
    pub cost: f64,
    /// 实际成本。
    pub actual_cost: f64,
    /// 平均耗时。
    pub average_latency_ms: Option<f64>,
}
