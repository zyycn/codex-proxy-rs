use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use sqlx::{Row, SqlitePool};
use thiserror::Error;

use crate::{
    accounts::model::{Account, AccountStatus},
    crypto::{CryptoError, SecretBox},
    pagination::{decode_cursor, encode_cursor, Page},
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

    pub async fn list_pool_accounts(&self) -> AccountRepositoryResult<Vec<Account>> {
        let rows = sqlx::query(
            "select accounts.id, email, accounts.account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher, access_token_expires_at, status, added_at, updated_at, account_usage.last_used_at as usage_last_used_at from accounts left join account_usage on account_usage.account_id = accounts.id order by added_at desc, accounts.id desc",
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
                quota_limit_reached: false,
                cloudflare_cooldown_until: None,
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
