//! PostgreSQL 账号用量查询存储。

use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, PgPool, Postgres, QueryBuilder, Row};
use thiserror::Error;

use crate::infra::{
    json::{decode_cursor, encode_cursor, Page},
    time::parse_rfc3339_utc as parse_rfc3339,
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
  au.last_used_at,
  coalesce(au.last_used_at, 'epoch'::timestamptz) as sort_last_used_at
from account_usage au
left join accounts a on a.id = au.account_id";

const USAGE_SUMMARY_SQL: &str = r"
select
  count(*) as account_count,
  coalesce(sum(request_count), 0)::bigint as request_count,
  coalesce(sum(empty_response_count), 0)::bigint as empty_response_count,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
  coalesce(sum(reasoning_tokens), 0)::bigint as reasoning_tokens,
  coalesce(sum(total_tokens), 0)::bigint as total_tokens,
  coalesce(sum(image_input_tokens), 0)::bigint as image_input_tokens,
  coalesce(sum(image_output_tokens), 0)::bigint as image_output_tokens,
  coalesce(sum(image_request_count), 0)::bigint as image_request_count,
  coalesce(sum(image_request_failed_count), 0)::bigint as image_request_failed_count
from account_usage";

/// PostgreSQL 用量存储错误。
#[derive(Debug, Error)]
pub enum PgAccountUsageStoreError {
    /// 数据库错误。
    #[error("PostgreSQL usage store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// 时间格式错误。
    #[error("PostgreSQL usage store timestamp error: {0}")]
    Timestamp(#[from] chrono::ParseError),
    /// 分页游标非法。
    #[error("invalid usage pagination cursor")]
    InvalidCursor,
}

/// PostgreSQL 用量存储结果。
pub type PgAccountUsageStoreResult<T> = Result<T, PgAccountUsageStoreError>;

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

/// PostgreSQL 用量存储。
#[derive(Clone)]
pub struct PgAccountUsageStore {
    pool: PgPool,
}

impl PgAccountUsageStore {
    /// 构造存储。
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 分页列出账号用量。
    pub async fn list_usage(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> PgAccountUsageStoreResult<Page<UsageListRecord>> {
        let limit = limit.clamp(1, 200);
        let fetch_limit = i64::from(limit) + 1;
        let mut builder = QueryBuilder::<Postgres>::new(LIST_USAGE_SELECT_SQL);
        if let Some(cursor) = cursor {
            let (last_used_at, account_id) =
                decode_cursor(&cursor).ok_or(PgAccountUsageStoreError::InvalidCursor)?;
            let last_used_at = parse_rfc3339(&last_used_at)
                .map_err(|_| PgAccountUsageStoreError::InvalidCursor)?;
            builder.push("\nwhere coalesce(au.last_used_at, 'epoch'::timestamptz) < ");
            builder.push_bind(last_used_at);
            builder.push("\n   or (coalesce(au.last_used_at, 'epoch'::timestamptz) = ");
            builder.push_bind(last_used_at);
            builder.push(" and au.account_id < ");
            builder.push_bind(account_id);
            builder.push(")");
        }
        builder.push(
            "\norder by coalesce(au.last_used_at, 'epoch'::timestamptz) desc, au.account_id desc\nlimit ",
        );
        builder.push_bind(fetch_limit);

        let rows = builder.build().fetch_all(&self.pool).await?;
        Ok(to_page(&rows, limit))
    }

    /// 按账号 ID 批量读取账号用量。
    pub async fn list_usage_by_account_ids(
        &self,
        account_ids: &[String],
    ) -> PgAccountUsageStoreResult<Vec<UsageListRecord>> {
        if account_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut builder = QueryBuilder::<Postgres>::new(LIST_USAGE_SELECT_SQL);
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
    pub async fn usage_summary(&self) -> PgAccountUsageStoreResult<UsageSummary> {
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
}

fn usage_list_from_row(row: &PgRow) -> PgAccountUsageStoreResult<UsageListRecord> {
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
        window_started_at: row.get("window_started_at"),
        window_reset_at: row.get("window_reset_at"),
        limit_window_seconds: row
            .get::<Option<i64>, _>("limit_window_seconds")
            .and_then(|value| u64::try_from(value).ok()),
        last_used_at: row.get("last_used_at"),
    })
}

fn to_page(rows: &[PgRow], limit: u32) -> Page<UsageListRecord> {
    let has_more = rows.len() > limit as usize;
    let mut items = Vec::with_capacity(limit as usize);
    let mut last_row: Option<&PgRow> = None;
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
            let ts = row
                .get::<DateTime<Utc>, _>("sort_last_used_at")
                .to_rfc3339();
            let id: String = row.get("account_id");
            encode_cursor(&ts, &id)
        })
    } else {
        None
    };
    Page { items, next_cursor }
}
