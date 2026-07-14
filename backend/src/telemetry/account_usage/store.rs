//! PostgreSQL 账号用量查询存储。

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row, postgres::PgRow};
use thiserror::Error;

use crate::infra::format::nonnegative_i64_to_u64;

const RECORD_USAGE_SQL: &str = r"
insert into account_usage (
  account_id, request_count, empty_response_count,
  input_tokens, output_tokens, cached_tokens, reasoning_tokens, total_tokens,
  image_input_tokens, image_output_tokens, image_request_count, image_request_failed_count,
  window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens,
  window_image_input_tokens, window_image_output_tokens,
  window_image_request_count, window_image_request_failed_count,
  window_started_at, last_used_at
) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22)
on conflict(account_id) do update set
  request_count = account_usage.request_count + excluded.request_count,
  empty_response_count = account_usage.empty_response_count + excluded.empty_response_count,
  input_tokens = account_usage.input_tokens + excluded.input_tokens,
  output_tokens = account_usage.output_tokens + excluded.output_tokens,
  cached_tokens = account_usage.cached_tokens + excluded.cached_tokens,
  reasoning_tokens = account_usage.reasoning_tokens + excluded.reasoning_tokens,
  total_tokens = account_usage.total_tokens + excluded.total_tokens,
  image_input_tokens = account_usage.image_input_tokens + excluded.image_input_tokens,
  image_output_tokens = account_usage.image_output_tokens + excluded.image_output_tokens,
  image_request_count = account_usage.image_request_count + excluded.image_request_count,
  image_request_failed_count = account_usage.image_request_failed_count + excluded.image_request_failed_count,
  window_request_count = account_usage.window_request_count + excluded.window_request_count,
  window_input_tokens = account_usage.window_input_tokens + excluded.window_input_tokens,
  window_output_tokens = account_usage.window_output_tokens + excluded.window_output_tokens,
  window_cached_tokens = account_usage.window_cached_tokens + excluded.window_cached_tokens,
  window_image_input_tokens = account_usage.window_image_input_tokens + excluded.window_image_input_tokens,
  window_image_output_tokens = account_usage.window_image_output_tokens + excluded.window_image_output_tokens,
  window_image_request_count = account_usage.window_image_request_count + excluded.window_image_request_count,
  window_image_request_failed_count = account_usage.window_image_request_failed_count + excluded.window_image_request_failed_count,
  window_started_at = coalesce(account_usage.window_started_at, excluded.window_started_at),
  last_used_at = excluded.last_used_at";

const SYNC_RUNTIME_WINDOW_SQL: &str = r"
insert into account_usage (
  account_id, window_request_count, window_input_tokens, window_output_tokens,
  window_cached_tokens, window_image_input_tokens, window_image_output_tokens,
  window_image_request_count, window_image_request_failed_count,
  window_started_at, window_reset_at, limit_window_seconds
) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
on conflict(account_id) do update set
  window_request_count = excluded.window_request_count,
  window_input_tokens = excluded.window_input_tokens,
  window_output_tokens = excluded.window_output_tokens,
  window_cached_tokens = excluded.window_cached_tokens,
  window_image_input_tokens = excluded.window_image_input_tokens,
  window_image_output_tokens = excluded.window_image_output_tokens,
  window_image_request_count = excluded.window_image_request_count,
  window_image_request_failed_count = excluded.window_image_request_failed_count,
  window_started_at = excluded.window_started_at,
  window_reset_at = excluded.window_reset_at,
  limit_window_seconds = excluded.limit_window_seconds
where account_usage.window_reset_at is null
  or account_usage.window_reset_at <= excluded.window_reset_at
  or account_usage.window_reset_at <= $13";

const SYNC_RATE_LIMIT_WINDOW_SQL: &str = r"
insert into account_usage (account_id, window_reset_at, limit_window_seconds)
values ($1, $2, $3)
on conflict(account_id) do update set
  (
    window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens,
    window_image_input_tokens, window_image_output_tokens,
    window_image_request_count, window_image_request_failed_count,
    window_started_at, window_reset_at, limit_window_seconds
  ) = (
    select
      case when should_reset then 0 else account_usage.window_request_count end,
      case when should_reset then 0 else account_usage.window_input_tokens end,
      case when should_reset then 0 else account_usage.window_output_tokens end,
      case when should_reset then 0 else account_usage.window_cached_tokens end,
      case when should_reset then 0 else account_usage.window_image_input_tokens end,
      case when should_reset then 0 else account_usage.window_image_output_tokens end,
      case when should_reset then 0 else account_usage.window_image_request_count end,
      case when should_reset then 0 else account_usage.window_image_request_failed_count end,
      case when should_reset then $4 else account_usage.window_started_at end,
      excluded.window_reset_at,
      coalesce(excluded.limit_window_seconds, account_usage.limit_window_seconds)
    from (
      values (
        account_usage.window_reset_at is not null
        and account_usage.window_reset_at <> excluded.window_reset_at
        and abs(extract(epoch from account_usage.window_reset_at - excluded.window_reset_at)) >=
          case
            when coalesce(excluded.limit_window_seconds, account_usage.limit_window_seconds, 0) > 0
              then coalesce(excluded.limit_window_seconds, account_usage.limit_window_seconds) / 2
            else 3600
          end
      )
    ) as decision(should_reset)
  )";

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

/// PostgreSQL 用量存储错误。
#[derive(Debug, Error)]
pub enum PgAccountUsageStoreError {
    /// 数据库错误。
    #[error("PostgreSQL usage store database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// PostgreSQL 用量存储结果。
pub type PgAccountUsageStoreResult<T> = Result<T, PgAccountUsageStoreError>;

/// 账号用量端口错误。
#[derive(Debug, Error)]
#[error("account usage store operation failed: {message}")]
pub struct AccountUsageStoreError {
    message: String,
}

impl From<PgAccountUsageStoreError> for AccountUsageStoreError {
    fn from(error: PgAccountUsageStoreError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountUsageDelta {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub empty_responses: u64,
    pub image_input_tokens: u64,
    pub image_output_tokens: u64,
    pub image_requests: u64,
    pub image_request_failures: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountUsageSnapshot {
    pub request_count: u64,
    pub empty_response_count: u64,
    pub image_input_tokens: u64,
    pub image_output_tokens: u64,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
    pub window_request_count: u64,
    pub window_input_tokens: u64,
    pub window_output_tokens: u64,
    pub window_cached_tokens: u64,
    pub window_image_input_tokens: u64,
    pub window_image_output_tokens: u64,
    pub window_image_request_count: u64,
    pub window_image_request_failed_count: u64,
    pub window_started_at: Option<DateTime<Utc>>,
    pub window_reset_at: Option<DateTime<Utc>>,
    pub limit_window_seconds: Option<u64>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountUsageWindow {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub image_input_tokens: u64,
    pub image_output_tokens: u64,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
    pub started_at: Option<DateTime<Utc>>,
    pub reset_at: Option<DateTime<Utc>>,
    pub limit_window_seconds: Option<u64>,
}

#[async_trait]
pub trait AccountUsageStore: Send + Sync + 'static {
    async fn snapshots(
        &self,
        account_ids: &[String],
    ) -> Result<HashMap<String, AccountUsageSnapshot>, AccountUsageStoreError>;

    async fn record_usage_delta(
        &self,
        account_id: &str,
        usage: AccountUsageDelta,
    ) -> Result<(), AccountUsageStoreError>;

    async fn sync_runtime_window(
        &self,
        account_id: &str,
        window: AccountUsageWindow,
    ) -> Result<(), AccountUsageStoreError>;

    async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> Result<(), AccountUsageStoreError>;

    async fn record_request(&self, account_id: &str) -> Result<(), AccountUsageStoreError> {
        self.record_usage_delta(
            account_id,
            AccountUsageDelta {
                requests: 1,
                ..AccountUsageDelta::default()
            },
        )
        .await
    }
}

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

/// PostgreSQL 用量存储。
#[derive(Clone)]
pub struct PgAccountUsageStore {
    pool: PgPool,
}

impl PgAccountUsageStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn load_snapshots(
        &self,
        account_ids: &[String],
    ) -> PgAccountUsageStoreResult<HashMap<String, AccountUsageSnapshot>> {
        if account_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut builder = QueryBuilder::<Postgres>::new(
            r"select
  account_id, request_count, empty_response_count,
  image_input_tokens, image_output_tokens,
  image_request_count, image_request_failed_count,
  window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens,
  window_image_input_tokens, window_image_output_tokens,
  window_image_request_count, window_image_request_failed_count,
  window_started_at, window_reset_at, limit_window_seconds, last_used_at
from account_usage
where account_id in (",
        );
        let mut separated = builder.separated(", ");
        for account_id in account_ids {
            separated.push_bind(account_id);
        }
        separated.push_unseparated(")");

        let rows = builder.build().fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|row| (row.get("account_id"), usage_snapshot_from_row(row)))
            .collect())
    }

    async fn write_usage_delta(
        &self,
        account_id: &str,
        delta: AccountUsageDelta,
    ) -> PgAccountUsageStoreResult<()> {
        let request_count = u64_to_i64_saturating(delta.requests);
        let input_tokens = u64_to_i64_saturating(delta.input_tokens);
        let output_tokens = u64_to_i64_saturating(delta.output_tokens);
        let cached_tokens = u64_to_i64_saturating(delta.cached_tokens);
        let image_input_tokens = u64_to_i64_saturating(delta.image_input_tokens);
        let image_output_tokens = u64_to_i64_saturating(delta.image_output_tokens);
        let image_request_count = u64_to_i64_saturating(delta.image_requests);
        let image_request_failed_count = u64_to_i64_saturating(delta.image_request_failures);
        let last_used_at = Utc::now();
        sqlx::query(RECORD_USAGE_SQL)
            .bind(account_id)
            .bind(request_count)
            .bind(u64_to_i64_saturating(delta.empty_responses))
            .bind(input_tokens)
            .bind(output_tokens)
            .bind(cached_tokens)
            .bind(u64_to_i64_saturating(delta.reasoning_tokens))
            .bind(u64_to_i64_saturating(delta.total_tokens))
            .bind(image_input_tokens)
            .bind(image_output_tokens)
            .bind(image_request_count)
            .bind(image_request_failed_count)
            .bind(request_count)
            .bind(input_tokens)
            .bind(output_tokens)
            .bind(cached_tokens)
            .bind(image_input_tokens)
            .bind(image_output_tokens)
            .bind(image_request_count)
            .bind(image_request_failed_count)
            .bind(last_used_at)
            .bind(last_used_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn replace_runtime_window(
        &self,
        account_id: &str,
        window: AccountUsageWindow,
    ) -> PgAccountUsageStoreResult<()> {
        sqlx::query(SYNC_RUNTIME_WINDOW_SQL)
            .bind(account_id)
            .bind(u64_to_i64_saturating(window.request_count))
            .bind(u64_to_i64_saturating(window.input_tokens))
            .bind(u64_to_i64_saturating(window.output_tokens))
            .bind(u64_to_i64_saturating(window.cached_tokens))
            .bind(u64_to_i64_saturating(window.image_input_tokens))
            .bind(u64_to_i64_saturating(window.image_output_tokens))
            .bind(u64_to_i64_saturating(window.image_request_count))
            .bind(u64_to_i64_saturating(window.image_request_failed_count))
            .bind(window.started_at)
            .bind(window.reset_at)
            .bind(window.limit_window_seconds.map(u64_to_i64_saturating))
            .bind(Utc::now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> PgAccountUsageStoreResult<()> {
        let limit_window_seconds = limit_window_seconds.map(u64_to_i64_saturating);
        sqlx::query(SYNC_RATE_LIMIT_WINDOW_SQL)
            .bind(account_id)
            .bind(reset_at)
            .bind(limit_window_seconds)
            .bind(Utc::now())
            .execute(&self.pool)
            .await?;
        Ok(())
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
}

#[async_trait]
impl AccountUsageStore for PgAccountUsageStore {
    async fn snapshots(
        &self,
        account_ids: &[String],
    ) -> Result<HashMap<String, AccountUsageSnapshot>, AccountUsageStoreError> {
        self.load_snapshots(account_ids).await.map_err(Into::into)
    }

    async fn record_usage_delta(
        &self,
        account_id: &str,
        usage: AccountUsageDelta,
    ) -> Result<(), AccountUsageStoreError> {
        self.write_usage_delta(account_id, usage)
            .await
            .map_err(Into::into)
    }

    async fn sync_runtime_window(
        &self,
        account_id: &str,
        window: AccountUsageWindow,
    ) -> Result<(), AccountUsageStoreError> {
        self.replace_runtime_window(account_id, window)
            .await
            .map_err(Into::into)
    }

    async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> Result<(), AccountUsageStoreError> {
        self.update_rate_limit_window(account_id, reset_at, limit_window_seconds)
            .await
            .map_err(Into::into)
    }
}

fn usage_list_from_row(row: &PgRow) -> PgAccountUsageStoreResult<UsageListRecord> {
    let limit_window_seconds = row
        .try_get::<Option<i64>, _>("limit_window_seconds")?
        .and_then(|value| u64::try_from(value).ok());
    Ok(UsageListRecord {
        account_id: row.try_get("account_id")?,
        email: row.try_get("email")?,
        label: row.try_get("label")?,
        plan_type: row.try_get("plan_type")?,
        request_count: row.try_get("request_count")?,
        empty_response_count: row.try_get("empty_response_count")?,
        input_tokens: row.try_get("input_tokens")?,
        output_tokens: row.try_get("output_tokens")?,
        cached_tokens: row.try_get("cached_tokens")?,
        reasoning_tokens: row.try_get("reasoning_tokens")?,
        total_tokens: row.try_get("total_tokens")?,
        image_input_tokens: row.try_get("image_input_tokens")?,
        image_output_tokens: row.try_get("image_output_tokens")?,
        image_request_count: row.try_get("image_request_count")?,
        image_request_failed_count: row.try_get("image_request_failed_count")?,
        window_request_count: row.try_get("window_request_count")?,
        window_input_tokens: row.try_get("window_input_tokens")?,
        window_output_tokens: row.try_get("window_output_tokens")?,
        window_cached_tokens: row.try_get("window_cached_tokens")?,
        window_started_at: row.try_get("window_started_at")?,
        window_reset_at: row.try_get("window_reset_at")?,
        limit_window_seconds,
        last_used_at: row.try_get("last_used_at")?,
    })
}

fn usage_snapshot_from_row(row: &PgRow) -> AccountUsageSnapshot {
    AccountUsageSnapshot {
        request_count: nonnegative_i64_to_u64(row.get("request_count")),
        empty_response_count: nonnegative_i64_to_u64(row.get("empty_response_count")),
        image_input_tokens: nonnegative_i64_to_u64(row.get("image_input_tokens")),
        image_output_tokens: nonnegative_i64_to_u64(row.get("image_output_tokens")),
        image_request_count: nonnegative_i64_to_u64(row.get("image_request_count")),
        image_request_failed_count: nonnegative_i64_to_u64(row.get("image_request_failed_count")),
        window_request_count: nonnegative_i64_to_u64(row.get("window_request_count")),
        window_input_tokens: nonnegative_i64_to_u64(row.get("window_input_tokens")),
        window_output_tokens: nonnegative_i64_to_u64(row.get("window_output_tokens")),
        window_cached_tokens: nonnegative_i64_to_u64(row.get("window_cached_tokens")),
        window_image_input_tokens: nonnegative_i64_to_u64(row.get("window_image_input_tokens")),
        window_image_output_tokens: nonnegative_i64_to_u64(row.get("window_image_output_tokens")),
        window_image_request_count: nonnegative_i64_to_u64(row.get("window_image_request_count")),
        window_image_request_failed_count: nonnegative_i64_to_u64(
            row.get("window_image_request_failed_count"),
        ),
        window_started_at: row.get("window_started_at"),
        window_reset_at: row.get("window_reset_at"),
        limit_window_seconds: optional_positive_i64_to_u64(row.get("limit_window_seconds")),
        last_used_at: row.get("last_used_at"),
    }
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn optional_positive_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > 0)
}
