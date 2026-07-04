//! SQLite 用量存储。

use chrono::{DateTime, Utc};
use sqlx::{sqlite::SqliteRow, QueryBuilder, Row, Sqlite, SqlitePool};
use thiserror::Error;

use crate::infra::{
    json::{decode_cursor, encode_cursor, Page},
    time::{
        parse_optional_rfc3339_utc as parse_optional_rfc3339, parse_rfc3339_utc as parse_rfc3339,
    },
};

const LIST_USAGE_SELECT_SQL: &str = r"
select
  au.account_id,
  a.email,
  a.label,
  a.plan_type,
  au.request_count,
  au.empty_response_count,
  au.input_tokens,
  au.output_tokens,
  au.cached_tokens,
  au.reasoning_tokens,
  au.total_tokens,
  au.image_input_tokens,
  au.image_output_tokens,
  au.image_request_count,
  au.image_request_failed_count,
  au.window_request_count,
  au.window_input_tokens,
  au.window_output_tokens,
  au.window_cached_tokens,
  au.window_started_at,
  au.window_reset_at,
  au.limit_window_seconds,
  au.last_used_at
from account_usage au
left join accounts a on a.id = au.account_id";

const USAGE_SUMMARY_SQL: &str = r"
select
  count(*) as account_count,
  coalesce(sum(request_count), 0) as request_count,
  coalesce(sum(empty_response_count), 0) as empty_response_count,
  coalesce(sum(input_tokens), 0) as input_tokens,
  coalesce(sum(output_tokens), 0) as output_tokens,
  coalesce(sum(cached_tokens), 0) as cached_tokens,
  coalesce(sum(reasoning_tokens), 0) as reasoning_tokens,
  coalesce(sum(total_tokens), 0) as total_tokens,
  coalesce(sum(image_input_tokens), 0) as image_input_tokens,
  coalesce(sum(image_output_tokens), 0) as image_output_tokens,
  coalesce(sum(image_request_count), 0) as image_request_count,
  coalesce(sum(image_request_failed_count), 0) as image_request_failed_count
from account_usage";

const LIST_USAGE_TIME_BUCKETS_SQL: &str = r"
select
  bucket_start,
  model,
  service_tier,
  coalesce(sum(request_count), 0) as request_count,
  coalesce(sum(error_count), 0) as error_count,
  coalesce(sum(input_tokens), 0) as input_tokens,
  coalesce(sum(output_tokens), 0) as output_tokens,
  coalesce(sum(cached_tokens), 0) as cached_tokens,
  coalesce(sum(first_token_latency_sum), 0) as first_token_latency_sum,
  coalesce(sum(first_token_latency_count), 0) as first_token_latency_count,
  coalesce(sum(latency_sum), 0) as latency_sum,
  coalesce(sum(latency_count), 0) as latency_count,
  coalesce(max(max_latency_ms), 0) as max_latency_ms,
  coalesce(min(nullif(min_latency_ms, 0)), 0) as min_latency_ms
from usage_time_buckets
where bucket_start >= ? and bucket_start <= ?
group by bucket_start, model, service_tier
order by bucket_start asc, model asc, service_tier asc";

/// SQLite 用量存储错误。
#[derive(Debug, Error)]
pub enum SqliteUsageStoreError {
    /// 数据库错误。
    #[error("sqlite usage store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// 时间格式错误。
    #[error("sqlite usage store timestamp error: {0}")]
    Timestamp(#[from] chrono::ParseError),
    /// 分页游标非法。
    #[error("invalid usage pagination cursor")]
    InvalidCursor,
}

/// SQLite 用量存储结果。
pub type SqliteUsageStoreResult<T> = Result<T, SqliteUsageStoreError>;

/// 账号用量列表记录。
#[derive(Debug, Clone)]
pub struct UsageListRecord {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// 历史请求总数。
    pub request_count: i64,
    /// 历史空响应次数。
    pub empty_response_count: i64,
    /// 累计输入 token。
    pub input_tokens: i64,
    /// 累计输出 token。
    pub output_tokens: i64,
    /// 累计缓存 token。
    pub cached_tokens: i64,
    /// 累计 reasoning token。
    pub reasoning_tokens: i64,
    /// 累计总 token。
    pub total_tokens: i64,
    /// 累计图片输入 token。
    pub image_input_tokens: i64,
    /// 累计图片输出 token。
    pub image_output_tokens: i64,
    /// 累计图片请求成功次数。
    pub image_request_count: i64,
    /// 累计图片请求失败次数。
    pub image_request_failed_count: i64,
    /// 当前额度窗口请求数。
    pub window_request_count: i64,
    /// 当前额度窗口输入 token。
    pub window_input_tokens: i64,
    /// 当前额度窗口输出 token。
    pub window_output_tokens: i64,
    /// 当前额度窗口缓存 token。
    pub window_cached_tokens: i64,
    /// 当前额度窗口开始时间。
    pub window_started_at: Option<DateTime<Utc>>,
    /// 当前额度窗口重置时间。
    pub window_reset_at: Option<DateTime<Utc>>,
    /// 当前额度窗口大小（秒）。
    pub limit_window_seconds: Option<u64>,
    /// 最近使用时间。
    pub last_used_at: Option<DateTime<Utc>>,
}

/// 账号用量汇总。
#[derive(Debug, Clone)]
pub struct UsageSummary {
    /// 账号数。
    pub account_count: i64,
    /// 总请求数。
    pub request_count: i64,
    /// 总空响应数。
    pub empty_response_count: i64,
    /// 总输入 token。
    pub input_tokens: i64,
    /// 总输出 token。
    pub output_tokens: i64,
    /// 总缓存 token。
    pub cached_tokens: i64,
    /// 总 reasoning token。
    pub reasoning_tokens: i64,
    /// 总 token。
    pub total_tokens: i64,
    /// 总图片输入 token。
    pub image_input_tokens: i64,
    /// 总图片输出 token。
    pub image_output_tokens: i64,
    /// 总图片请求成功数。
    pub image_request_count: i64,
    /// 总图片请求失败数。
    pub image_request_failed_count: i64,
}

/// 时间桶聚合用量记录。
#[derive(Debug, Clone)]
pub struct UsageTimeBucketRecord {
    /// 桶开始时间。
    pub bucket_start: DateTime<Utc>,
    /// 模型。
    pub model: String,
    /// 服务层级。
    pub service_tier: Option<String>,
    /// 请求数。
    pub request_count: i64,
    /// 错误数。
    pub error_count: i64,
    /// 输入 token。
    pub input_tokens: i64,
    /// 输出 token。
    pub output_tokens: i64,
    /// 缓存 token。
    pub cached_tokens: i64,
    /// 首 token 延迟总和。
    pub first_token_latency_sum: i64,
    /// 首 token 延迟样本数。
    pub first_token_latency_count: i64,
    /// 完成延迟总和。
    pub latency_sum: i64,
    /// 完成延迟样本数。
    pub latency_count: i64,
    /// 最大完成延迟。
    pub max_latency_ms: i64,
    /// 最小完成延迟。
    pub min_latency_ms: i64,
}

/// SQLite 用量存储。
#[derive(Clone)]
pub struct SqliteUsageStore {
    pool: SqlitePool,
}

impl SqliteUsageStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 分页列出账号用量。
    pub async fn list_usage(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteUsageStoreResult<Page<UsageListRecord>> {
        let limit = limit.clamp(1, 200);
        let fetch_limit = i64::from(limit) + 1;
        let mut builder = QueryBuilder::<Sqlite>::new(LIST_USAGE_SELECT_SQL);
        if let Some(cursor) = cursor {
            let (last_used_at, account_id) =
                decode_cursor(&cursor).ok_or(SqliteUsageStoreError::InvalidCursor)?;
            builder.push("\nwhere au.last_used_at < ");
            builder.push_bind(last_used_at.clone());
            builder.push("\n   or (au.last_used_at = ");
            builder.push_bind(last_used_at);
            builder.push(" and au.account_id < ");
            builder.push_bind(account_id);
            builder.push(")");
        }
        builder.push("\norder by au.last_used_at desc, au.account_id desc\nlimit ");
        builder.push_bind(fetch_limit);

        let rows = builder.build().fetch_all(&self.pool).await?;
        Ok(to_page(&rows, limit))
    }

    /// 按账号 ID 批量读取账号用量。
    pub async fn list_usage_by_account_ids(
        &self,
        account_ids: &[String],
    ) -> SqliteUsageStoreResult<Vec<UsageListRecord>> {
        if account_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut builder = QueryBuilder::<Sqlite>::new(LIST_USAGE_SELECT_SQL);
        builder.push("\nwhere au.account_id in (");
        let mut separated = builder.separated(", ");
        for account_id in account_ids {
            separated.push_bind(account_id);
        }
        separated.push_unseparated(")");

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.iter().map(usage_list_from_row).collect()
    }

    /// 汇总账号用量。
    pub async fn usage_summary(&self) -> SqliteUsageStoreResult<UsageSummary> {
        let row = sqlx::query(USAGE_SUMMARY_SQL).fetch_one(&self.pool).await?;
        Ok(UsageSummary {
            account_count: row.get("account_count"),
            request_count: row.get("request_count"),
            empty_response_count: row.get("empty_response_count"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cached_tokens: row.get("cached_tokens"),
            reasoning_tokens: row.get("reasoning_tokens"),
            total_tokens: row.get("total_tokens"),
            image_input_tokens: row.get("image_input_tokens"),
            image_output_tokens: row.get("image_output_tokens"),
            image_request_count: row.get("image_request_count"),
            image_request_failed_count: row.get("image_request_failed_count"),
        })
    }

    /// 列出指定时间范围内的时间桶聚合用量。
    pub async fn list_time_buckets(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> SqliteUsageStoreResult<Vec<UsageTimeBucketRecord>> {
        let rows = sqlx::query(LIST_USAGE_TIME_BUCKETS_SQL)
            .bind(start.to_rfc3339())
            .bind(end.to_rfc3339())
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(usage_time_bucket_from_row).collect()
    }
}

fn usage_list_from_row(row: &SqliteRow) -> SqliteUsageStoreResult<UsageListRecord> {
    Ok(UsageListRecord {
        account_id: row.get("account_id"),
        email: row.get("email"),
        label: row.get("label"),
        plan_type: row.get("plan_type"),
        request_count: row.get("request_count"),
        empty_response_count: row.get("empty_response_count"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        reasoning_tokens: row.get("reasoning_tokens"),
        total_tokens: row.get("total_tokens"),
        image_input_tokens: row.get("image_input_tokens"),
        image_output_tokens: row.get("image_output_tokens"),
        image_request_count: row.get("image_request_count"),
        image_request_failed_count: row.get("image_request_failed_count"),
        window_request_count: row.get("window_request_count"),
        window_input_tokens: row.get("window_input_tokens"),
        window_output_tokens: row.get("window_output_tokens"),
        window_cached_tokens: row.get("window_cached_tokens"),
        window_started_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("window_started_at").as_deref(),
        )?,
        window_reset_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("window_reset_at").as_deref(),
        )?,
        limit_window_seconds: row
            .get::<Option<i64>, _>("limit_window_seconds")
            .and_then(|value| u64::try_from(value).ok()),
        last_used_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("last_used_at").as_deref(),
        )?,
    })
}

fn usage_time_bucket_from_row(row: &SqliteRow) -> SqliteUsageStoreResult<UsageTimeBucketRecord> {
    let service_tier = row.get::<String, _>("service_tier").trim().to_string();
    Ok(UsageTimeBucketRecord {
        bucket_start: parse_rfc3339(row.get::<&str, _>("bucket_start"))?,
        model: row.get("model"),
        service_tier: (!service_tier.is_empty()).then_some(service_tier),
        request_count: row.get("request_count"),
        error_count: row.get("error_count"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        first_token_latency_sum: row.get("first_token_latency_sum"),
        first_token_latency_count: row.get("first_token_latency_count"),
        latency_sum: row.get("latency_sum"),
        latency_count: row.get("latency_count"),
        max_latency_ms: row.get("max_latency_ms"),
        min_latency_ms: row.get("min_latency_ms"),
    })
}

fn to_page(rows: &[SqliteRow], limit: u32) -> Page<UsageListRecord> {
    let has_more = rows.len() > limit as usize;
    let mut items = Vec::with_capacity(limit as usize);
    let mut last_row: Option<&SqliteRow> = None;
    for (i, row) in rows.iter().enumerate() {
        if i >= limit as usize {
            break;
        }
        if let Ok(item) = usage_list_from_row(row) {
            items.push(item);
            last_row = Some(row);
        }
    }
    let next_cursor = if has_more {
        last_row.map(|row| {
            let ts: String = row.get("last_used_at");
            let id: String = row.get("account_id");
            encode_cursor(&ts, &id)
        })
    } else {
        None
    };
    Page { items, next_cursor }
}
