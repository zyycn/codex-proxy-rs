//! SQLite 用量存储。

use chrono::{DateTime, Utc};
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use thiserror::Error;

use crate::infra::json::{decode_cursor, encode_cursor, page_offset, NumberedPage, Page};

const LIST_USAGE_AFTER_CURSOR_SQL: &str = r"
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
  au.last_used_at
from account_usage au
left join accounts a on a.id = au.account_id
where au.last_used_at < ?
   or (au.last_used_at = ? and au.account_id < ?)
order by au.last_used_at desc, au.account_id desc
limit ?";

const LIST_USAGE_SQL: &str = r"
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
  au.last_used_at
from account_usage au
left join accounts a on a.id = au.account_id
order by au.last_used_at desc, au.account_id desc
limit ?";

const LIST_USAGE_PAGE_SQL: &str = r"
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
  au.last_used_at
from account_usage au
left join accounts a on a.id = au.account_id
order by au.last_used_at desc, au.account_id desc
limit ? offset ?";

const COUNT_USAGE_SQL: &str = "select count(*) from account_usage";

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

/// 账号用量列表记录（不含窗口用量）。
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

    /// 返回底层连接池。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 分页列出账号用量。
    pub async fn list_usage(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteUsageStoreResult<Page<UsageListRecord>> {
        let limit = limit.clamp(1, 200);
        if let Some(cursor) = cursor {
            let (last_used_at, account_id) =
                decode_cursor(&cursor).ok_or(SqliteUsageStoreError::InvalidCursor)?;
            let rows = sqlx::query(LIST_USAGE_AFTER_CURSOR_SQL)
                .bind(&last_used_at)
                .bind(&last_used_at)
                .bind(&account_id)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(rows, limit))
        } else {
            let rows = sqlx::query(LIST_USAGE_SQL)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(rows, limit))
        }
    }

    /// 按页码列出账号用量。
    pub async fn list_usage_page(
        &self,
        page: u32,
        page_size: u32,
    ) -> SqliteUsageStoreResult<NumberedPage<UsageListRecord>> {
        let page_size = page_size.clamp(1, 200);
        let offset = page_offset(page, page_size);
        let rows = sqlx::query(LIST_USAGE_PAGE_SQL)
            .bind(i64::from(page_size))
            .bind(offset.min(i64::MAX as u64) as i64)
            .fetch_all(&self.pool)
            .await?;
        let items = rows
            .iter()
            .map(usage_list_from_row)
            .collect::<SqliteUsageStoreResult<Vec<_>>>()?;
        let (total,): (i64,) = sqlx::query_as(COUNT_USAGE_SQL)
            .fetch_one(&self.pool)
            .await?;

        Ok(NumberedPage {
            items,
            total: total.max(0) as u64,
            page: page.max(1),
            page_size,
        })
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
        last_used_at: parse_optional_rfc3339(row.get::<Option<String>, _>("last_used_at"))?,
    })
}

fn parse_optional_rfc3339(value: Option<String>) -> SqliteUsageStoreResult<Option<DateTime<Utc>>> {
    value
        .as_deref()
        .map(|value| Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc)))
        .transpose()
}

fn to_page(rows: Vec<SqliteRow>, limit: u32) -> Page<UsageListRecord> {
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
