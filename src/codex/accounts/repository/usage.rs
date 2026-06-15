use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};

use crate::utils::pagination::{decode_cursor, encode_cursor, Page};

use super::{
    optional_positive_i64_to_u64, parse_optional_rfc3339, AccountRepository,
    AccountRepositoryError, AccountRepositoryResult, AccountUsageListRecord, AccountUsageRecord,
    AccountUsageSummary, UsageDelta,
};

const RECORD_USAGE_SQL: &str = r"
insert into account_usage (
  account_id,
  request_count,
  empty_response_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  image_input_tokens,
  image_output_tokens,
  image_request_count,
  image_request_failed_count,
  window_request_count,
  window_input_tokens,
  window_output_tokens,
  window_cached_tokens,
  window_image_input_tokens,
  window_image_output_tokens,
  window_image_request_count,
  window_image_request_failed_count,
  last_used_at
) values (?, 1, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?)
on conflict(account_id) do update set
  request_count = request_count + 1,
  empty_response_count = empty_response_count + excluded.empty_response_count,
  input_tokens = input_tokens + excluded.input_tokens,
  output_tokens = output_tokens + excluded.output_tokens,
  cached_tokens = cached_tokens + excluded.cached_tokens,
  image_input_tokens = image_input_tokens + excluded.image_input_tokens,
  image_output_tokens = image_output_tokens + excluded.image_output_tokens,
  image_request_count = image_request_count + excluded.image_request_count,
  image_request_failed_count = image_request_failed_count + excluded.image_request_failed_count,
  window_request_count = window_request_count + 1,
  window_input_tokens = window_input_tokens + excluded.window_input_tokens,
  window_output_tokens = window_output_tokens + excluded.window_output_tokens,
  window_cached_tokens = window_cached_tokens + excluded.window_cached_tokens,
  window_image_input_tokens = window_image_input_tokens + excluded.window_image_input_tokens,
  window_image_output_tokens = window_image_output_tokens + excluded.window_image_output_tokens,
  window_image_request_count = window_image_request_count + excluded.window_image_request_count,
  window_image_request_failed_count = window_image_request_failed_count + excluded.window_image_request_failed_count,
  window_started_at = case
    when account_usage.window_started_at is null
      and (account_usage.window_reset_at is not null or account_usage.limit_window_seconds is not null)
    then excluded.last_used_at
    else account_usage.window_started_at
  end,
  last_used_at = excluded.last_used_at";

const GET_USAGE_SQL: &str = r"
select
  account_id,
  request_count,
  empty_response_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  image_input_tokens,
  image_output_tokens,
  image_request_count,
  image_request_failed_count,
  window_request_count,
  window_input_tokens,
  window_output_tokens,
  window_cached_tokens,
  window_image_input_tokens,
  window_image_output_tokens,
  window_image_request_count,
  window_image_request_failed_count,
  window_started_at,
  window_reset_at,
  limit_window_seconds,
  last_used_at
from account_usage
where account_id = ?";

const SELECT_RATE_LIMIT_WINDOW_SQL: &str = r"
select
  window_reset_at,
  limit_window_seconds
from account_usage
where account_id = ?";

const SYNC_RATE_LIMIT_WINDOW_RESET_SQL: &str = r"
insert into account_usage (
  account_id,
  window_request_count,
  window_input_tokens,
  window_output_tokens,
  window_cached_tokens,
  window_image_input_tokens,
  window_image_output_tokens,
  window_image_request_count,
  window_image_request_failed_count,
  window_started_at,
  window_reset_at,
  limit_window_seconds
) values (?, 0, 0, 0, 0, 0, 0, 0, 0, ?, ?, ?)
on conflict(account_id) do update set
  window_request_count = 0,
  window_input_tokens = 0,
  window_output_tokens = 0,
  window_cached_tokens = 0,
  window_image_input_tokens = 0,
  window_image_output_tokens = 0,
  window_image_request_count = 0,
  window_image_request_failed_count = 0,
  window_started_at = excluded.window_started_at,
  window_reset_at = excluded.window_reset_at,
  limit_window_seconds = coalesce(excluded.limit_window_seconds, account_usage.limit_window_seconds)";

const SYNC_RATE_LIMIT_WINDOW_SQL: &str = r"
insert into account_usage (
  account_id,
  window_reset_at,
  limit_window_seconds
) values (?, ?, ?)
on conflict(account_id) do update set
  window_reset_at = excluded.window_reset_at,
  limit_window_seconds = coalesce(excluded.limit_window_seconds, account_usage.limit_window_seconds)";

const RESET_ACCOUNT_USAGE_SQL: &str = r"
insert into account_usage (
  account_id,
  request_count,
  empty_response_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  image_input_tokens,
  image_output_tokens,
  image_request_count,
  image_request_failed_count,
  window_request_count,
  window_input_tokens,
  window_output_tokens,
  window_cached_tokens,
  window_image_input_tokens,
  window_image_output_tokens,
  window_image_request_count,
  window_image_request_failed_count,
  window_started_at,
  window_reset_at,
  last_used_at
) values (?, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, null, null, null)
on conflict(account_id) do update set
  request_count = 0,
  empty_response_count = 0,
  input_tokens = 0,
  output_tokens = 0,
  cached_tokens = 0,
  image_input_tokens = 0,
  image_output_tokens = 0,
  image_request_count = 0,
  image_request_failed_count = 0,
  window_request_count = 0,
  window_input_tokens = 0,
  window_output_tokens = 0,
  window_cached_tokens = 0,
  window_image_input_tokens = 0,
  window_image_output_tokens = 0,
  window_image_request_count = 0,
  window_image_request_failed_count = 0,
  window_started_at = null,
  window_reset_at = null,
  last_used_at = null";

const LIST_ACCOUNT_USAGE_AFTER_CURSOR_SQL: &str = r"
select
  account_usage.account_id,
  accounts.email,
  accounts.label,
  accounts.plan_type,
  account_usage.request_count,
  account_usage.empty_response_count,
  account_usage.input_tokens,
  account_usage.output_tokens,
  account_usage.cached_tokens,
  account_usage.image_input_tokens,
  account_usage.image_output_tokens,
  account_usage.image_request_count,
  account_usage.image_request_failed_count,
  account_usage.last_used_at
from account_usage
left join accounts on accounts.id = account_usage.account_id
where coalesce(account_usage.last_used_at, '') < ?
  or (
    coalesce(account_usage.last_used_at, '') = ?
    and account_usage.account_id < ?
  )
order by coalesce(account_usage.last_used_at, '') desc, account_usage.account_id desc
limit ?";

const LIST_ACCOUNT_USAGE_SQL: &str = r"
select
  account_usage.account_id,
  accounts.email,
  accounts.label,
  accounts.plan_type,
  account_usage.request_count,
  account_usage.empty_response_count,
  account_usage.input_tokens,
  account_usage.output_tokens,
  account_usage.cached_tokens,
  account_usage.image_input_tokens,
  account_usage.image_output_tokens,
  account_usage.image_request_count,
  account_usage.image_request_failed_count,
  account_usage.last_used_at
from account_usage
left join accounts on accounts.id = account_usage.account_id
order by coalesce(account_usage.last_used_at, '') desc, account_usage.account_id desc
limit ?";

const ACCOUNT_USAGE_SUMMARY_SQL: &str = r"
select
  count(*) as account_count,
  coalesce(sum(request_count), 0) as request_count,
  coalesce(sum(empty_response_count), 0) as empty_response_count,
  coalesce(sum(input_tokens), 0) as input_tokens,
  coalesce(sum(output_tokens), 0) as output_tokens,
  coalesce(sum(cached_tokens), 0) as cached_tokens,
  coalesce(sum(image_input_tokens), 0) as image_input_tokens,
  coalesce(sum(image_output_tokens), 0) as image_output_tokens,
  coalesce(sum(image_request_count), 0) as image_request_count,
  coalesce(sum(image_request_failed_count), 0) as image_request_failed_count
from account_usage";

impl AccountRepository {
    pub async fn record_usage(
        &self,
        account_id: &str,
        usage: UsageDelta,
    ) -> AccountRepositoryResult<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(RECORD_USAGE_SQL)
            .bind(account_id)
            .bind(usage.empty_response_count)
            .bind(usage.input_tokens)
            .bind(usage.output_tokens)
            .bind(usage.cached_tokens)
            .bind(usage.image_input_tokens)
            .bind(usage.image_output_tokens)
            .bind(usage.image_request_count)
            .bind(usage.image_request_failed_count)
            .bind(usage.input_tokens)
            .bind(usage.output_tokens)
            .bind(usage.cached_tokens)
            .bind(usage.image_input_tokens)
            .bind(usage.image_output_tokens)
            .bind(usage.image_request_count)
            .bind(usage.image_request_failed_count)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_usage(
        &self,
        account_id: &str,
    ) -> AccountRepositoryResult<Option<AccountUsageRecord>> {
        let row = sqlx::query(GET_USAGE_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| usage_from_row(&row)).transpose()
    }

    pub async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> AccountRepositoryResult<()> {
        let row = sqlx::query(SELECT_RATE_LIMIT_WINDOW_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        let existing_reset_at = row
            .as_ref()
            .map(|row| parse_optional_rfc3339(row.get::<Option<String>, _>("window_reset_at")))
            .transpose()?
            .flatten();
        let existing_limit_window_seconds = row
            .as_ref()
            .and_then(|row| optional_positive_i64_to_u64(row.get("limit_window_seconds")));
        let limit_window_seconds_db = limit_window_seconds.map(u64_to_i64_saturating);
        let reset_at_db = reset_at.to_rfc3339();

        if should_reset_usage_window(
            existing_reset_at,
            existing_limit_window_seconds,
            reset_at,
            limit_window_seconds,
        ) {
            sqlx::query(SYNC_RATE_LIMIT_WINDOW_RESET_SQL)
                .bind(account_id)
                .bind(Utc::now().to_rfc3339())
                .bind(reset_at_db)
                .bind(limit_window_seconds_db)
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query(SYNC_RATE_LIMIT_WINDOW_SQL)
                .bind(account_id)
                .bind(reset_at_db)
                .bind(limit_window_seconds_db)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct AccountUsageRepository {
    pool: SqlitePool,
}

impl AccountUsageRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> AccountRepositoryResult<Page<AccountUsageListRecord>> {
        let fetch_limit = i64::from(limit) + 1;
        let rows = if let Some(cursor) = cursor {
            let (last_used_at, account_id) =
                decode_cursor(&cursor).ok_or(AccountRepositoryError::InvalidCursor)?;
            sqlx::query(LIST_ACCOUNT_USAGE_AFTER_CURSOR_SQL)
                .bind(&last_used_at)
                .bind(last_used_at)
                .bind(account_id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query(LIST_ACCOUNT_USAGE_SQL)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        };

        let has_next = rows.len() > limit as usize;
        let take_count = rows.len().min(limit as usize);
        let mut items = Vec::with_capacity(take_count);
        for row in rows.into_iter().take(take_count) {
            items.push(usage_list_from_row(&row)?);
        }
        let next_cursor = if has_next {
            items.last().map(|usage| {
                encode_cursor(
                    &usage
                        .last_used_at
                        .map(|value| value.to_rfc3339())
                        .unwrap_or_default(),
                    &usage.account_id,
                )
            })
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    pub async fn summary(&self) -> AccountRepositoryResult<AccountUsageSummary> {
        let row = sqlx::query(ACCOUNT_USAGE_SUMMARY_SQL)
            .fetch_one(&self.pool)
            .await?;
        Ok(AccountUsageSummary {
            account_count: row.get("account_count"),
            request_count: row.get("request_count"),
            empty_response_count: row.get("empty_response_count"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cached_tokens: row.get("cached_tokens"),
            image_input_tokens: row.get("image_input_tokens"),
            image_output_tokens: row.get("image_output_tokens"),
            image_request_count: row.get("image_request_count"),
            image_request_failed_count: row.get("image_request_failed_count"),
        })
    }

    pub async fn reset_account(&self, account_id: &str) -> AccountRepositoryResult<()> {
        sqlx::query(RESET_ACCOUNT_USAGE_SQL)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn usage_from_row(row: &sqlx::sqlite::SqliteRow) -> AccountRepositoryResult<AccountUsageRecord> {
    Ok(AccountUsageRecord {
        account_id: row.get("account_id"),
        request_count: row.get("request_count"),
        empty_response_count: row.get("empty_response_count"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        image_input_tokens: row.get("image_input_tokens"),
        image_output_tokens: row.get("image_output_tokens"),
        image_request_count: row.get("image_request_count"),
        image_request_failed_count: row.get("image_request_failed_count"),
        window_request_count: row.get("window_request_count"),
        window_input_tokens: row.get("window_input_tokens"),
        window_output_tokens: row.get("window_output_tokens"),
        window_cached_tokens: row.get("window_cached_tokens"),
        window_image_input_tokens: row.get("window_image_input_tokens"),
        window_image_output_tokens: row.get("window_image_output_tokens"),
        window_image_request_count: row.get("window_image_request_count"),
        window_image_request_failed_count: row.get("window_image_request_failed_count"),
        window_started_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("window_started_at"),
        )?,
        window_reset_at: parse_optional_rfc3339(row.get::<Option<String>, _>("window_reset_at"))?,
        limit_window_seconds: optional_positive_i64_to_u64(row.get("limit_window_seconds")),
        last_used_at: parse_optional_rfc3339(row.get::<Option<String>, _>("last_used_at"))?,
    })
}

fn usage_list_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> AccountRepositoryResult<AccountUsageListRecord> {
    Ok(AccountUsageListRecord {
        account_id: row.get("account_id"),
        email: row.get("email"),
        label: row.get("label"),
        plan_type: row.get("plan_type"),
        request_count: row.get("request_count"),
        empty_response_count: row.get("empty_response_count"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        image_input_tokens: row.get("image_input_tokens"),
        image_output_tokens: row.get("image_output_tokens"),
        image_request_count: row.get("image_request_count"),
        image_request_failed_count: row.get("image_request_failed_count"),
        last_used_at: parse_optional_rfc3339(row.get::<Option<String>, _>("last_used_at"))?,
    })
}

fn should_reset_usage_window(
    existing_reset_at: Option<DateTime<Utc>>,
    existing_limit_window_seconds: Option<u64>,
    new_reset_at: DateTime<Utc>,
    new_limit_window_seconds: Option<u64>,
) -> bool {
    let Some(existing_reset_at) = existing_reset_at else {
        return false;
    };
    if existing_reset_at == new_reset_at {
        return false;
    }
    let drift = existing_reset_at
        .signed_duration_since(new_reset_at)
        .num_seconds()
        .unsigned_abs();
    let window_seconds = new_limit_window_seconds
        .or(existing_limit_window_seconds)
        .unwrap_or(0);
    let threshold = if window_seconds > 0 {
        window_seconds / 2
    } else {
        3_600
    };
    drift >= threshold
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}
