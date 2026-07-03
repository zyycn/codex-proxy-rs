//! SQLite 账号仓储 row 映射与查询辅助。

use secrecy::SecretString;
use serde_json::Value;
use sqlx::{sqlite::SqliteRow, QueryBuilder, Row, Sqlite, SqlitePool};

use crate::{
    infra::{
        format::nonnegative_i64_to_u64, json::Page,
        time::parse_optional_rfc3339_utc as parse_optional_rfc3339,
    },
    upstream::accounts::{
        model::{Account, AccountStatus, AccountUsageDelta},
        quota::{quota_snapshot_limit_window_seconds, quota_snapshot_reset_at},
    },
};

use super::{
    queries::{GET_POOL_ACCOUNT_SQL, LIST_POOL_ACCOUNTS_SQL},
    AccountModelUsageRecord, AccountQuotaSnapshot, AccountStoreError, SqliteAccountStore,
    SqliteAccountStoreError, SqliteAccountStoreResult, StoredAccount, StoredAccountMetadata,
    UsageDelta,
};

pub(super) async fn list_pool_accounts(
    store: &SqliteAccountStore,
) -> SqliteAccountStoreResult<Vec<Account>> {
    let rows = sqlx::query(LIST_POOL_ACCOUNTS_SQL)
        .fetch_all(&store.pool)
        .await?;
    let mut accounts = Vec::with_capacity(rows.len());

    for row in rows {
        accounts.push(pool_account_from_row(&row)?);
    }

    Ok(accounts)
}

pub(super) async fn get_pool_account(
    store: &SqliteAccountStore,
    account_id: &str,
) -> SqliteAccountStoreResult<Option<Account>> {
    let row = sqlx::query(GET_POOL_ACCOUNT_SQL)
        .bind(account_id)
        .fetch_optional(&store.pool)
        .await?;
    row.map(|row| pool_account_from_row(&row)).transpose()
}

pub(super) fn pool_account_from_row(row: &SqliteRow) -> SqliteAccountStoreResult<Account> {
    let quota_json = row
        .get::<Option<String>, _>("quota_json")
        .and_then(|quota_json| serde_json::from_str::<Value>(&quota_json).ok());
    let quota_window_reset_at = quota_json.as_ref().and_then(quota_snapshot_reset_at);
    let quota_limit_window_seconds = quota_json
        .as_ref()
        .and_then(quota_snapshot_limit_window_seconds);

    Ok(Account {
        id: row.get("id"),
        email: row.get("email"),
        account_id: row.get("account_id"),
        user_id: row.get("user_id"),
        label: row.get("label"),
        plan_type: row.get("plan_type"),
        access_token: row.get("access_token"),
        refresh_token: row.get("refresh_token"),
        access_token_expires_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("access_token_expires_at")
                .as_deref(),
        )?,
        next_refresh_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("next_refresh_at").as_deref(),
        )?,
        status: status_from_db(&row.get::<String, _>("status"))?,
        quota_limit_reached: row.get::<i64, _>("quota_limit_reached") != 0,
        quota_verify_required: row.get::<i64, _>("quota_verify_required") != 0,
        quota_cooldown_until: parse_optional_rfc3339(
            row.get::<Option<String>, _>("quota_cooldown_until")
                .as_deref(),
        )?,
        cloudflare_cooldown_until: parse_optional_rfc3339(
            row.get::<Option<String>, _>("cloudflare_cooldown_until")
                .as_deref(),
        )?,
        request_count: nonnegative_i64_to_u64(row.get::<i64, _>("usage_request_count")),
        empty_response_count: nonnegative_i64_to_u64(
            row.get::<i64, _>("usage_empty_response_count"),
        ),
        image_input_tokens: nonnegative_i64_to_u64(row.get::<i64, _>("usage_image_input_tokens")),
        image_output_tokens: nonnegative_i64_to_u64(row.get::<i64, _>("usage_image_output_tokens")),
        image_request_count: nonnegative_i64_to_u64(row.get::<i64, _>("usage_image_request_count")),
        image_request_failed_count: nonnegative_i64_to_u64(
            row.get::<i64, _>("usage_image_request_failed_count"),
        ),
        window_request_count: nonnegative_i64_to_u64(
            row.get::<i64, _>("usage_window_request_count"),
        ),
        window_input_tokens: nonnegative_i64_to_u64(row.get::<i64, _>("usage_window_input_tokens")),
        window_output_tokens: nonnegative_i64_to_u64(
            row.get::<i64, _>("usage_window_output_tokens"),
        ),
        window_cached_tokens: nonnegative_i64_to_u64(
            row.get::<i64, _>("usage_window_cached_tokens"),
        ),
        window_image_input_tokens: nonnegative_i64_to_u64(
            row.get::<i64, _>("usage_window_image_input_tokens"),
        ),
        window_image_output_tokens: nonnegative_i64_to_u64(
            row.get::<i64, _>("usage_window_image_output_tokens"),
        ),
        window_image_request_count: nonnegative_i64_to_u64(
            row.get::<i64, _>("usage_window_image_request_count"),
        ),
        window_image_request_failed_count: nonnegative_i64_to_u64(
            row.get::<i64, _>("usage_window_image_request_failed_count"),
        ),
        window_started_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("usage_window_started_at")
                .as_deref(),
        )?,
        window_reset_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("usage_window_reset_at")
                .as_deref(),
        )?
        .or(quota_window_reset_at),
        limit_window_seconds: optional_positive_i64_to_u64(
            row.get::<Option<i64>, _>("usage_limit_window_seconds"),
        )
        .or(quota_limit_window_seconds),
        added_at: row.get("added_at"),
        last_used_at: row.get("usage_last_used_at"),
    })
}

pub(super) fn stored_account_from_row(row: &SqliteRow) -> SqliteAccountStoreResult<StoredAccount> {
    Ok(StoredAccount {
        id: row.get("id"),
        email: row.get("email"),
        account_id: row.get("account_id"),
        user_id: row.get("user_id"),
        label: row.get("label"),
        plan_type: row.get("plan_type"),
        access_token: SecretString::new(row.get::<String, _>("access_token").into()),
        refresh_token: row
            .get::<Option<String>, _>("refresh_token")
            .map(|token| SecretString::new(token.into())),
        access_token_expires_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("access_token_expires_at")
                .as_deref(),
        )?,
        next_refresh_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("next_refresh_at").as_deref(),
        )?,
        status: status_from_db(&row.get::<String, _>("status"))?,
        added_at: row.get("added_at"),
        updated_at: row.get("updated_at"),
    })
}

pub(super) fn metadata_from_row(
    row: &SqliteRow,
) -> SqliteAccountStoreResult<StoredAccountMetadata> {
    Ok(StoredAccountMetadata {
        id: row.get("id"),
        email: row.get("email"),
        account_id: row.get("account_id"),
        user_id: row.get("user_id"),
        label: row.get("label"),
        plan_type: row.get("plan_type"),
        has_refresh_token: row.get::<i64, _>("has_refresh_token") != 0,
        access_token_expires_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("access_token_expires_at")
                .as_deref(),
        )?,
        status: status_from_db(&row.get::<String, _>("status"))?,
        added_at: row.get("added_at"),
        updated_at: row.get("updated_at"),
    })
}

pub(super) fn map_account_store_error(error: &impl ToString) -> AccountStoreError {
    AccountStoreError::OperationFailed {
        message: error.to_string(),
    }
}

pub(super) fn sqlite_usage_delta(usage: AccountUsageDelta) -> UsageDelta {
    let request_count = u64_to_i64_saturating(usage.requests);
    let input_tokens = u64_to_i64_saturating(usage.input_tokens);
    let output_tokens = u64_to_i64_saturating(usage.output_tokens);
    let cached_tokens = u64_to_i64_saturating(usage.cached_tokens);
    let image_input_tokens = u64_to_i64_saturating(usage.image_input_tokens);
    let image_output_tokens = u64_to_i64_saturating(usage.image_output_tokens);
    let image_request_count = u64_to_i64_saturating(usage.image_requests);
    let image_request_failed_count = u64_to_i64_saturating(usage.image_request_failures);
    UsageDelta {
        request_count,
        empty_response_count: u64_to_i64_saturating(usage.empty_responses),
        input_tokens,
        output_tokens,
        cached_tokens,
        reasoning_tokens: u64_to_i64_saturating(usage.reasoning_tokens),
        total_tokens: u64_to_i64_saturating(usage.total_tokens),
        image_input_tokens,
        image_output_tokens,
        image_request_count,
        image_request_failed_count,
        window_request_count: request_count,
        window_input_tokens: input_tokens,
        window_output_tokens: output_tokens,
        window_cached_tokens: cached_tokens,
        window_image_input_tokens: image_input_tokens,
        window_image_output_tokens: image_output_tokens,
        window_image_request_count: image_request_count,
        window_image_request_failed_count: image_request_failed_count,
    }
}

pub(super) fn quota_plan_type(quota_json: &str) -> Option<String> {
    serde_json::from_str::<Value>(quota_json)
        .ok()?
        .get("plan_type")?
        .as_str()
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && !matches!(value.to_ascii_lowercase().as_str(), "unknown" | "null")
        })
        .map(ToString::to_string)
}

pub(super) fn model_usage_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> SqliteAccountStoreResult<AccountModelUsageRecord> {
    Ok(AccountModelUsageRecord {
        account_id: row.get("account_id"),
        model: row.get("model"),
        request_count: row.get("request_count"),
        error_count: row.get("error_count"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        last_used_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("last_used_at").as_deref(),
        )?,
    })
}

pub(super) fn quota_snapshot_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> SqliteAccountStoreResult<AccountQuotaSnapshot> {
    Ok(AccountQuotaSnapshot {
        account_id: row.get("id"),
        email: row.get("email"),
        quota_json: row.get("quota_json"),
        quota_fetched_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("quota_fetched_at").as_deref(),
        )?,
    })
}

pub(super) fn status_to_db(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Active => "active",
        AccountStatus::Expired => "expired",
        AccountStatus::QuotaExhausted => "quota_exhausted",
        AccountStatus::Refreshing => "refreshing",
        AccountStatus::Disabled => "disabled",
        AccountStatus::Banned => "banned",
    }
}

pub(super) fn optional_update_value(value: &Option<Option<String>>) -> Option<&str> {
    value.as_ref().and_then(|value| value.as_deref())
}

pub(super) fn status_from_db(value: &str) -> SqliteAccountStoreResult<AccountStatus> {
    match value {
        "active" => Ok(AccountStatus::Active),
        "expired" => Ok(AccountStatus::Expired),
        "quota_exhausted" => Ok(AccountStatus::QuotaExhausted),
        "refreshing" => Ok(AccountStatus::Refreshing),
        "disabled" => Ok(AccountStatus::Disabled),
        "banned" => Ok(AccountStatus::Banned),
        other => Err(SqliteAccountStoreError::InvalidStatus(other.to_string())),
    }
}

pub(super) fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

pub(super) fn optional_positive_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > 0)
}

pub(super) async fn count_account_metadata(
    pool: &SqlitePool,
    search: Option<&str>,
) -> SqliteAccountStoreResult<u64> {
    let mut builder = QueryBuilder::<Sqlite>::new("select count(*) from accounts");
    push_account_metadata_search(&mut builder, search);
    let (total,): (i64,) = builder.build_query_as().fetch_one(pool).await?;
    Ok(nonnegative_i64_to_u64(total))
}

pub(super) fn push_account_metadata_search(
    builder: &mut QueryBuilder<Sqlite>,
    search: Option<&str>,
) {
    let Some(search) = search else {
        return;
    };

    let pattern = format!("%{search}%");
    builder.push(" where id like ");
    builder.push_bind(pattern.clone());
    builder.push(" or email like ");
    builder.push_bind(pattern.clone());
    builder.push(" or label like ");
    builder.push_bind(pattern.clone());
    builder.push(" or chatgpt_account_id like ");
    builder.push_bind(pattern.clone());
    builder.push(" or chatgpt_user_id like ");
    builder.push_bind(pattern);
}

pub(super) fn to_page<T>(
    rows: &[SqliteRow],
    limit: u32,
    mapper: impl Fn(&SqliteRow) -> SqliteAccountStoreResult<T>,
    cursor_fields: (&str, &str),
) -> Page<T> {
    let has_more = rows.len() > limit as usize;
    let mut items: Vec<T> = Vec::with_capacity(limit as usize);
    let mut last_row: Option<&SqliteRow> = None;
    for (i, row) in rows.iter().enumerate() {
        if i >= limit as usize {
            break;
        }
        if let Ok(item) = mapper(row) {
            items.push(item);
            last_row = Some(row);
        }
    }
    let next_cursor = if has_more {
        last_row.map(|row| {
            use sqlx::Row;
            let ts: String = row.get(cursor_fields.0);
            let id: String = row.get(cursor_fields.1);
            crate::infra::json::encode_cursor(&ts, &id)
        })
    } else {
        None
    };
    Page { items, next_cursor }
}
