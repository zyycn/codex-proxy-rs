//! SQLite 账号仓储适配器。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use thiserror::Error;

use crate::infra::{
    json::{decode_cursor, page_offset, NumberedPage, Page},
    time::parse_optional_rfc3339_utc as parse_optional_rfc3339,
};
use crate::upstream::accounts::model::{
    Account, AccountModelUsageDelta, AccountStatus, AccountUsageDelta,
};
use crate::upstream::accounts::window::should_reset_usage_window;

mod queries;
mod rows;

use queries::*;
use rows::{
    count_account_metadata, get_pool_account, list_pool_accounts, map_account_store_error,
    metadata_from_row, optional_positive_i64_to_u64, optional_update_value,
    push_account_metadata_search, quota_plan_type, quota_snapshot_from_row, sqlite_usage_delta,
    status_to_db, stored_account_from_row, to_page, u64_to_i64_saturating,
};

// ============================================================================
// 错误类型
// ============================================================================

/// SQLite 账号仓储错误。
#[derive(Debug, Error)]
pub enum SqliteAccountStoreError {
    /// 数据库错误。
    #[error("sqlite account store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// 时间格式错误。
    #[error("sqlite account store timestamp error: {0}")]
    Timestamp(#[from] chrono::ParseError),
    /// 账号状态非法。
    #[error("sqlite account store status error: {0}")]
    InvalidStatus(String),
    /// 分页游标非法。
    #[error("invalid account pagination cursor")]
    InvalidCursor,
}

/// SQLite 账号仓储结果。
pub type SqliteAccountStoreResult<T> = Result<T, SqliteAccountStoreError>;

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

/// 用量增量（SQLite 内部表示）。
#[derive(Debug, Clone, Copy, Default)]
pub struct UsageDelta {
    request_count: i64,
    empty_response_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    reasoning_tokens: i64,
    total_tokens: i64,
    image_input_tokens: i64,
    image_output_tokens: i64,
    image_request_count: i64,
    image_request_failed_count: i64,
    window_request_count: i64,
    window_input_tokens: i64,
    window_output_tokens: i64,
    window_cached_tokens: i64,
    window_image_input_tokens: i64,
    window_image_output_tokens: i64,
    window_image_request_count: i64,
    window_image_request_failed_count: i64,
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

    /// 记录账号用量增量。
    async fn record_usage_delta(
        &self,
        account_id: &str,
        usage: AccountUsageDelta,
    ) -> AccountStoreResult<()>;

    /// 记录账号模型维度用量增量。
    async fn record_model_usage_delta(
        &self,
        account_id: &str,
        model: &str,
        usage: AccountModelUsageDelta,
    ) -> AccountStoreResult<()>;

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
    async fn sync_runtime_account_state(
        &self,
        account: &Account,
        sync_usage_window: bool,
    ) -> AccountStoreResult<bool>;

    /// 同步账号当前 rate-limit 统计窗口。
    async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> AccountStoreResult<()>;

    /// 记录账号被用于一次外部请求。
    async fn record_request(&self, account_id: &str) -> AccountStoreResult<()> {
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

#[derive(Clone)]
pub struct SqliteAccountStore {
    pool: SqlitePool,
}

impl SqliteAccountStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 返回底层连接池。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 插入新账号。
    pub async fn insert(&self, account: NewAccount) -> SqliteAccountStoreResult<()> {
        let now = Utc::now().to_rfc3339();
        let refresh_token = account
            .refresh_token
            .as_ref()
            .map(ExposeSecret::expose_secret);
        sqlx::query(INSERT_ACCOUNT_SQL)
            .bind(&account.id)
            .bind(&account.email)
            .bind(&account.account_id)
            .bind(&account.user_id)
            .bind(&account.label)
            .bind(&account.plan_type)
            .bind(account.access_token.expose_secret())
            .bind(refresh_token)
            .bind(account.access_token_expires_at.map(|dt| dt.to_rfc3339()))
            .bind(status_to_db(account.status))
            .bind(
                account
                    .added_at
                    .map_or_else(|| Utc::now().to_rfc3339(), |dt| dt.to_rfc3339()),
            )
            .bind(&now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 读取单个账号。
    pub async fn get(&self, account_id: &str) -> SqliteAccountStoreResult<Option<StoredAccount>> {
        let row = sqlx::query(SELECT_STORED_ACCOUNT_BY_ID_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(stored_account_from_row).transpose()
    }

    /// 通过 ChatGPT 身份查找账号。
    pub async fn find_by_chatgpt_identity(
        &self,
        chatgpt_account_id: &str,
        chatgpt_user_id: Option<&str>,
    ) -> SqliteAccountStoreResult<Option<StoredAccount>> {
        let row = sqlx::query(SELECT_STORED_ACCOUNT_BY_CHATGPT_IDENTITY_SQL)
            .bind(chatgpt_account_id)
            .bind(chatgpt_user_id)
            .bind(chatgpt_user_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(stored_account_from_row).transpose()
    }

    /// 分页列出所有账号（含 token）。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteAccountStoreResult<Page<StoredAccount>> {
        let limit = limit.clamp(1, 200);
        if let Some(cursor) = cursor {
            let (added_at, id) =
                decode_cursor(&cursor).ok_or(SqliteAccountStoreError::InvalidCursor)?;
            let rows = sqlx::query(LIST_STORED_ACCOUNTS_AFTER_CURSOR_SQL)
                .bind(&added_at)
                .bind(&added_at)
                .bind(&id)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(
                &rows,
                limit,
                stored_account_from_row,
                ("added_at", "id"),
            ))
        } else {
            let rows = sqlx::query(LIST_STORED_ACCOUNTS_SQL)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(
                &rows,
                limit,
                stored_account_from_row,
                ("added_at", "id"),
            ))
        }
    }

    /// 分页列出账号元数据（不含 token）。
    pub async fn list_metadata(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteAccountStoreResult<Page<StoredAccountMetadata>> {
        let limit = limit.clamp(1, 200);
        if let Some(cursor) = cursor {
            let (added_at, id) =
                decode_cursor(&cursor).ok_or(SqliteAccountStoreError::InvalidCursor)?;
            let rows = sqlx::query(LIST_ACCOUNT_METADATA_AFTER_CURSOR_SQL)
                .bind(&added_at)
                .bind(&added_at)
                .bind(&id)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(&rows, limit, metadata_from_row, ("added_at", "id")))
        } else {
            let rows = sqlx::query(LIST_ACCOUNT_METADATA_SQL)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(&rows, limit, metadata_from_row, ("added_at", "id")))
        }
    }

    /// 按页码列出账号元数据（不含 token）。
    pub async fn list_metadata_page(
        &self,
        page: u32,
        page_size: u32,
        search: Option<&str>,
    ) -> SqliteAccountStoreResult<NumberedPage<StoredAccountMetadata>> {
        let page_size = page_size.clamp(1, 200);
        let search = search.map(str::trim).filter(|value| !value.is_empty());
        let total = count_account_metadata(&self.pool, search).await?;
        let offset = page_offset(page, page_size);

        let mut builder = QueryBuilder::<Sqlite>::new(LIST_ACCOUNT_METADATA_SELECT_SQL);
        push_account_metadata_search(&mut builder, search);
        builder.push(" order by added_at desc, id desc limit ");
        builder.push_bind(i64::from(page_size));
        builder.push(" offset ");
        builder.push_bind(offset.min(i64::MAX as u64) as i64);

        let rows = builder.build().fetch_all(&self.pool).await?;
        let items = rows
            .iter()
            .map(metadata_from_row)
            .collect::<SqliteAccountStoreResult<Vec<_>>>()?;

        Ok(NumberedPage {
            items,
            total,
            page: page.max(1),
            page_size,
        })
    }

    /// 读取单个账号元数据（不含 token）。
    pub async fn get_metadata(
        &self,
        account_id: &str,
    ) -> SqliteAccountStoreResult<Option<StoredAccountMetadata>> {
        let row = sqlx::query(SELECT_ACCOUNT_METADATA_BY_ID_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(metadata_from_row).transpose()
    }

    /// 更新单账号元数据（不含 token）。
    pub async fn update_metadata(
        &self,
        account_id: &str,
        update: AccountMetadataUpdate,
    ) -> SqliteAccountStoreResult<bool> {
        if !update.any() {
            return Ok(false);
        }
        let now = Utc::now().to_rfc3339();
        let status = update.status.map(status_to_db);
        let result = sqlx::query(UPDATE_ACCOUNT_METADATA_SQL)
            .bind(update.email.is_some())
            .bind(optional_update_value(&update.email))
            .bind(update.account_id.is_some())
            .bind(optional_update_value(&update.account_id))
            .bind(update.user_id.is_some())
            .bind(optional_update_value(&update.user_id))
            .bind(update.label.is_some())
            .bind(optional_update_value(&update.label))
            .bind(update.plan_type.is_some())
            .bind(optional_update_value(&update.plan_type))
            .bind(update.status.is_some())
            .bind(status)
            .bind(status)
            .bind(&now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 更新账号 claims（含 refresh token）。
    pub async fn update_from_claims(
        &self,
        account_id: &str,
        update: AccountClaimsUpdate,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let refresh_token = update
            .refresh_token
            .as_ref()
            .map(ExposeSecret::expose_secret);

        let result = if let Some(refresh_token) = refresh_token {
            sqlx::query(UPDATE_ACCOUNT_CLAIMS_WITH_REFRESH_SQL)
                .bind(&update.email)
                .bind(&update.account_id)
                .bind(&update.user_id)
                .bind(&update.plan_type)
                .bind(update.access_token.expose_secret())
                .bind(refresh_token)
                .bind(update.access_token_expires_at.map(|dt| dt.to_rfc3339()))
                .bind(update.next_refresh_at.map(|dt| dt.to_rfc3339()))
                .bind(status_to_db(update.status))
                .bind(&now)
                .bind(account_id)
                .execute(&self.pool)
                .await?
        } else {
            sqlx::query(UPDATE_ACCOUNT_CLAIMS_PRESERVING_REFRESH_SQL)
                .bind(&update.email)
                .bind(&update.account_id)
                .bind(&update.user_id)
                .bind(&update.plan_type)
                .bind(update.access_token.expose_secret())
                .bind(update.access_token_expires_at.map(|dt| dt.to_rfc3339()))
                .bind(update.next_refresh_at.map(|dt| dt.to_rfc3339()))
                .bind(status_to_db(update.status))
                .bind(&now)
                .bind(account_id)
                .execute(&self.pool)
                .await?
        };
        Ok(result.rows_affected() > 0)
    }

    /// 通过导入数据更新已有账号。
    pub async fn update_from_import(
        &self,
        update: ImportedAccountUpdate,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let refresh_token = update
            .account
            .refresh_token
            .as_ref()
            .map(ExposeSecret::expose_secret);
        let quota_json = update.quota_json;
        let quota_fetched_at = update.quota_fetched_at.map(|dt| dt.to_rfc3339());
        let quota_verify_required = if update.quota_verify_required { 1 } else { 0 };

        let result = if let Some(refresh_token) = refresh_token {
            sqlx::query(UPDATE_IMPORTED_ACCOUNT_WITH_REFRESH_SQL)
                .bind(&update.account.email)
                .bind(&update.account.account_id)
                .bind(&update.account.user_id)
                .bind(&update.account.label)
                .bind(&update.account.plan_type)
                .bind(update.account.access_token.expose_secret())
                .bind(refresh_token)
                .bind(
                    update
                        .account
                        .access_token_expires_at
                        .map(|dt| dt.to_rfc3339()),
                )
                .bind(status_to_db(update.account.status))
                .bind(&quota_json)
                .bind(&quota_fetched_at)
                .bind(&quota_fetched_at)
                .bind(quota_verify_required)
                .bind(&now)
                .bind(&update.account.id)
                .execute(&self.pool)
                .await?
        } else {
            sqlx::query(UPDATE_IMPORTED_ACCOUNT_PRESERVING_REFRESH_SQL)
                .bind(&update.account.email)
                .bind(&update.account.account_id)
                .bind(&update.account.user_id)
                .bind(&update.account.label)
                .bind(&update.account.plan_type)
                .bind(update.account.access_token.expose_secret())
                .bind(
                    update
                        .account
                        .access_token_expires_at
                        .map(|dt| dt.to_rfc3339()),
                )
                .bind(status_to_db(update.account.status))
                .bind(&quota_json)
                .bind(&quota_fetched_at)
                .bind(&quota_fetched_at)
                .bind(quota_verify_required)
                .bind(&now)
                .bind(&update.account.id)
                .execute(&self.pool)
                .await?
        };
        Ok(result.rows_affected() > 0)
    }

    /// 设置下一次刷新时间。
    pub async fn set_next_refresh_at(
        &self,
        account_id: &str,
        next_refresh_at: Option<DateTime<Utc>>,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(SET_NEXT_REFRESH_AT_SQL)
            .bind(next_refresh_at.map(|dt| dt.to_rfc3339()))
            .bind(&now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 标记账号进入配额冷却期。
    pub async fn mark_quota_limited_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(MARK_QUOTA_LIMITED_UNTIL_SQL)
            .bind(cooldown_until.to_rfc3339())
            .bind(&now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 同步运行时自然刷新出来的账号状态。
    pub async fn sync_runtime_account_state(
        &self,
        account: &Account,
        sync_usage_window: bool,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let quota_limit_reached = if account.quota_limit_reached { 1 } else { 0 };
        let quota_verify_required = if account.quota_verify_required { 1 } else { 0 };
        let quota_cooldown_until = account.quota_cooldown_until.map(|dt| dt.to_rfc3339());
        let cloudflare_cooldown_until = account.cloudflare_cooldown_until.map(|dt| dt.to_rfc3339());
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(SYNC_RUNTIME_ACCOUNT_STATE_SQL)
            .bind(quota_limit_reached)
            .bind(&now)
            .bind(quota_limit_reached)
            .bind(status_to_db(account.status))
            .bind(quota_limit_reached)
            .bind(&now)
            .bind(quota_limit_reached)
            .bind(quota_verify_required)
            .bind(&now)
            .bind(quota_verify_required)
            .bind(&quota_cooldown_until)
            .bind(&now)
            .bind(&quota_cooldown_until)
            .bind(&cloudflare_cooldown_until)
            .bind(&now)
            .bind(&cloudflare_cooldown_until)
            .bind(&now)
            .bind(&account.id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(false);
        }

        if sync_usage_window {
            sqlx::query(SYNC_RUNTIME_ACCOUNT_USAGE_WINDOW_SQL)
                .bind(&account.id)
                .bind(u64_to_i64_saturating(account.window_request_count))
                .bind(u64_to_i64_saturating(account.window_input_tokens))
                .bind(u64_to_i64_saturating(account.window_output_tokens))
                .bind(u64_to_i64_saturating(account.window_cached_tokens))
                .bind(u64_to_i64_saturating(account.window_image_input_tokens))
                .bind(u64_to_i64_saturating(account.window_image_output_tokens))
                .bind(u64_to_i64_saturating(account.window_image_request_count))
                .bind(u64_to_i64_saturating(
                    account.window_image_request_failed_count,
                ))
                .bind(account.window_started_at.map(|dt| dt.to_rfc3339()))
                .bind(account.window_reset_at.map(|dt| dt.to_rfc3339()))
                .bind(account.limit_window_seconds.map(u64_to_i64_saturating))
                .bind(&now)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(true)
    }

    /// 标记账号 Cloudflare 冷却期。
    pub async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(SET_CLOUDFLARE_COOLDOWN_UNTIL_SQL)
            .bind(cooldown_until.to_rfc3339())
            .bind(&now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 更新账号状态。
    pub async fn set_status(
        &self,
        account_id: &str,
        status: AccountStatus,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let status = status_to_db(status);
        let result = sqlx::query(
            r"
update accounts
set
  status = case
    when ? = 'active' and quota_limit_reached = 1 then 'quota_exhausted'
    else ?
  end,
  updated_at = ?
where id = ?",
        )
        .bind(status)
        .bind(status)
        .bind(&now)
        .bind(account_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 记录用量。
    pub async fn record_usage(
        &self,
        account_id: &str,
        delta: UsageDelta,
    ) -> SqliteAccountStoreResult<()> {
        let last_used_at = Utc::now().to_rfc3339();
        sqlx::query(RECORD_USAGE_SQL)
            .bind(account_id)
            .bind(delta.request_count)
            .bind(delta.empty_response_count)
            .bind(delta.input_tokens)
            .bind(delta.output_tokens)
            .bind(delta.cached_tokens)
            .bind(delta.reasoning_tokens)
            .bind(delta.total_tokens)
            .bind(delta.image_input_tokens)
            .bind(delta.image_output_tokens)
            .bind(delta.image_request_count)
            .bind(delta.image_request_failed_count)
            .bind(delta.window_request_count)
            .bind(delta.window_input_tokens)
            .bind(delta.window_output_tokens)
            .bind(delta.window_cached_tokens)
            .bind(delta.window_image_input_tokens)
            .bind(delta.window_image_output_tokens)
            .bind(delta.window_image_request_count)
            .bind(delta.window_image_request_failed_count)
            .bind(&last_used_at)
            .bind(&last_used_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 记录模型维度用量。
    pub async fn record_model_usage(
        &self,
        account_id: &str,
        model: &str,
        delta: AccountModelUsageDelta,
    ) -> SqliteAccountStoreResult<()> {
        let model = model.trim();
        if model.is_empty() {
            return Ok(());
        }
        let last_used_at = Utc::now().to_rfc3339();
        sqlx::query(RECORD_MODEL_USAGE_SQL)
            .bind(account_id)
            .bind(model)
            .bind(u64_to_i64_saturating(delta.requests))
            .bind(u64_to_i64_saturating(delta.errors))
            .bind(u64_to_i64_saturating(delta.input_tokens))
            .bind(u64_to_i64_saturating(delta.output_tokens))
            .bind(u64_to_i64_saturating(delta.cached_tokens))
            .bind(&last_used_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 读取配额快照列表。
    pub async fn list_quota_snapshots(
        &self,
    ) -> SqliteAccountStoreResult<Vec<AccountQuotaSnapshot>> {
        let rows = sqlx::query(LIST_QUOTA_SNAPSHOTS_SQL)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(quota_snapshot_from_row).collect()
    }

    /// 读取单账号配额 JSON。
    pub async fn get_quota_json(
        &self,
        account_id: &str,
    ) -> SqliteAccountStoreResult<Option<String>> {
        let row = sqlx::query("select quota_json from accounts where id = ?")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|row| row.get::<Option<String>, _>("quota_json")))
    }

    /// 更新配额 JSON。
    pub async fn update_quota_json(
        &self,
        account_id: &str,
        quota_json: &str,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let plan_type = quota_plan_type(quota_json);
        let result = sqlx::query(UPDATE_QUOTA_JSON_SQL)
            .bind(quota_json)
            .bind(&now)
            .bind(&plan_type)
            .bind(&now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 应用配额快照。
    pub async fn apply_quota_snapshot(
        &self,
        account_id: &str,
        quota_json: &str,
        limit_reached: bool,
        cooldown_until: Option<DateTime<Utc>>,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let cooldown = cooldown_until.map(|dt| dt.to_rfc3339());
        let plan_type = quota_plan_type(quota_json);
        let result = sqlx::query(APPLY_QUOTA_SNAPSHOT_SQL)
            .bind(quota_json)
            .bind(&now)
            .bind(&plan_type)
            .bind(if limit_reached { 1 } else { 0 })
            .bind(if limit_reached { 1 } else { 0 })
            .bind(&cooldown)
            .bind(&now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 应用导入的配额状态。
    pub async fn apply_imported_quota_state(
        &self,
        account_id: &str,
        quota_json: Option<&str>,
        quota_fetched_at: Option<DateTime<Utc>>,
        quota_verify_required: bool,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let fetched = quota_fetched_at.map(|dt| dt.to_rfc3339());
        let plan_type = quota_json.and_then(quota_plan_type);
        let result = sqlx::query(APPLY_IMPORTED_QUOTA_STATE_SQL)
            .bind(quota_json)
            .bind(&fetched)
            .bind(&fetched)
            .bind(&plan_type)
            .bind(if quota_verify_required { 1 } else { 0 })
            .bind(&now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 同步 rate-limit 窗口（含重置）。
    pub async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> SqliteAccountStoreResult<()> {
        let existing = sqlx::query(SELECT_RATE_LIMIT_WINDOW_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;

        let should_reset = existing
            .as_ref()
            .map(|row| {
                let existing_reset_at = parse_optional_rfc3339(
                    row.get::<Option<String>, _>("window_reset_at").as_deref(),
                )
                .ok()
                .flatten();
                let existing_limit_window_seconds =
                    optional_positive_i64_to_u64(row.get::<Option<i64>, _>("limit_window_seconds"));
                should_reset_usage_window(
                    existing_reset_at,
                    existing_limit_window_seconds,
                    reset_at,
                    limit_window_seconds,
                )
            })
            .unwrap_or_default();

        if should_reset {
            let reset_at_str = reset_at.to_rfc3339();
            let window_started_at = Utc::now().to_rfc3339();
            sqlx::query(SYNC_RATE_LIMIT_WINDOW_RESET_SQL)
                .bind(account_id)
                .bind(&window_started_at)
                .bind(&reset_at_str)
                .bind(limit_window_seconds.map(|v| v as i64))
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query(SYNC_RATE_LIMIT_WINDOW_SQL)
                .bind(account_id)
                .bind(reset_at.to_rfc3339())
                .bind(limit_window_seconds.map(|v| v as i64))
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// 删除单个账号。
    pub async fn delete(&self, account_id: &str) -> SqliteAccountStoreResult<bool> {
        let result = sqlx::query(DELETE_ACCOUNT_SQL)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

impl SqliteAccountStore {
    /// 设置账号标签。
    pub async fn set_label(
        &self,
        account_id: &str,
        label: Option<String>,
    ) -> Result<bool, SqliteAccountStoreError> {
        let updated_at = Utc::now().to_rfc3339();
        let result = sqlx::query("UPDATE accounts SET label = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(&label)
            .bind(updated_at)
            .bind(account_id)
            .execute(&self.pool)
            .await
            .map_err(SqliteAccountStoreError::Database)?;
        Ok(result.rows_affected() > 0)
    }
}

#[async_trait]
impl AccountStore for SqliteAccountStore {
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
        SqliteAccountStore::mark_quota_limited_until(self, account_id, cooldown_until)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        SqliteAccountStore::set_cloudflare_cooldown_until(self, account_id, cooldown_until)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn set_status(
        &self,
        account_id: &str,
        status: AccountStatus,
    ) -> AccountStoreResult<bool> {
        SqliteAccountStore::set_status(self, account_id, status)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn record_usage_delta(
        &self,
        account_id: &str,
        usage: AccountUsageDelta,
    ) -> AccountStoreResult<()> {
        self.record_usage(account_id, sqlite_usage_delta(usage))
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn record_model_usage_delta(
        &self,
        account_id: &str,
        model: &str,
        usage: AccountModelUsageDelta,
    ) -> AccountStoreResult<()> {
        self.record_model_usage(account_id, model, usage)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn get_quota_json(&self, account_id: &str) -> AccountStoreResult<Option<String>> {
        SqliteAccountStore::get_quota_json(self, account_id)
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
        SqliteAccountStore::apply_quota_snapshot(
            self,
            account_id,
            quota_json,
            limit_reached,
            cooldown_until,
        )
        .await
        .map_err(|error| map_account_store_error(&error))
    }

    async fn sync_runtime_account_state(
        &self,
        account: &Account,
        sync_usage_window: bool,
    ) -> AccountStoreResult<bool> {
        SqliteAccountStore::sync_runtime_account_state(self, account, sync_usage_window)
            .await
            .map_err(|error| map_account_store_error(&error))
    }

    async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> AccountStoreResult<()> {
        SqliteAccountStore::sync_rate_limit_window(self, account_id, reset_at, limit_window_seconds)
            .await
            .map_err(|error| map_account_store_error(&error))
    }
}
