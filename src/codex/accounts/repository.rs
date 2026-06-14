use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use sqlx::{Row, SqlitePool};
use thiserror::Error;

use crate::{
    codex::accounts::model::{Account, AccountStatus},
    platform::crypto::{CryptoError, SecretBox},
    utils::pagination::{decode_cursor, encode_cursor, Page},
};

mod lease;
mod quota;
mod token;
mod usage;

pub use usage::AccountUsageRepository;

#[derive(Debug, Error)]
pub enum AccountRepositoryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("account secret encryption error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("invalid account status: {0}")]
    InvalidStatus(String),
    #[error("invalid account timestamp: {0}")]
    InvalidTimestamp(#[from] chrono::ParseError),
    #[error("invalid pagination cursor")]
    InvalidCursor,
}

pub type AccountRepositoryResult<T> = Result<T, AccountRepositoryError>;

#[derive(Debug)]
pub struct NewAccount {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub status: AccountStatus,
}

#[derive(Debug)]
pub struct TokenUpdate {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub struct AccountClaimsUpdate {
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub plan_type: Option<String>,
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub status: AccountStatus,
}

#[derive(Debug, Clone)]
pub struct StoredAccount {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub status: AccountStatus,
    pub added_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct StoredAccountMetadata {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub status: AccountStatus,
    pub added_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
pub struct UsageDelta {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub empty_response_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageRecord {
    pub account_id: String,
    pub request_count: i64,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub window_request_count: i64,
    pub window_input_tokens: i64,
    pub window_output_tokens: i64,
    pub window_cached_tokens: i64,
    pub window_started_at: Option<DateTime<Utc>>,
    pub window_reset_at: Option<DateTime<Utc>>,
    pub limit_window_seconds: Option<u64>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageListRecord {
    pub account_id: String,
    pub email: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub request_count: i64,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountQuotaSnapshot {
    pub account_id: String,
    pub email: Option<String>,
    pub quota_json: String,
    pub quota_fetched_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountUsageSummary {
    pub account_count: i64,
    pub request_count: i64,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
}

#[derive(Clone)]
pub struct AccountRepository {
    pool: SqlitePool,
    secret_box: SecretBox,
}

const INSERT_ACCOUNT_SQL: &str = r"
insert into accounts (
  id,
  email,
  account_id,
  user_id,
  label,
  plan_type,
  access_token_cipher,
  refresh_token_cipher,
  access_token_expires_at,
  status,
  added_at,
  updated_at
) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

const SELECT_STORED_ACCOUNT_BY_ID_SQL: &str = r"
select
  id,
  email,
  account_id,
  user_id,
  label,
  plan_type,
  access_token_cipher,
  refresh_token_cipher,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
where id = ?";

const SELECT_STORED_ACCOUNT_BY_CHATGPT_IDENTITY_SQL: &str = r"
select
  id,
  email,
  account_id,
  user_id,
  label,
  plan_type,
  access_token_cipher,
  refresh_token_cipher,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
where account_id = ?
  and ((user_id is null and ? is null) or user_id = ?)
order by added_at asc
limit 1";

const LIST_STORED_ACCOUNTS_AFTER_CURSOR_SQL: &str = r"
select
  id,
  email,
  account_id,
  user_id,
  label,
  plan_type,
  access_token_cipher,
  refresh_token_cipher,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
where added_at < ?
  or (added_at = ? and id < ?)
order by added_at desc, id desc
limit ?";

const LIST_STORED_ACCOUNTS_SQL: &str = r"
select
  id,
  email,
  account_id,
  user_id,
  label,
  plan_type,
  access_token_cipher,
  refresh_token_cipher,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
order by added_at desc, id desc
limit ?";

const LIST_ALL_STORED_ACCOUNTS_SQL: &str = r"
select
  id,
  email,
  account_id,
  user_id,
  label,
  plan_type,
  access_token_cipher,
  refresh_token_cipher,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
order by added_at desc, id desc";

const LIST_METADATA_AFTER_CURSOR_SQL: &str = r"
select
  id,
  email,
  account_id,
  user_id,
  label,
  plan_type,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
where added_at < ?
  or (added_at = ? and id < ?)
order by added_at desc, id desc
limit ?";

const LIST_METADATA_SQL: &str = r"
select
  id,
  email,
  account_id,
  user_id,
  label,
  plan_type,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
order by added_at desc, id desc
limit ?";

const LIST_ALL_METADATA_SQL: &str = r"
select
  id,
  email,
  account_id,
  user_id,
  label,
  plan_type,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
order by added_at desc, id desc";

const LIST_POOL_ACCOUNTS_SQL: &str = r"
select
  accounts.id,
  email,
  accounts.account_id,
  user_id,
  label,
  plan_type,
  access_token_cipher,
  refresh_token_cipher,
  access_token_expires_at,
  status,
  added_at,
  updated_at,
  quota_limit_reached,
  quota_cooldown_until,
  cloudflare_cooldown_until,
  coalesce(account_usage.request_count, 0) as usage_request_count,
  coalesce(account_usage.empty_response_count, 0) as usage_empty_response_count,
  coalesce(account_usage.window_request_count, 0) as usage_window_request_count,
  coalesce(account_usage.window_input_tokens, 0) as usage_window_input_tokens,
  coalesce(account_usage.window_output_tokens, 0) as usage_window_output_tokens,
  coalesce(account_usage.window_cached_tokens, 0) as usage_window_cached_tokens,
  account_usage.window_started_at as usage_window_started_at,
  account_usage.window_reset_at as usage_window_reset_at,
  account_usage.limit_window_seconds as usage_limit_window_seconds,
  account_usage.last_used_at as usage_last_used_at
from accounts
left join account_usage on account_usage.account_id = accounts.id
order by added_at desc, accounts.id desc";

impl AccountRepository {
    pub fn new(pool: SqlitePool, secret_box: SecretBox) -> Self {
        Self { pool, secret_box }
    }

    pub async fn insert(&self, account: NewAccount) -> AccountRepositoryResult<()> {
        let now = Utc::now().to_rfc3339();
        let access_token_cipher = self.secret_box.encrypt(&account.access_token)?;
        let refresh_token_cipher = account
            .refresh_token
            .as_ref()
            .map(|token| self.secret_box.encrypt(token))
            .transpose()?;
        sqlx::query(INSERT_ACCOUNT_SQL)
            .bind(account.id)
            .bind(account.email)
            .bind(account.account_id)
            .bind(account.user_id)
            .bind(account.label)
            .bind(account.plan_type)
            .bind(access_token_cipher)
            .bind(refresh_token_cipher)
            .bind(
                account
                    .access_token_expires_at
                    .map(|value| value.to_rfc3339()),
            )
            .bind(status_to_db(account.status))
            .bind(&now)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get(&self, id: &str) -> AccountRepositoryResult<Option<StoredAccount>> {
        let row = sqlx::query(SELECT_STORED_ACCOUNT_BY_ID_SQL)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| self.account_from_row(&row)).transpose()
    }

    pub async fn exists(&self, id: &str) -> AccountRepositoryResult<bool> {
        let row = sqlx::query("select 1 from accounts where id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn find_by_chatgpt_identity(
        &self,
        account_id: &str,
        user_id: Option<&str>,
    ) -> AccountRepositoryResult<Option<StoredAccount>> {
        let row = sqlx::query(SELECT_STORED_ACCOUNT_BY_CHATGPT_IDENTITY_SQL)
            .bind(account_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| self.account_from_row(&row)).transpose()
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> AccountRepositoryResult<Page<StoredAccount>> {
        let fetch_limit = i64::from(limit) + 1;
        let rows = if let Some(cursor) = cursor {
            let (created_at, id) =
                decode_cursor(&cursor).ok_or(AccountRepositoryError::InvalidCursor)?;
            sqlx::query(LIST_STORED_ACCOUNTS_AFTER_CURSOR_SQL)
                .bind(&created_at)
                .bind(created_at)
                .bind(id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query(LIST_STORED_ACCOUNTS_SQL)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        };

        let has_next = rows.len() > limit as usize;
        let take_count = rows.len().min(limit as usize);
        let mut items = Vec::with_capacity(take_count);
        for row in rows.into_iter().take(take_count) {
            items.push(self.account_from_row(&row)?);
        }
        let next_cursor = if has_next {
            items
                .last()
                .map(|account| encode_cursor(&account.added_at.to_rfc3339(), &account.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    pub async fn list_all(&self) -> AccountRepositoryResult<Vec<StoredAccount>> {
        let rows = sqlx::query(LIST_ALL_STORED_ACCOUNTS_SQL)
            .fetch_all(&self.pool)
            .await?;
        let mut accounts = Vec::with_capacity(rows.len());
        for row in rows {
            accounts.push(self.account_from_row(&row)?);
        }
        Ok(accounts)
    }

    pub async fn list_metadata(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> AccountRepositoryResult<Page<StoredAccountMetadata>> {
        let fetch_limit = i64::from(limit) + 1;
        let rows = if let Some(cursor) = cursor {
            let (created_at, id) =
                decode_cursor(&cursor).ok_or(AccountRepositoryError::InvalidCursor)?;
            sqlx::query(LIST_METADATA_AFTER_CURSOR_SQL)
                .bind(&created_at)
                .bind(created_at)
                .bind(id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query(LIST_METADATA_SQL)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        };

        let has_next = rows.len() > limit as usize;
        let take_count = rows.len().min(limit as usize);
        let mut items = Vec::with_capacity(take_count);
        for row in rows.into_iter().take(take_count) {
            items.push(metadata_from_row(&row)?);
        }
        let next_cursor = if has_next {
            items
                .last()
                .map(|account| encode_cursor(&account.added_at.to_rfc3339(), &account.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    pub async fn list_all_metadata(&self) -> AccountRepositoryResult<Vec<StoredAccountMetadata>> {
        let rows = sqlx::query(LIST_ALL_METADATA_SQL)
            .fetch_all(&self.pool)
            .await?;
        let mut accounts = Vec::with_capacity(rows.len());
        for row in rows {
            accounts.push(metadata_from_row(&row)?);
        }
        Ok(accounts)
    }

    pub async fn list_pool_accounts(&self) -> AccountRepositoryResult<Vec<Account>> {
        let rows = sqlx::query(LIST_POOL_ACCOUNTS_SQL)
            .fetch_all(&self.pool)
            .await?;
        let mut accounts = Vec::with_capacity(rows.len());
        for row in rows {
            let stored = self.account_from_row(&row)?;
            let access_token = stored.access_token.expose_secret().to_string();
            let refresh_token = stored
                .refresh_token
                .as_ref()
                .map(|token| token.expose_secret().to_string());
            accounts.push(Account {
                id: stored.id,
                email: stored.email,
                account_id: stored.account_id,
                user_id: stored.user_id,
                label: stored.label,
                plan_type: stored.plan_type,
                access_token,
                refresh_token,
                access_token_expires_at: stored.access_token_expires_at,
                status: stored.status,
                quota_limit_reached: row.get::<i64, _>("quota_limit_reached") != 0,
                quota_cooldown_until: parse_optional_rfc3339(
                    row.get::<Option<String>, _>("quota_cooldown_until"),
                )?,
                cloudflare_cooldown_until: parse_optional_rfc3339(
                    row.get::<Option<String>, _>("cloudflare_cooldown_until"),
                )?,
                request_count: row.get::<i64, _>("usage_request_count").max(0) as u64,
                empty_response_count: row.get::<i64, _>("usage_empty_response_count").max(0) as u64,
                window_request_count: nonnegative_i64_to_u64(
                    row.get::<i64, _>("usage_window_request_count"),
                ),
                window_input_tokens: nonnegative_i64_to_u64(
                    row.get::<i64, _>("usage_window_input_tokens"),
                ),
                window_output_tokens: nonnegative_i64_to_u64(
                    row.get::<i64, _>("usage_window_output_tokens"),
                ),
                window_cached_tokens: nonnegative_i64_to_u64(
                    row.get::<i64, _>("usage_window_cached_tokens"),
                ),
                window_started_at: parse_optional_rfc3339(
                    row.get::<Option<String>, _>("usage_window_started_at"),
                )?,
                window_reset_at: parse_optional_rfc3339(
                    row.get::<Option<String>, _>("usage_window_reset_at"),
                )?,
                limit_window_seconds: optional_positive_i64_to_u64(
                    row.get::<Option<i64>, _>("usage_limit_window_seconds"),
                ),
                added_at: stored.added_at.to_rfc3339(),
                last_used_at: row.get("usage_last_used_at"),
            });
        }
        Ok(accounts)
    }

    pub async fn set_status(
        &self,
        id: &str,
        status: AccountStatus,
    ) -> AccountRepositoryResult<bool> {
        let result = sqlx::query("update accounts set status = ?, updated_at = ? where id = ?")
            .bind(status_to_db(status))
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_label(
        &self,
        id: &str,
        label: Option<String>,
    ) -> AccountRepositoryResult<bool> {
        let result = sqlx::query("update accounts set label = ?, updated_at = ? where id = ?")
            .bind(label)
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete(&self, id: &str) -> AccountRepositoryResult<bool> {
        let result = sqlx::query("delete from accounts where id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_all(&self) -> AccountRepositoryResult<u64> {
        let result = sqlx::query("delete from accounts")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    fn account_from_row(
        &self,
        row: &sqlx::sqlite::SqliteRow,
    ) -> AccountRepositoryResult<StoredAccount> {
        let access_token_cipher = row.get::<String, _>("access_token_cipher");
        let access_token = self.secret_box.decrypt(&access_token_cipher)?;
        let refresh_token = match row.get::<Option<String>, _>("refresh_token_cipher") {
            Some(cipher) => Some(self.secret_box.decrypt(&cipher)?),
            None => None,
        };
        Ok(StoredAccount {
            id: row.get("id"),
            email: row.get("email"),
            account_id: row.get("account_id"),
            user_id: row.get("user_id"),
            label: row.get("label"),
            plan_type: row.get("plan_type"),
            access_token,
            refresh_token,
            access_token_expires_at: parse_optional_rfc3339(
                row.get::<Option<String>, _>("access_token_expires_at"),
            )?,
            status: status_from_db(&row.get::<String, _>("status"))?,
            added_at: parse_rfc3339(&row.get::<String, _>("added_at"))?,
            updated_at: parse_rfc3339(&row.get::<String, _>("updated_at"))?,
        })
    }
}

fn status_to_db(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Active => "active",
        AccountStatus::Expired => "expired",
        AccountStatus::QuotaExhausted => "quota_exhausted",
        AccountStatus::Refreshing => "refreshing",
        AccountStatus::Disabled => "disabled",
        AccountStatus::Banned => "banned",
    }
}

fn status_from_db(value: &str) -> AccountRepositoryResult<AccountStatus> {
    match value {
        "active" => Ok(AccountStatus::Active),
        "expired" => Ok(AccountStatus::Expired),
        "quota_exhausted" => Ok(AccountStatus::QuotaExhausted),
        "refreshing" => Ok(AccountStatus::Refreshing),
        "disabled" => Ok(AccountStatus::Disabled),
        "banned" => Ok(AccountStatus::Banned),
        other => Err(AccountRepositoryError::InvalidStatus(other.to_string())),
    }
}

fn parse_optional_rfc3339(value: Option<String>) -> AccountRepositoryResult<Option<DateTime<Utc>>> {
    value.as_deref().map(parse_rfc3339).transpose()
}

fn parse_rfc3339(value: &str) -> AccountRepositoryResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn nonnegative_i64_to_u64(value: i64) -> u64 {
    value.max(0) as u64
}

fn optional_positive_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn metadata_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> AccountRepositoryResult<StoredAccountMetadata> {
    Ok(StoredAccountMetadata {
        id: row.get("id"),
        email: row.get("email"),
        account_id: row.get("account_id"),
        user_id: row.get("user_id"),
        label: row.get("label"),
        plan_type: row.get("plan_type"),
        access_token_expires_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("access_token_expires_at"),
        )?,
        status: status_from_db(&row.get::<String, _>("status"))?,
        added_at: parse_rfc3339(&row.get::<String, _>("added_at"))?,
        updated_at: parse_rfc3339(&row.get::<String, _>("updated_at"))?,
    })
}
