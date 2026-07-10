//! PostgreSQL 账号仓储 row 映射与查询辅助。

use secrecy::SecretString;
use serde_json::Value;
use sqlx::{postgres::PgRow, PgPool, Postgres, QueryBuilder, Row};

use crate::{
    accounts::{
        account::{Account, AccountStatus},
        quota::{quota_snapshot_limit_window_seconds, quota_snapshot_reset_at},
    },
    infra::{format::nonnegative_i64_to_u64, json::Page},
};

use super::{
    queries::{GET_POOL_ACCOUNT_SQL, LIST_POOL_ACCOUNTS_SQL},
    AccountQuotaSnapshot, AccountStoreError, PgAccountStore, PgAccountStoreError,
    PgAccountStoreResult, StoredAccount, StoredAccountMetadata,
};

pub(super) async fn list_pool_accounts(
    store: &PgAccountStore,
) -> PgAccountStoreResult<Vec<Account>> {
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
    store: &PgAccountStore,
    account_id: &str,
) -> PgAccountStoreResult<Option<Account>> {
    let row = sqlx::query(GET_POOL_ACCOUNT_SQL)
        .bind(account_id)
        .fetch_optional(&store.pool)
        .await?;
    row.map(|row| pool_account_from_row(&row)).transpose()
}

pub(super) fn pool_account_from_row(row: &PgRow) -> PgAccountStoreResult<Account> {
    let quota_json = row
        .get::<Option<sqlx::types::Json<Value>>, _>("quota_json")
        .map(|quota_json| quota_json.0);
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
        access_token_expires_at: row.get("access_token_expires_at"),
        next_refresh_at: row.get("next_refresh_at"),
        status: status_from_db(&row.get::<String, _>("status"))?,
        quota_limit_reached: row.get("quota_limit_reached"),
        quota_verify_required: row.get("quota_verify_required"),
        quota_cooldown_until: row.get("quota_cooldown_until"),
        cloudflare_cooldown_until: row.get("cloudflare_cooldown_until"),
        request_count: 0,
        empty_response_count: 0,
        image_input_tokens: 0,
        image_output_tokens: 0,
        image_request_count: 0,
        image_request_failed_count: 0,
        window_request_count: 0,
        window_input_tokens: 0,
        window_output_tokens: 0,
        window_cached_tokens: 0,
        window_image_input_tokens: 0,
        window_image_output_tokens: 0,
        window_image_request_count: 0,
        window_image_request_failed_count: 0,
        window_started_at: None,
        window_reset_at: quota_window_reset_at,
        limit_window_seconds: quota_limit_window_seconds,
        added_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("added_at")
            .to_rfc3339(),
        last_used_at: None,
    })
}

pub(super) fn stored_account_from_row(row: &PgRow) -> PgAccountStoreResult<StoredAccount> {
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
        access_token_expires_at: row.get("access_token_expires_at"),
        next_refresh_at: row.get("next_refresh_at"),
        status: status_from_db(&row.get::<String, _>("status"))?,
        added_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("added_at")
            .to_rfc3339(),
        updated_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .to_rfc3339(),
    })
}

pub(super) fn metadata_from_row(row: &PgRow) -> PgAccountStoreResult<StoredAccountMetadata> {
    Ok(StoredAccountMetadata {
        id: row.get("id"),
        email: row.get("email"),
        account_id: row.get("account_id"),
        user_id: row.get("user_id"),
        label: row.get("label"),
        plan_type: row.get("plan_type"),
        has_refresh_token: row.get("has_refresh_token"),
        access_token_expires_at: row.get("access_token_expires_at"),
        status: status_from_db(&row.get::<String, _>("status"))?,
        added_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("added_at")
            .to_rfc3339(),
        updated_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .to_rfc3339(),
    })
}

pub(super) fn map_account_store_error(error: &impl ToString) -> AccountStoreError {
    AccountStoreError::OperationFailed {
        message: error.to_string(),
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

pub(super) fn quota_snapshot_from_row(
    row: &sqlx::postgres::PgRow,
) -> PgAccountStoreResult<AccountQuotaSnapshot> {
    Ok(AccountQuotaSnapshot {
        account_id: row.get("id"),
        email: row.get("email"),
        quota_json: row
            .get::<sqlx::types::Json<Value>, _>("quota_json")
            .0
            .to_string(),
        quota_fetched_at: row.get("quota_fetched_at"),
    })
}

pub(super) fn status_to_db(status: AccountStatus) -> &'static str {
    status.as_str()
}

pub(super) fn optional_update_value(value: &Option<Option<String>>) -> Option<&str> {
    value.as_ref().and_then(|value| value.as_deref())
}

pub(super) fn status_from_db(value: &str) -> PgAccountStoreResult<AccountStatus> {
    match value {
        "active" => Ok(AccountStatus::Active),
        "expired" => Ok(AccountStatus::Expired),
        "quota_exhausted" => Ok(AccountStatus::QuotaExhausted),
        "disabled" => Ok(AccountStatus::Disabled),
        "banned" => Ok(AccountStatus::Banned),
        other => Err(PgAccountStoreError::InvalidStatus(other.to_string())),
    }
}

pub(super) async fn count_account_metadata(
    pool: &PgPool,
    search: Option<&str>,
) -> PgAccountStoreResult<u64> {
    let mut builder = QueryBuilder::<Postgres>::new("select count(*) from accounts");
    push_account_metadata_search(&mut builder, search);
    let (total,): (i64,) = builder.build_query_as().fetch_one(pool).await?;
    Ok(nonnegative_i64_to_u64(total))
}

pub(super) fn push_account_metadata_search(
    builder: &mut QueryBuilder<Postgres>,
    search: Option<&str>,
) {
    let Some(search) = search else {
        return;
    };

    let pattern = format!("%{search}%");
    builder.push(" where id ilike ");
    builder.push_bind(pattern.clone());
    builder.push(" or email ilike ");
    builder.push_bind(pattern.clone());
    builder.push(" or label ilike ");
    builder.push_bind(pattern.clone());
    builder.push(" or chatgpt_account_id ilike ");
    builder.push_bind(pattern.clone());
    builder.push(" or chatgpt_user_id ilike ");
    builder.push_bind(pattern);
}

pub(super) fn to_page<T>(
    rows: &[PgRow],
    limit: u32,
    mapper: impl Fn(&PgRow) -> PgAccountStoreResult<T>,
    cursor_fields: (&str, &str),
) -> Page<T> {
    let has_more = rows.len() > limit as usize;
    let mut items: Vec<T> = Vec::with_capacity(limit as usize);
    let mut last_row: Option<&PgRow> = None;
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
            let ts = row
                .get::<chrono::DateTime<chrono::Utc>, _>(cursor_fields.0)
                .to_rfc3339();
            let id: String = row.get(cursor_fields.1);
            crate::infra::json::encode_cursor(&ts, &id)
        })
    } else {
        None
    };
    Page { items, next_cursor }
}
