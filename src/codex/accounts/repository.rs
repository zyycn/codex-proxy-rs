use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use sqlx::{Row, SqlitePool};
use thiserror::Error;

use crate::{
    codex::accounts::model::{Account, AccountStatus},
    platform::crypto::{CryptoError, SecretBox},
    utils::pagination::{decode_cursor, encode_cursor, Page},
};

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageRecord {
    pub account_id: String,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageListRecord {
    pub account_id: String,
    pub email: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub request_count: i64,
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
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
}

#[derive(Clone)]
pub struct AccountRepository {
    pool: SqlitePool,
    secret_box: SecretBox,
}

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
        sqlx::query(
            "insert into accounts (id, email, account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher, access_token_expires_at, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(account.id)
        .bind(account.email)
        .bind(account.account_id)
        .bind(account.user_id)
        .bind(account.label)
        .bind(account.plan_type)
        .bind(access_token_cipher)
        .bind(refresh_token_cipher)
        .bind(account.access_token_expires_at.map(|value| value.to_rfc3339()))
        .bind(status_to_db(account.status))
        .bind(&now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get(&self, id: &str) -> AccountRepositoryResult<Option<StoredAccount>> {
        let row = sqlx::query(
            "select id, email, account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher, access_token_expires_at, status, added_at, updated_at from accounts where id = ?",
        )
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
        let row = sqlx::query(
            "select id, email, account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher, access_token_expires_at, status, added_at, updated_at from accounts where account_id = ? and ((user_id is null and ? is null) or user_id = ?) order by added_at asc limit 1",
        )
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
            sqlx::query(
                "select id, email, account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher, access_token_expires_at, status, added_at, updated_at from accounts where added_at < ? or (added_at = ? and id < ?) order by added_at desc, id desc limit ?",
            )
            .bind(&created_at)
            .bind(created_at)
            .bind(id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "select id, email, account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher, access_token_expires_at, status, added_at, updated_at from accounts order by added_at desc, id desc limit ?",
            )
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
        let rows = sqlx::query(
            "select id, email, account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher, access_token_expires_at, status, added_at, updated_at from accounts order by added_at desc, id desc",
        )
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
            sqlx::query(
                "select id, email, account_id, user_id, label, plan_type, access_token_expires_at, status, added_at, updated_at from accounts where added_at < ? or (added_at = ? and id < ?) order by added_at desc, id desc limit ?",
            )
            .bind(&created_at)
            .bind(created_at)
            .bind(id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "select id, email, account_id, user_id, label, plan_type, access_token_expires_at, status, added_at, updated_at from accounts order by added_at desc, id desc limit ?",
            )
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
        let rows = sqlx::query(
            "select id, email, account_id, user_id, label, plan_type, access_token_expires_at, status, added_at, updated_at from accounts order by added_at desc, id desc",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut accounts = Vec::with_capacity(rows.len());
        for row in rows {
            accounts.push(metadata_from_row(&row)?);
        }
        Ok(accounts)
    }

    pub async fn list_pool_accounts(&self) -> AccountRepositoryResult<Vec<Account>> {
        let rows = sqlx::query(
            "select accounts.id, email, accounts.account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher, access_token_expires_at, status, added_at, updated_at, quota_limit_reached, quota_cooldown_until, cloudflare_cooldown_until, account_usage.last_used_at as usage_last_used_at from accounts left join account_usage on account_usage.account_id = accounts.id order by added_at desc, accounts.id desc",
        )
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
                added_at: stored.added_at.to_rfc3339(),
                last_used_at: row.get("usage_last_used_at"),
            });
        }
        Ok(accounts)
    }

    pub async fn list_quota_snapshots(&self) -> AccountRepositoryResult<Vec<AccountQuotaSnapshot>> {
        let rows = sqlx::query(
            "select id, email, quota_json, quota_fetched_at from accounts where quota_json is not null and trim(quota_json) <> '' order by coalesce(quota_fetched_at, '') desc, id desc",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut snapshots = Vec::with_capacity(rows.len());
        for row in rows {
            snapshots.push(quota_snapshot_from_row(&row)?);
        }
        Ok(snapshots)
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

    pub async fn update_tokens(
        &self,
        id: &str,
        update: TokenUpdate,
    ) -> AccountRepositoryResult<bool> {
        let now = Utc::now().to_rfc3339();
        let access_token_cipher = self.secret_box.encrypt(&update.access_token)?;
        let expires_at = update
            .access_token_expires_at
            .map(|value| value.to_rfc3339());
        let result = if let Some(refresh_token) = update.refresh_token {
            let refresh_token_cipher = self.secret_box.encrypt(&refresh_token)?;
            sqlx::query(
                "update accounts set access_token_cipher = ?, refresh_token_cipher = ?, access_token_expires_at = ?, status = case when status in ('disabled', 'banned') then status else 'active' end, updated_at = ? where id = ?",
            )
            .bind(access_token_cipher)
            .bind(refresh_token_cipher)
            .bind(expires_at)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?
        } else {
            // 刷新接口可能不返回新的 refresh_token；此时必须保留旧 RT，避免账号失去后续刷新能力。
            sqlx::query(
                "update accounts set access_token_cipher = ?, access_token_expires_at = ?, status = case when status in ('disabled', 'banned') then status else 'active' end, updated_at = ? where id = ?",
            )
            .bind(access_token_cipher)
            .bind(expires_at)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?
        };
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_from_claims(
        &self,
        id: &str,
        update: AccountClaimsUpdate,
    ) -> AccountRepositoryResult<bool> {
        let now = Utc::now().to_rfc3339();
        let access_token_cipher = self.secret_box.encrypt(&update.access_token)?;
        let expires_at = update
            .access_token_expires_at
            .map(|value| value.to_rfc3339());
        let result = if let Some(refresh_token) = update.refresh_token {
            let refresh_token_cipher = self.secret_box.encrypt(&refresh_token)?;
            sqlx::query(
                "update accounts set email = ?, account_id = ?, user_id = ?, plan_type = ?, access_token_cipher = ?, refresh_token_cipher = ?, access_token_expires_at = ?, status = ?, updated_at = ? where id = ?",
            )
            .bind(update.email)
            .bind(update.account_id)
            .bind(update.user_id)
            .bind(update.plan_type)
            .bind(access_token_cipher)
            .bind(refresh_token_cipher)
            .bind(expires_at)
            .bind(status_to_db(update.status))
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?
        } else {
            // OpenAI 刷新/导入未给新 RT 时保留原值，避免把可继续刷新的账号写坏。
            sqlx::query(
                "update accounts set email = ?, account_id = ?, user_id = ?, plan_type = ?, access_token_cipher = ?, access_token_expires_at = ?, status = ?, updated_at = ? where id = ?",
            )
            .bind(update.email)
            .bind(update.account_id)
            .bind(update.user_id)
            .bind(update.plan_type)
            .bind(access_token_cipher)
            .bind(expires_at)
            .bind(status_to_db(update.status))
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?
        };
        Ok(result.rows_affected() > 0)
    }

    pub async fn record_usage(
        &self,
        account_id: &str,
        usage: UsageDelta,
    ) -> AccountRepositoryResult<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, 1, ?, ?, ?, ?) on conflict(account_id) do update set request_count = request_count + 1, input_tokens = input_tokens + excluded.input_tokens, output_tokens = output_tokens + excluded.output_tokens, cached_tokens = cached_tokens + excluded.cached_tokens, last_used_at = excluded.last_used_at",
        )
        .bind(account_id)
        .bind(usage.input_tokens)
        .bind(usage.output_tokens)
        .bind(usage.cached_tokens)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_usage(
        &self,
        account_id: &str,
    ) -> AccountRepositoryResult<Option<AccountUsageRecord>> {
        let row = sqlx::query(
            "select account_id, request_count, input_tokens, output_tokens, cached_tokens, last_used_at from account_usage where account_id = ?",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| usage_from_row(&row)).transpose()
    }

    pub async fn update_quota_json(
        &self,
        account_id: &str,
        quota_json: &str,
    ) -> AccountRepositoryResult<bool> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
        )
        .bind(quota_json)
        .bind(&now)
        .bind(now)
        .bind(account_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_quota_cooldown_until(
        &self,
        id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountRepositoryResult<bool> {
        let result = sqlx::query(
            "update accounts set quota_limit_reached = 1, quota_cooldown_until = ?, updated_at = ? where id = ?",
        )
        .bind(cooldown_until.to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_cloudflare_cooldown_until(
        &self,
        id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountRepositoryResult<bool> {
        let result = sqlx::query(
            "update accounts set cloudflare_cooldown_until = ?, updated_at = ? where id = ?",
        )
        .bind(cooldown_until.to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
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

fn usage_from_row(row: &sqlx::sqlite::SqliteRow) -> AccountRepositoryResult<AccountUsageRecord> {
    Ok(AccountUsageRecord {
        account_id: row.get("account_id"),
        request_count: row.get("request_count"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        last_used_at: parse_optional_rfc3339(row.get::<Option<String>, _>("last_used_at"))?,
    })
}

fn quota_snapshot_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> AccountRepositoryResult<AccountQuotaSnapshot> {
    Ok(AccountQuotaSnapshot {
        account_id: row.get("id"),
        email: row.get("email"),
        quota_json: row.get("quota_json"),
        quota_fetched_at: parse_optional_rfc3339(row.get::<Option<String>, _>("quota_fetched_at"))?,
    })
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
            sqlx::query(
                "select account_usage.account_id, accounts.email, accounts.label, accounts.plan_type, account_usage.request_count, account_usage.input_tokens, account_usage.output_tokens, account_usage.cached_tokens, account_usage.last_used_at from account_usage left join accounts on accounts.id = account_usage.account_id where coalesce(account_usage.last_used_at, '') < ? or (coalesce(account_usage.last_used_at, '') = ? and account_usage.account_id < ?) order by coalesce(account_usage.last_used_at, '') desc, account_usage.account_id desc limit ?",
            )
            .bind(&last_used_at)
            .bind(last_used_at)
            .bind(account_id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "select account_usage.account_id, accounts.email, accounts.label, accounts.plan_type, account_usage.request_count, account_usage.input_tokens, account_usage.output_tokens, account_usage.cached_tokens, account_usage.last_used_at from account_usage left join accounts on accounts.id = account_usage.account_id order by coalesce(account_usage.last_used_at, '') desc, account_usage.account_id desc limit ?",
            )
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
        let row = sqlx::query(
            "select count(*) as account_count, coalesce(sum(request_count), 0) as request_count, coalesce(sum(input_tokens), 0) as input_tokens, coalesce(sum(output_tokens), 0) as output_tokens, coalesce(sum(cached_tokens), 0) as cached_tokens from account_usage",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(AccountUsageSummary {
            account_count: row.get("account_count"),
            request_count: row.get("request_count"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cached_tokens: row.get("cached_tokens"),
        })
    }

    pub async fn reset_account(&self, account_id: &str) -> AccountRepositoryResult<()> {
        sqlx::query(
            "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, 0, 0, 0, 0, null) on conflict(account_id) do update set request_count = 0, input_tokens = 0, output_tokens = 0, cached_tokens = 0, last_used_at = null",
        )
        .bind(account_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
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
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        last_used_at: parse_optional_rfc3339(row.get::<Option<String>, _>("last_used_at"))?,
    })
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
