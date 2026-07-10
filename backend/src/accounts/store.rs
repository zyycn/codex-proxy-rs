//! PostgreSQL 账号仓储适配器。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use thiserror::Error;

use crate::accounts::account::{Account, AccountStatus};
use crate::infra::{
    json::{decode_cursor, page_offset, NumberedPage, Page},
    time::parse_rfc3339_utc,
};

mod queries;
mod rows;

use queries::*;
use rows::{
    count_account_metadata, get_pool_account, list_pool_accounts, map_account_store_error,
    metadata_from_row, optional_update_value, push_account_metadata_search, quota_plan_type,
    quota_snapshot_from_row, status_to_db, stored_account_from_row, to_page,
};

// ============================================================================
// 错误类型
// ============================================================================

/// PostgreSQL 账号仓储错误。
#[derive(Debug, Error)]
pub enum PgAccountStoreError {
    /// 数据库错误。
    #[error("PostgreSQL account store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// 时间格式错误。
    #[error("account store timestamp error: {0}")]
    Timestamp(#[from] chrono::ParseError),
    #[error("account store JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// 账号状态非法。
    #[error("PostgreSQL account store status error: {0}")]
    InvalidStatus(String),
    /// 分页游标非法。
    #[error("invalid account pagination cursor")]
    InvalidCursor,
}

/// PostgreSQL 账号仓储结果。
pub type PgAccountStoreResult<T> = Result<T, PgAccountStoreError>;

// ============================================================================
// 数据类型
// ============================================================================

/// 新建账号数据。
#[derive(Debug)]
pub struct NewAccount {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// access token 明文。
    pub access_token: SecretString,
    /// refresh token 明文。
    pub refresh_token: Option<SecretString>,
    /// access token 过期时间。
    pub access_token_expires_at: Option<DateTime<Utc>>,
    /// 初始状态。
    pub status: AccountStatus,
    /// 账号添加时间。
    pub added_at: Option<DateTime<Utc>>,
}

/// 通过 JWT claims 更新已有账号。
#[derive(Debug)]
pub struct AccountClaimsUpdate {
    /// 展示邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// 新 access token。
    pub access_token: SecretString,
    /// 可选的新 refresh token。
    pub refresh_token: Option<SecretString>,
    /// access token 过期时间。
    pub access_token_expires_at: Option<DateTime<Utc>>,
    /// 下一次刷新时间。
    pub next_refresh_at: Option<DateTime<Utc>>,
    /// 更新后的状态。
    pub status: AccountStatus,
}

/// 通过导入数据更新已有账号。
#[derive(Debug)]
pub struct ImportedAccountUpdate {
    /// 新建账号数据。
    pub account: NewAccount,
    /// 缓存配额 JSON。
    pub quota_json: Option<String>,
    /// 配额抓取时间。
    pub quota_fetched_at: Option<DateTime<Utc>>,
    /// 是否需要执行额外配额校验。
    pub quota_verify_required: bool,
}

/// 存储中的账号完整记录。
#[derive(Debug, Clone)]
pub struct StoredAccount {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// access token 明文。
    pub access_token: SecretString,
    /// refresh token 明文。
    pub refresh_token: Option<SecretString>,
    /// access token 过期时间。
    pub access_token_expires_at: Option<DateTime<Utc>>,
    /// 下一次刷新时间。
    pub next_refresh_at: Option<DateTime<Utc>>,
    /// 账号状态。
    pub status: AccountStatus,
    /// 添加时间。
    pub added_at: String,
    /// 更新时间。
    pub updated_at: String,
}

/// 存储中的账号元数据（不含 token）。
#[derive(Debug, Clone)]
pub struct StoredAccountMetadata {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// 是否保存了 refresh token。
    pub has_refresh_token: bool,
    /// access token 过期时间。
    pub access_token_expires_at: Option<DateTime<Utc>>,
    /// 账号状态。
    pub status: AccountStatus,
    /// 添加时间。
    pub added_at: String,
    /// 更新时间。
    pub updated_at: String,
}

#[derive(Debug, Clone, Default)]
pub struct AccountMetadataUpdate {
    pub email: Option<Option<String>>,
    pub account_id: Option<Option<String>>,
    pub user_id: Option<Option<String>>,
    pub label: Option<Option<String>>,
    pub plan_type: Option<Option<String>>,
    pub status: Option<AccountStatus>,
}

impl AccountMetadataUpdate {
    pub fn any(&self) -> bool {
        self.email.is_some()
            || self.account_id.is_some()
            || self.user_id.is_some()
            || self.label.is_some()
            || self.plan_type.is_some()
            || self.status.is_some()
    }
}

/// 账号配额快照。
#[derive(Debug, Clone)]
pub struct AccountQuotaSnapshot {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 配额 JSON。
    pub quota_json: String,
    /// 配额抓取时间。
    pub quota_fetched_at: Option<DateTime<Utc>>,
}

// ============================================================================
// AccountStore 端口 trait
// ============================================================================

/// 账号存储错误。
#[derive(Debug, Error)]
pub enum AccountStoreError {
    /// 底层存储失败。
    #[error("account store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 账号存储结果类型。
pub type AccountStoreResult<T> = Result<T, AccountStoreError>;

/// 提供运行时账号列表的端口。
#[async_trait]
pub trait AccountStore: Send + Sync + 'static {
    /// 列出当前账号池可见的账号。
    async fn list_pool_accounts(&self) -> AccountStoreResult<Vec<Account>>;

    /// 读取单个账号池账号快照。
    async fn get_pool_account(&self, account_id: &str) -> AccountStoreResult<Option<Account>> {
        let accounts = self.list_pool_accounts().await?;
        Ok(accounts
            .into_iter()
            .find(|account| account.id == account_id))
    }

    /// 标记账号进入配额冷却期。
    async fn mark_quota_limited_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool>;

    /// 标记账号进入 Cloudflare 冷却期。
    async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool>;

    /// 更新账号状态。
    async fn set_status(&self, account_id: &str, status: AccountStatus)
        -> AccountStoreResult<bool>;

    /// 读取账号当前配额 JSON。
    async fn get_quota_json(&self, account_id: &str) -> AccountStoreResult<Option<String>>;

    /// 应用已经验证过的账号配额快照。
    async fn apply_quota_snapshot(
        &self,
        account_id: &str,
        quota_json: &str,
        limit_reached: bool,
        cooldown_until: Option<DateTime<Utc>>,
    ) -> AccountStoreResult<bool>;

    /// 同步运行时自然刷新出来的账号状态。
    async fn sync_runtime_account_state(&self, account: &Account) -> AccountStoreResult<bool>;
}

#[derive(Clone)]
pub struct PgAccountStore {
    pool: PgPool,
}

mod write;

impl PgAccountStore {
    /// 设置账号标签。
    pub async fn set_label(
        &self,
        account_id: &str,
        label: Option<String>,
    ) -> Result<bool, PgAccountStoreError> {
        let updated_at = Utc::now();
        let result = sqlx::query("update accounts set label = $1, updated_at = $2 where id = $3")
            .bind(&label)
            .bind(updated_at)
            .bind(account_id)
            .execute(&self.pool)
            .await
            .map_err(PgAccountStoreError::Database)?;
        Ok(result.rows_affected() > 0)
    }
}

#[async_trait]
impl AccountStore for PgAccountStore {
    async fn list_pool_accounts(&self) -> AccountStoreResult<Vec<Account>> {
        list_pool_accounts(self)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn get_pool_account(&self, account_id: &str) -> AccountStoreResult<Option<Account>> {
        get_pool_account(self, account_id)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn mark_quota_limited_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        PgAccountStore::mark_quota_limited_until(self, account_id, cooldown_until)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        PgAccountStore::set_cloudflare_cooldown_until(self, account_id, cooldown_until)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn set_status(
        &self,
        account_id: &str,
        status: AccountStatus,
    ) -> AccountStoreResult<bool> {
        PgAccountStore::set_status(self, account_id, status)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn get_quota_json(&self, account_id: &str) -> AccountStoreResult<Option<String>> {
        PgAccountStore::get_quota_json(self, account_id)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn apply_quota_snapshot(
        &self,
        account_id: &str,
        quota_json: &str,
        limit_reached: bool,
        cooldown_until: Option<DateTime<Utc>>,
    ) -> AccountStoreResult<bool> {
        PgAccountStore::apply_quota_snapshot(
            self,
            account_id,
            quota_json,
            limit_reached,
            cooldown_until,
        )
        .await
        .map_err(|error| map_account_store_error(&error))
    }

    async fn sync_runtime_account_state(&self, account: &Account) -> AccountStoreResult<bool> {
        PgAccountStore::sync_runtime_account_state(self, account)
            .await
            .map_err(|error| map_account_store_error(&error))
    }
}
