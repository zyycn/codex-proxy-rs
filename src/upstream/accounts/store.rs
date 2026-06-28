//! SQLite 账号仓储适配器。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use sqlx::{sqlite::SqliteRow, QueryBuilder, Row, Sqlite, SqlitePool};
use thiserror::Error;

use crate::infra::json::{decode_cursor, page_offset, NumberedPage, Page};
use crate::upstream::accounts::model::{
    Account, AccountModelUsageDelta, AccountStatus, AccountUsageDelta,
};

// ============================================================================
// SQL 常量
// ============================================================================

const LIST_POOL_ACCOUNTS_SQL: &str = r"
select
  a.id,
  a.email,
  a.chatgpt_account_id as account_id,
  a.chatgpt_user_id as user_id,
  a.label,
  a.plan_type,
  a.access_token,
  a.refresh_token,
  a.access_token_expires_at,
  a.next_refresh_at,
  a.status,
  a.quota_limit_reached,
  a.quota_verify_required,
  a.quota_cooldown_until,
  a.cloudflare_cooldown_until,
  a.added_at,
  coalesce(au.request_count, 0) as usage_request_count,
  coalesce(au.empty_response_count, 0) as usage_empty_response_count,
  coalesce(au.image_input_tokens, 0) as usage_image_input_tokens,
  coalesce(au.image_output_tokens, 0) as usage_image_output_tokens,
  coalesce(au.image_request_count, 0) as usage_image_request_count,
  coalesce(au.image_request_failed_count, 0) as usage_image_request_failed_count,
  coalesce(au.window_request_count, 0) as usage_window_request_count,
  coalesce(au.window_input_tokens, 0) as usage_window_input_tokens,
  coalesce(au.window_output_tokens, 0) as usage_window_output_tokens,
  coalesce(au.window_cached_tokens, 0) as usage_window_cached_tokens,
  coalesce(au.window_image_input_tokens, 0) as usage_window_image_input_tokens,
  coalesce(au.window_image_output_tokens, 0) as usage_window_image_output_tokens,
  coalesce(au.window_image_request_count, 0) as usage_window_image_request_count,
  coalesce(au.window_image_request_failed_count, 0) as usage_window_image_request_failed_count,
  au.window_started_at as usage_window_started_at,
  au.window_reset_at as usage_window_reset_at,
  au.limit_window_seconds as usage_limit_window_seconds,
  au.last_used_at as usage_last_used_at
from accounts a
left join account_usage au on au.account_id = a.id
order by a.added_at desc, a.id desc";

const GET_POOL_ACCOUNT_SQL: &str = r"
select
  a.id,
  a.email,
  a.chatgpt_account_id as account_id,
  a.chatgpt_user_id as user_id,
  a.label,
  a.plan_type,
  a.access_token,
  a.refresh_token,
  a.access_token_expires_at,
  a.next_refresh_at,
  a.status,
  a.quota_limit_reached,
  a.quota_verify_required,
  a.quota_cooldown_until,
  a.cloudflare_cooldown_until,
  a.added_at,
  coalesce(au.request_count, 0) as usage_request_count,
  coalesce(au.empty_response_count, 0) as usage_empty_response_count,
  coalesce(au.image_input_tokens, 0) as usage_image_input_tokens,
  coalesce(au.image_output_tokens, 0) as usage_image_output_tokens,
  coalesce(au.image_request_count, 0) as usage_image_request_count,
  coalesce(au.image_request_failed_count, 0) as usage_image_request_failed_count,
  coalesce(au.window_request_count, 0) as usage_window_request_count,
  coalesce(au.window_input_tokens, 0) as usage_window_input_tokens,
  coalesce(au.window_output_tokens, 0) as usage_window_output_tokens,
  coalesce(au.window_cached_tokens, 0) as usage_window_cached_tokens,
  coalesce(au.window_image_input_tokens, 0) as usage_window_image_input_tokens,
  coalesce(au.window_image_output_tokens, 0) as usage_window_image_output_tokens,
  coalesce(au.window_image_request_count, 0) as usage_window_image_request_count,
  coalesce(au.window_image_request_failed_count, 0) as usage_window_image_request_failed_count,
  au.window_started_at as usage_window_started_at,
  au.window_reset_at as usage_window_reset_at,
  au.limit_window_seconds as usage_limit_window_seconds,
  au.last_used_at as usage_last_used_at
from accounts a
left join account_usage au on au.account_id = a.id
where a.id = ?";

const INSERT_ACCOUNT_SQL: &str = r"
insert into accounts (
  id,
  email,
  chatgpt_account_id,
  chatgpt_user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  status,
  added_at,
  updated_at
) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

const SELECT_STORED_ACCOUNT_BY_ID_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  next_refresh_at,
  status,
  added_at,
  updated_at
from accounts
where id = ?";

const SELECT_STORED_ACCOUNT_BY_CHATGPT_IDENTITY_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  next_refresh_at,
  status,
  added_at,
  updated_at
from accounts
where chatgpt_account_id = ?
  and ((chatgpt_user_id is null and ? is null) or chatgpt_user_id = ?)
order by added_at asc
limit 1";

const SELECT_ACCOUNT_METADATA_BY_ID_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
where id = ?";

const UPDATE_ACCOUNT_METADATA_SQL: &str = r"
update accounts
set
  email = case when ? then ? else email end,
  chatgpt_account_id = case when ? then ? else chatgpt_account_id end,
  chatgpt_user_id = case when ? then ? else chatgpt_user_id end,
  label = case when ? then ? else label end,
  plan_type = case when ? then ? else plan_type end,
  status = case when ? then ? else status end,
  updated_at = ?
where id = ?";

const LIST_STORED_ACCOUNTS_AFTER_CURSOR_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  next_refresh_at,
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
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  next_refresh_at,
  status,
  added_at,
  updated_at
from accounts
order by added_at desc, id desc
limit ?";

const LIST_ACCOUNT_METADATA_AFTER_CURSOR_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
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

const LIST_ACCOUNT_METADATA_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
order by added_at desc, id desc
limit ?";

const LIST_ACCOUNT_METADATA_SELECT_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts";

const RECORD_USAGE_SQL: &str = r"
insert into account_usage (
  account_id,
  request_count,
  empty_response_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  reasoning_tokens,
  total_tokens,
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
) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
on conflict(account_id) do update set
  request_count = request_count + excluded.request_count,
  empty_response_count = empty_response_count + excluded.empty_response_count,
  input_tokens = input_tokens + excluded.input_tokens,
  output_tokens = output_tokens + excluded.output_tokens,
  cached_tokens = cached_tokens + excluded.cached_tokens,
  reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens,
  total_tokens = total_tokens + excluded.total_tokens,
  image_input_tokens = image_input_tokens + excluded.image_input_tokens,
  image_output_tokens = image_output_tokens + excluded.image_output_tokens,
  image_request_count = image_request_count + excluded.image_request_count,
  image_request_failed_count = image_request_failed_count + excluded.image_request_failed_count,
  window_request_count = window_request_count + excluded.window_request_count,
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

const RECORD_MODEL_USAGE_SQL: &str = r"
insert into account_model_usage (
  account_id,
  model,
  request_count,
  error_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  last_used_at
) values (?, ?, ?, ?, ?, ?, ?, ?)
on conflict(account_id, model) do update set
  request_count = request_count + excluded.request_count,
  error_count = error_count + excluded.error_count,
  input_tokens = input_tokens + excluded.input_tokens,
  output_tokens = output_tokens + excluded.output_tokens,
  cached_tokens = cached_tokens + excluded.cached_tokens,
  last_used_at = excluded.last_used_at";

const LIST_MODEL_USAGE_SQL: &str = r"
select
  account_id,
  model,
  request_count,
  error_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  last_used_at
from account_model_usage
order by account_id asc, request_count desc, last_used_at desc, model asc";

const LIST_QUOTA_SNAPSHOTS_SQL: &str = r"
select
  id,
  email,
  quota_json,
  quota_fetched_at
from accounts
where quota_json is not null
  and trim(quota_json) <> ''
order by coalesce(quota_fetched_at, '') desc, id desc";

const UPDATE_QUOTA_JSON_SQL: &str = r"
update accounts
set
  quota_json = ?,
  quota_fetched_at = ?,
  plan_type = coalesce(?, plan_type),
  updated_at = ?
where id = ?";

const APPLY_QUOTA_SNAPSHOT_SQL: &str = r"
update accounts
set
  quota_json = ?,
  quota_fetched_at = ?,
  plan_type = coalesce(?, plan_type),
  quota_limit_reached = ?,
  quota_verify_required = 0,
  quota_cooldown_until = ?,
  updated_at = ?
where id = ?";

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

const MARK_QUOTA_LIMITED_UNTIL_SQL: &str = r"
update accounts
set
  quota_limit_reached = 1,
  quota_verify_required = 0,
  quota_cooldown_until = ?,
  updated_at = ?
where id = ?";

const SET_CLOUDFLARE_COOLDOWN_UNTIL_SQL: &str = r"
update accounts
set
  cloudflare_cooldown_until = ?,
  updated_at = ?
where id = ?";

const UPDATE_ACCOUNT_CLAIMS_WITH_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  chatgpt_account_id = ?,
  chatgpt_user_id = ?,
  plan_type = ?,
  access_token = ?,
  refresh_token = ?,
  access_token_expires_at = ?,
  next_refresh_at = ?,
  status = ?,
  updated_at = ?
where id = ?";

const UPDATE_ACCOUNT_CLAIMS_PRESERVING_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  chatgpt_account_id = ?,
  chatgpt_user_id = ?,
  plan_type = ?,
  access_token = ?,
  access_token_expires_at = ?,
  next_refresh_at = ?,
  status = ?,
  updated_at = ?
where id = ?";

const SET_NEXT_REFRESH_AT_SQL: &str = r"
update accounts
set
  next_refresh_at = ?,
  updated_at = ?
where id = ?";

const UPDATE_IMPORTED_ACCOUNT_WITH_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  chatgpt_account_id = ?,
  chatgpt_user_id = ?,
  label = ?,
  plan_type = ?,
  access_token = ?,
  refresh_token = ?,
  access_token_expires_at = ?,
  status = ?,
  quota_json = coalesce(?, quota_json),
  quota_fetched_at = case when ? is null then quota_fetched_at else ? end,
  quota_limit_reached = 0,
  quota_cooldown_until = null,
  quota_verify_required = ?,
  updated_at = ?
where id = ?";

const UPDATE_IMPORTED_ACCOUNT_PRESERVING_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  chatgpt_account_id = ?,
  chatgpt_user_id = ?,
  label = ?,
  plan_type = ?,
  access_token = ?,
  access_token_expires_at = ?,
  status = ?,
  quota_json = coalesce(?, quota_json),
  quota_fetched_at = case when ? is null then quota_fetched_at else ? end,
  quota_limit_reached = 0,
  quota_cooldown_until = null,
  quota_verify_required = ?,
  updated_at = ?
where id = ?";

const APPLY_IMPORTED_QUOTA_STATE_SQL: &str = r"
update accounts
set
  quota_json = coalesce(?, quota_json),
  quota_fetched_at = case when ? is null then quota_fetched_at else ? end,
  plan_type = coalesce(?, plan_type),
  quota_limit_reached = 0,
  quota_cooldown_until = null,
  quota_verify_required = ?,
  updated_at = ?
where id = ?";

const DELETE_ACCOUNT_SQL: &str = "delete from accounts where id = ?";

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

/// 账号模型维度用量记录。
#[derive(Debug, Clone)]
pub struct AccountModelUsageRecord {
    /// 账号 ID。
    pub account_id: String,
    /// 模型 ID。
    pub model: String,
    /// 历史请求总数。
    pub request_count: i64,
    /// 历史错误数。
    pub error_count: i64,
    /// 累计输入 token。
    pub input_tokens: i64,
    /// 累计输出 token。
    pub output_tokens: i64,
    /// 累计缓存 token。
    pub cached_tokens: i64,
    /// 最近使用时间。
    pub last_used_at: Option<DateTime<Utc>>,
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

    /// 更新账号当前配额 JSON。
    async fn update_quota_json(
        &self,
        account_id: &str,
        quota_json: &str,
    ) -> AccountStoreResult<bool>;

    /// 应用已经验证过的账号配额快照。
    async fn apply_quota_snapshot(
        &self,
        account_id: &str,
        quota_json: &str,
        limit_reached: bool,
        cooldown_until: Option<DateTime<Utc>>,
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
        let result = sqlx::query("update accounts set status = ?, updated_at = ? where id = ?")
            .bind(status_to_db(status))
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

    /// 列出模型维度用量。
    pub async fn list_model_usage(&self) -> SqliteAccountStoreResult<Vec<AccountModelUsageRecord>> {
        let rows = sqlx::query(LIST_MODEL_USAGE_SQL)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(model_usage_from_row).collect()
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

    async fn update_quota_json(
        &self,
        account_id: &str,
        quota_json: &str,
    ) -> AccountStoreResult<bool> {
        SqliteAccountStore::update_quota_json(self, account_id, quota_json)
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

// ============================================================================
// 私有辅助函数
// ============================================================================

async fn list_pool_accounts(store: &SqliteAccountStore) -> SqliteAccountStoreResult<Vec<Account>> {
    let rows = sqlx::query(LIST_POOL_ACCOUNTS_SQL)
        .fetch_all(&store.pool)
        .await?;
    let mut accounts = Vec::with_capacity(rows.len());

    for row in rows {
        accounts.push(pool_account_from_row(&row)?);
    }

    Ok(accounts)
}

async fn get_pool_account(
    store: &SqliteAccountStore,
    account_id: &str,
) -> SqliteAccountStoreResult<Option<Account>> {
    let row = sqlx::query(GET_POOL_ACCOUNT_SQL)
        .bind(account_id)
        .fetch_optional(&store.pool)
        .await?;
    row.map(|row| pool_account_from_row(&row)).transpose()
}

fn pool_account_from_row(row: &SqliteRow) -> SqliteAccountStoreResult<Account> {
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
        )?,
        limit_window_seconds: optional_positive_i64_to_u64(
            row.get::<Option<i64>, _>("usage_limit_window_seconds"),
        ),
        added_at: row.get("added_at"),
        last_used_at: row.get("usage_last_used_at"),
    })
}

fn stored_account_from_row(row: &SqliteRow) -> SqliteAccountStoreResult<StoredAccount> {
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

fn metadata_from_row(row: &SqliteRow) -> SqliteAccountStoreResult<StoredAccountMetadata> {
    Ok(StoredAccountMetadata {
        id: row.get("id"),
        email: row.get("email"),
        account_id: row.get("account_id"),
        user_id: row.get("user_id"),
        label: row.get("label"),
        plan_type: row.get("plan_type"),
        access_token_expires_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("access_token_expires_at")
                .as_deref(),
        )?,
        status: status_from_db(&row.get::<String, _>("status"))?,
        added_at: row.get("added_at"),
        updated_at: row.get("updated_at"),
    })
}

fn map_account_store_error(error: &impl ToString) -> AccountStoreError {
    AccountStoreError::OperationFailed {
        message: error.to_string(),
    }
}

fn sqlite_usage_delta(usage: AccountUsageDelta) -> UsageDelta {
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

fn quota_plan_type(quota_json: &str) -> Option<String> {
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

fn model_usage_from_row(
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

fn quota_snapshot_from_row(
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

fn optional_update_value(value: &Option<Option<String>>) -> Option<&str> {
    value.as_ref().and_then(|value| value.as_deref())
}

fn status_from_db(value: &str) -> SqliteAccountStoreResult<AccountStatus> {
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

fn parse_optional_rfc3339(value: Option<&str>) -> SqliteAccountStoreResult<Option<DateTime<Utc>>> {
    value.map(parse_rfc3339).transpose()
}

fn parse_rfc3339(value: &str) -> SqliteAccountStoreResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn nonnegative_i64_to_u64(value: i64) -> u64 {
    value.max(0).cast_unsigned()
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn optional_positive_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > 0)
}

async fn count_account_metadata(
    pool: &SqlitePool,
    search: Option<&str>,
) -> SqliteAccountStoreResult<u64> {
    let mut builder = QueryBuilder::<Sqlite>::new("select count(*) from accounts");
    push_account_metadata_search(&mut builder, search);
    let (total,): (i64,) = builder.build_query_as().fetch_one(pool).await?;
    Ok(total.max(0).cast_unsigned())
}

fn push_account_metadata_search(builder: &mut QueryBuilder<Sqlite>, search: Option<&str>) {
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

fn to_page<T>(
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
