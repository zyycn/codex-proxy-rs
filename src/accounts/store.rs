//! SQLite 账号仓储适配器。

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use thiserror::Error;

pub use crate::accounts::model::{Account, AccountStatus, AccountUsageDelta};
use uuid::Uuid;

use crate::infra::crypto::{CryptoError, SecretBox};
use crate::infra::json::{decode_cursor, Page};

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
  a.access_token_cipher,
  a.refresh_token_cipher,
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
  a.access_token_cipher,
  a.refresh_token_cipher,
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
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token_cipher,
  refresh_token_cipher,
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
  access_token_cipher,
  refresh_token_cipher,
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

const LIST_STORED_ACCOUNTS_AFTER_CURSOR_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token_cipher,
  refresh_token_cipher,
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
  access_token_cipher,
  refresh_token_cipher,
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

const GET_USAGE_SQL: &str = r"
select
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
  window_started_at,
  window_reset_at,
  limit_window_seconds,
  last_used_at
from account_usage
where account_id = ?";

const LIST_USAGE_AFTER_CURSOR_SQL: &str = r"
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
  au.last_used_at
from account_usage au
left join accounts a on a.id = au.account_id
where au.last_used_at < ?
   or (au.last_used_at = ? and au.account_id < ?)
order by au.last_used_at desc, au.account_id desc
limit ?";

const LIST_USAGE_SQL: &str = r"
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
  au.last_used_at
from account_usage au
left join accounts a on a.id = au.account_id
order by au.last_used_at desc, au.account_id desc
limit ?";

const USAGE_SUMMARY_SQL: &str = r"
select
  count(*) as account_count,
  coalesce(sum(request_count), 0) as request_count,
  coalesce(sum(empty_response_count), 0) as empty_response_count,
  coalesce(sum(input_tokens), 0) as input_tokens,
  coalesce(sum(output_tokens), 0) as output_tokens,
  coalesce(sum(cached_tokens), 0) as cached_tokens,
  coalesce(sum(reasoning_tokens), 0) as reasoning_tokens,
  coalesce(sum(total_tokens), 0) as total_tokens,
  coalesce(sum(image_input_tokens), 0) as image_input_tokens,
  coalesce(sum(image_output_tokens), 0) as image_output_tokens,
  coalesce(sum(image_request_count), 0) as image_request_count,
  coalesce(sum(image_request_failed_count), 0) as image_request_failed_count
from account_usage";

const RESET_USAGE_SQL: &str = r"
update account_usage
set
  request_count = 0,
  empty_response_count = 0,
  input_tokens = 0,
  output_tokens = 0,
  cached_tokens = 0,
  reasoning_tokens = 0,
  total_tokens = 0,
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
  last_used_at = null
where account_id = ?";

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
  access_token_cipher = ?,
  refresh_token_cipher = ?,
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
  access_token_cipher = ?,
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
  access_token_cipher = ?,
  refresh_token_cipher = ?,
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
  access_token_cipher = ?,
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

const DELETE_ALL_ACCOUNTS_SQL: &str = "delete from accounts";

const DELETE_ACCOUNT_SQL: &str = "delete from accounts where id = ?";

// ============================================================================
// Error types
// ============================================================================

/// SQLite 账号仓储错误。
#[derive(Debug, Error)]
pub enum SqliteAccountStoreError {
    /// 数据库错误。
    #[error("sqlite account store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// 加解密错误。
    #[error("sqlite account store crypto error: {0}")]
    Crypto(#[from] CryptoError),
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

/// Account usage summary.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AccountUsageSummary {
    pub account_count: i64,
    pub request_count: i64,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub image_input_tokens: i64,
    pub image_output_tokens: i64,
    pub image_request_count: i64,
    pub image_request_failed_count: i64,
}

// ============================================================================
// Data types
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
    /// access token 密文。
    pub access_token: SecretString,
    /// refresh token 密文。
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

/// 账号用量记录。
#[derive(Debug, Clone)]
pub struct AccountUsageRecord {
    /// 账号 ID。
    pub account_id: String,
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
    /// 当前窗口请求数。
    pub window_request_count: i64,
    /// 当前窗口输入 token。
    pub window_input_tokens: i64,
    /// 当前窗口输出 token。
    pub window_output_tokens: i64,
    /// 当前窗口缓存 token。
    pub window_cached_tokens: i64,
    /// 当前窗口图片输入 token。
    pub window_image_input_tokens: i64,
    /// 当前窗口图片输出 token。
    pub window_image_output_tokens: i64,
    /// 当前窗口图片请求数。
    pub window_image_request_count: i64,
    /// 当前窗口图片请求失败数。
    pub window_image_request_failed_count: i64,
    /// 当前窗口起始时间。
    pub window_started_at: Option<DateTime<Utc>>,
    /// 当前窗口重置时间。
    pub window_reset_at: Option<DateTime<Utc>>,
    /// 限流窗口大小。
    pub limit_window_seconds: Option<u64>,
    /// 最近使用时间。
    pub last_used_at: Option<DateTime<Utc>>,
}

/// 账号用量列表记录（不含窗口用量）。
#[derive(Debug, Clone)]
pub struct AccountUsageListRecord {
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
    /// 最近使用时间。
    pub last_used_at: Option<DateTime<Utc>>,
}

/// 账号用量汇总。
#[derive(Debug, Clone)]
pub struct UsageSummary {
    /// 账号数。
    pub account_count: i64,
    /// 总请求数。
    pub request_count: i64,
    /// 总空响应数。
    pub empty_response_count: i64,
    /// 总输入 token。
    pub input_tokens: i64,
    /// 总输出 token。
    pub output_tokens: i64,
    /// 总缓存 token。
    pub cached_tokens: i64,
    /// 总 reasoning token。
    pub reasoning_tokens: i64,
    /// 总 token。
    pub total_tokens: i64,
    /// 总图片输入 token。
    pub image_input_tokens: i64,
    /// 总图片输出 token。
    pub image_output_tokens: i64,
    /// 总图片请求成功数。
    pub image_request_count: i64,
    /// 总图片请求失败数。
    pub image_request_failed_count: i64,
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

    /// 读取账号当前配额 JSON。
    async fn get_quota_json(&self, _account_id: &str) -> AccountStoreResult<Option<String>> {
        Ok(None)
    }

    /// 更新账号当前配额 JSON。
    async fn update_quota_json(
        &self,
        _account_id: &str,
        _quota_json: &str,
    ) -> AccountStoreResult<bool> {
        Ok(false)
    }

    /// 应用已经验证过的账号配额快照。
    async fn apply_quota_snapshot(
        &self,
        account_id: &str,
        quota_json: &str,
        limit_reached: bool,
        cooldown_until: Option<DateTime<Utc>>,
    ) -> AccountStoreResult<bool> {
        let _ = (limit_reached, cooldown_until);
        self.update_quota_json(account_id, quota_json).await
    }

    /// 同步账号当前 rate-limit 统计窗口。
    async fn sync_rate_limit_window(
        &self,
        _account_id: &str,
        _reset_at: DateTime<Utc>,
        _limit_window_seconds: Option<u64>,
    ) -> AccountStoreResult<()> {
        Ok(())
    }

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

// ============================================================================
// SqliteCookieStore
// ============================================================================

const DEFAULT_COOKIE_DOMAIN: &str = "chatgpt.com";
const CAPTURABLE_COOKIES: &[&str] = &["cf_clearance"];

/// SQLite Cookie 存储错误。
#[derive(Debug, Error)]
pub enum SqliteCookieStoreError {
    /// 数据库错误。
    #[error("sqlite cookie store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// 加解密错误。
    #[error("sqlite cookie store crypto error: {0}")]
    Crypto(#[from] crate::infra::crypto::CryptoError),
}

/// SQLite Cookie 存储结果。
pub type SqliteCookieStoreResult<T> = Result<T, SqliteCookieStoreError>;

/// Cookie store implementation.  
#[derive(Clone)]
pub struct SqliteCookieStore {
    pool: sqlx::SqlitePool,
    secret: crate::infra::crypto::SecretBox,
}

impl SqliteCookieStore {
    /// Create a new cookie store.
    pub fn new(pool: sqlx::SqlitePool, secret: crate::infra::crypto::SecretBox) -> Self {
        Self { pool, secret }
    }

    /// 返回底层连接池。
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// 检查账号是否存在。
    pub async fn account_exists(&self, account_id: &str) -> SqliteCookieStoreResult<bool> {
        let row = sqlx::query("select 1 from accounts where id = ?")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// 捕获上游 `Set-Cookie` 响应头中允许持久化的 Cookie。
    pub async fn capture_set_cookie(
        &self,
        account_id: &str,
        raw: &str,
    ) -> SqliteCookieStoreResult<()> {
        let Some(parsed) = parse_set_cookie(raw) else {
            return Ok(());
        };
        if !CAPTURABLE_COOKIES.contains(&parsed.name.as_str()) {
            return Ok(());
        }
        self.upsert_cookie(account_id, parsed).await
    }

    /// 将 Cookie 请求头写入账号 Cookie 存储。
    pub async fn set_cookie_header(
        &self,
        account_id: &str,
        raw: &str,
    ) -> SqliteCookieStoreResult<usize> {
        let parsed = parse_cookie_header(raw);
        let count = parsed.len();
        for cookie in parsed {
            self.upsert_cookie(account_id, cookie).await?;
        }
        Ok(count)
    }

    /// 为请求域名读取账号 Cookie 请求头。
    pub async fn cookie_header(
        &self,
        account_id: &str,
        request_domain: &str,
    ) -> SqliteCookieStoreResult<Option<String>> {
        self.cookie_header_for_request(account_id, request_domain, "/")
            .await
    }

    /// 为请求域名和路径读取账号 Cookie 请求头。
    pub async fn cookie_header_for_request(
        &self,
        account_id: &str,
        request_domain: &str,
        request_path: &str,
    ) -> SqliteCookieStoreResult<Option<String>> {
        let rows = sqlx::query(
            "select domain, name, value_cipher, path, expires_at from account_cookies where account_id = ?",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;
        let mut pairs = Vec::new();
        let now = Utc::now();
        for row in rows {
            let domain = row.get::<String, _>("domain");
            if !domain_matches(request_domain, &domain) {
                continue;
            }
            let path = row.get::<String, _>("path");
            if !path_matches(request_path, &path) {
                continue;
            }
            let expires_at = row.get::<Option<String>, _>("expires_at");
            if cookie_is_expired(expires_at.as_deref(), now) {
                continue;
            }
            let name = row.get::<String, _>("name");
            let value_cipher = row.get::<String, _>("value_cipher");
            let value = self.secret.decrypt(&value_cipher)?;
            pairs.push(CookieHeaderPair {
                path_len: path.len(),
                name: name.clone(),
                value: format!("{name}={}", value.expose_secret()),
            });
        }
        if pairs.is_empty() {
            Ok(None)
        } else {
            pairs.sort_by(|left, right| {
                right
                    .path_len
                    .cmp(&left.path_len)
                    .then_with(|| left.name.cmp(&right.name))
            });
            Ok(Some(
                pairs
                    .into_iter()
                    .map(|pair| pair.value)
                    .collect::<Vec<_>>()
                    .join("; "),
            ))
        }
    }

    /// 删除账号全部 Cookie。
    pub async fn delete_account_cookies(&self, account_id: &str) -> SqliteCookieStoreResult<u64> {
        let result = sqlx::query("delete from account_cookies where account_id = ?")
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 删除指定时间之前过期的 Cookie。
    pub async fn cleanup_expired(&self, now: DateTime<Utc>) -> SqliteCookieStoreResult<u64> {
        let result = sqlx::query(
            "delete from account_cookies where expires_at is not null and expires_at <= ?",
        )
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    async fn upsert_cookie(
        &self,
        account_id: &str,
        parsed: ParsedCookie,
    ) -> SqliteCookieStoreResult<()> {
        let now = Utc::now().to_rfc3339();
        let value_cipher = self
            .secret
            .encrypt(&SecretString::new(parsed.value.into()))?;
        sqlx::query(
            "insert into account_cookies (id, account_id, domain, name, value_cipher, path, expires_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?) on conflict(account_id, domain, name, path) do update set value_cipher = excluded.value_cipher, expires_at = excluded.expires_at, updated_at = excluded.updated_at",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(account_id)
        .bind(parsed.domain)
        .bind(parsed.name)
        .bind(value_cipher)
        .bind(parsed.path)
        .bind(parsed.expires_at)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedCookie {
    domain: String,
    name: String,
    value: String,
    path: String,
    expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CookieHeaderPair {
    path_len: usize,
    name: String,
    value: String,
}

fn parse_set_cookie(raw: &str) -> Option<ParsedCookie> {
    let mut parts = raw.split(';').map(str::trim);
    let (name, value) = parts.next()?.split_once('=')?;
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() || value.is_empty() {
        return None;
    }

    let mut domain = DEFAULT_COOKIE_DOMAIN.to_string();
    let mut path = "/".to_string();
    let mut expires_at = None;
    for part in parts {
        let Some((attribute, value)) = part.split_once('=') else {
            continue;
        };
        match attribute.trim().to_ascii_lowercase().as_str() {
            "domain" => domain = value.trim().trim_start_matches('.').to_string(),
            "path" => path = normalize_cookie_path(value),
            "max-age" => {
                if let Ok(seconds) = value.trim().parse::<i64>() {
                    expires_at = Some(max_age_expires_at(seconds));
                }
                break;
            }
            "expires" => expires_at = Some(value.trim().to_string()),
            _ => {}
        }
    }

    Some(ParsedCookie {
        domain,
        name: name.to_string(),
        value: value.to_string(),
        path,
        expires_at,
    })
}

fn max_age_expires_at(seconds: i64) -> String {
    let now = Utc::now();
    if seconds <= 0 {
        return (now - Duration::seconds(1)).to_rfc3339();
    }
    (now + Duration::seconds(seconds.min(i32::MAX as i64))).to_rfc3339()
}

fn parse_cookie_header(raw: &str) -> Vec<ParsedCookie> {
    raw.split(';')
        .map(str::trim)
        .filter_map(|part| {
            let (name, value) = part.split_once('=')?;
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() || value.is_empty() {
                return None;
            }
            Some(ParsedCookie {
                domain: DEFAULT_COOKIE_DOMAIN.to_string(),
                name: name.to_string(),
                value: value.to_string(),
                path: "/".to_string(),
                expires_at: None,
            })
        })
        .collect()
}

fn domain_matches(request_domain: &str, cookie_domain: &str) -> bool {
    request_domain == cookie_domain
        || request_domain
            .strip_suffix(cookie_domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    let request_path = normalize_request_path(request_path);
    let cookie_path = normalize_cookie_path(cookie_path);
    request_path == cookie_path
        || (request_path.starts_with(&cookie_path)
            && (cookie_path.ends_with('/')
                || request_path
                    .as_bytes()
                    .get(cookie_path.len())
                    .is_some_and(|byte| *byte == b'/')))
}

fn normalize_request_path(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn normalize_cookie_path(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.to_string()
    } else {
        "/".to_string()
    }
}

fn cookie_is_expired(expires_at: Option<&str>, now: DateTime<Utc>) -> bool {
    expires_at
        .and_then(parse_cookie_expires_at)
        .is_some_and(|expires_at| expires_at <= now)
}

fn parse_cookie_expires_at(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(value)
        .or_else(|_| DateTime::parse_from_rfc3339(value))
        .map(|expires_at| expires_at.with_timezone(&Utc))
        .ok()
}

#[derive(Clone)]
pub struct SqliteAccountStore {
    pool: SqlitePool,
    secret_box: SecretBox,
}

impl SqliteAccountStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool, secret_box: SecretBox) -> Self {
        Self { pool, secret_box }
    }

    /// 返回底层连接池。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 插入新账号。
    pub async fn insert(&self, account: NewAccount) -> SqliteAccountStoreResult<()> {
        let now = Utc::now().to_rfc3339();
        let access_token_cipher = self.secret_box.encrypt(&account.access_token)?;
        let refresh_token_cipher = account
            .refresh_token
            .map(|token| self.secret_box.encrypt(&token))
            .transpose()?;
        sqlx::query(INSERT_ACCOUNT_SQL)
            .bind(&account.id)
            .bind(&account.email)
            .bind(&account.account_id)
            .bind(&account.user_id)
            .bind(&account.label)
            .bind(&account.plan_type)
            .bind(&access_token_cipher)
            .bind(&refresh_token_cipher)
            .bind(account.access_token_expires_at.map(|dt| dt.to_rfc3339()))
            .bind(status_to_db(account.status))
            .bind(
                account
                    .added_at
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
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
        row.as_ref()
            .map(|row| stored_account_from_row(self, row))
            .transpose()
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
        row.as_ref()
            .map(|row| stored_account_from_row(self, row))
            .transpose()
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
                rows,
                limit,
                |row| stored_account_from_row(self, row),
                ("added_at", "id"),
            ))
        } else {
            let rows = sqlx::query(LIST_STORED_ACCOUNTS_SQL)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(
                rows,
                limit,
                |row| stored_account_from_row(self, row),
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
            Ok(to_page(rows, limit, metadata_from_row, ("added_at", "id")))
        } else {
            let rows = sqlx::query(LIST_ACCOUNT_METADATA_SQL)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(rows, limit, metadata_from_row, ("added_at", "id")))
        }
    }

    /// 更新账号 claims（含 refresh token）。
    pub async fn update_from_claims(
        &self,
        account_id: &str,
        update: AccountClaimsUpdate,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let access_token_cipher = self.secret_box.encrypt(&update.access_token)?;
        let refresh_token_cipher = update
            .refresh_token
            .map(|token| self.secret_box.encrypt(&token))
            .transpose()?;

        let result = if let Some(refresh_cipher) = &refresh_token_cipher {
            sqlx::query(UPDATE_ACCOUNT_CLAIMS_WITH_REFRESH_SQL)
                .bind(&update.email)
                .bind(&update.account_id)
                .bind(&update.user_id)
                .bind(&update.plan_type)
                .bind(&access_token_cipher)
                .bind(refresh_cipher)
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
                .bind(&access_token_cipher)
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
        let access_token_cipher = self.secret_box.encrypt(&update.account.access_token)?;
        let refresh_token_cipher = update
            .account
            .refresh_token
            .map(|token| self.secret_box.encrypt(&token))
            .transpose()?;
        let quota_json = update.quota_json;
        let quota_fetched_at = update.quota_fetched_at.map(|dt| dt.to_rfc3339());
        let quota_verify_required = if update.quota_verify_required { 1 } else { 0 };

        let result = if let Some(refresh_cipher) = &refresh_token_cipher {
            sqlx::query(UPDATE_IMPORTED_ACCOUNT_WITH_REFRESH_SQL)
                .bind(&update.account.email)
                .bind(&update.account.account_id)
                .bind(&update.account.user_id)
                .bind(&update.account.label)
                .bind(&update.account.plan_type)
                .bind(&access_token_cipher)
                .bind(refresh_cipher)
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
                .bind(&access_token_cipher)
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

    /// 获取用量记录。
    pub async fn get_usage(
        &self,
        account_id: &str,
    ) -> SqliteAccountStoreResult<Option<AccountUsageRecord>> {
        let row = sqlx::query(GET_USAGE_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(usage_from_row).transpose()
    }

    /// 分页列出用量。
    pub async fn list_usage(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteAccountStoreResult<Page<AccountUsageListRecord>> {
        let limit = limit.clamp(1, 200);
        if let Some(cursor) = cursor {
            let (last_used_at, account_id) =
                decode_cursor(&cursor).ok_or(SqliteAccountStoreError::InvalidCursor)?;
            let rows = sqlx::query(LIST_USAGE_AFTER_CURSOR_SQL)
                .bind(&last_used_at)
                .bind(&last_used_at)
                .bind(&account_id)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(
                rows,
                limit,
                usage_list_from_row,
                ("last_used_at", "account_id"),
            ))
        } else {
            let rows = sqlx::query(LIST_USAGE_SQL)
                .bind(limit + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(
                rows,
                limit,
                usage_list_from_row,
                ("last_used_at", "account_id"),
            ))
        }
    }

    /// 用量汇总。
    pub async fn usage_summary(&self) -> SqliteAccountStoreResult<UsageSummary> {
        let row = sqlx::query(USAGE_SUMMARY_SQL).fetch_one(&self.pool).await?;
        Ok(UsageSummary {
            account_count: row.get("account_count"),
            request_count: row.get("request_count"),
            empty_response_count: row.get("empty_response_count"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cached_tokens: row.get("cached_tokens"),
            reasoning_tokens: row.get("reasoning_tokens"),
            total_tokens: row.get("total_tokens"),
            image_input_tokens: row.get("image_input_tokens"),
            image_output_tokens: row.get("image_output_tokens"),
            image_request_count: row.get("image_request_count"),
            image_request_failed_count: row.get("image_request_failed_count"),
        })
    }

    /// 重置用量。
    pub async fn reset_usage(&self, account_id: &str) -> SqliteAccountStoreResult<bool> {
        let result = sqlx::query(RESET_USAGE_SQL)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
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
                let existing_reset_at =
                    parse_optional_rfc3339(row.get::<Option<String>, _>("window_reset_at"))
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

    /// 删除全部账号。
    pub async fn delete_all(&self) -> SqliteAccountStoreResult<u64> {
        let result = sqlx::query(DELETE_ALL_ACCOUNTS_SQL)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
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
    /// Set account label.
    pub async fn set_label(
        &self,
        account_id: &str,
        label: Option<String>,
    ) -> Result<bool, SqliteAccountStoreError> {
        sqlx::query("UPDATE accounts SET label = ?1, updated_at = datetime('now') WHERE id = ?2")
            .bind(&label)
            .bind(account_id)
            .execute(&self.pool)
            .await
            .map_err(SqliteAccountStoreError::Database)?;
        Ok(true)
    }

    /// Delete an account by ID.
    pub async fn delete_account(&self, account_id: &str) -> Result<bool, SqliteAccountStoreError> {
        let result = sqlx::query("DELETE FROM accounts WHERE id = ?1")
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
            .map_err(map_account_store_error)
    }

    async fn get_pool_account(&self, account_id: &str) -> AccountStoreResult<Option<Account>> {
        get_pool_account(self, account_id)
            .await
            .map_err(map_account_store_error)
    }

    async fn mark_quota_limited_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        SqliteAccountStore::mark_quota_limited_until(self, account_id, cooldown_until)
            .await
            .map_err(map_account_store_error)
    }

    async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        SqliteAccountStore::set_cloudflare_cooldown_until(self, account_id, cooldown_until)
            .await
            .map_err(map_account_store_error)
    }

    async fn set_status(
        &self,
        account_id: &str,
        status: AccountStatus,
    ) -> AccountStoreResult<bool> {
        SqliteAccountStore::set_status(self, account_id, status)
            .await
            .map_err(map_account_store_error)
    }

    async fn record_usage_delta(
        &self,
        account_id: &str,
        usage: AccountUsageDelta,
    ) -> AccountStoreResult<()> {
        self.record_usage(account_id, sqlite_usage_delta(usage))
            .await
            .map_err(map_account_store_error)
    }

    async fn get_quota_json(&self, account_id: &str) -> AccountStoreResult<Option<String>> {
        SqliteAccountStore::get_quota_json(self, account_id)
            .await
            .map_err(map_account_store_error)
    }

    async fn update_quota_json(
        &self,
        account_id: &str,
        quota_json: &str,
    ) -> AccountStoreResult<bool> {
        SqliteAccountStore::update_quota_json(self, account_id, quota_json)
            .await
            .map_err(map_account_store_error)
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
        .map_err(map_account_store_error)
    }

    async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> AccountStoreResult<()> {
        SqliteAccountStore::sync_rate_limit_window(self, account_id, reset_at, limit_window_seconds)
            .await
            .map_err(map_account_store_error)
    }
}

// ============================================================================
// Private helpers
// ============================================================================

async fn list_pool_accounts(store: &SqliteAccountStore) -> SqliteAccountStoreResult<Vec<Account>> {
    let rows = sqlx::query(LIST_POOL_ACCOUNTS_SQL)
        .fetch_all(&store.pool)
        .await?;
    let mut accounts = Vec::with_capacity(rows.len());

    for row in rows {
        accounts.push(pool_account_from_row(store, &row)?);
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
    row.map(|row| pool_account_from_row(store, &row))
        .transpose()
}

fn pool_account_from_row(
    store: &SqliteAccountStore,
    row: &SqliteRow,
) -> SqliteAccountStoreResult<Account> {
    let access_token_cipher = row.get::<String, _>("access_token_cipher");
    let access_token = store.secret_box.decrypt(&access_token_cipher)?;
    let refresh_token = row
        .get::<Option<String>, _>("refresh_token_cipher")
        .map(|cipher| store.secret_box.decrypt(&cipher))
        .transpose()?
        .map(|token| token.expose_secret().to_string());

    Ok(Account {
        id: row.get("id"),
        email: row.get("email"),
        account_id: row.get("account_id"),
        user_id: row.get("user_id"),
        label: row.get("label"),
        plan_type: row.get("plan_type"),
        access_token: access_token.expose_secret().to_string(),
        refresh_token,
        access_token_expires_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("access_token_expires_at"),
        )?,
        next_refresh_at: parse_optional_rfc3339(row.get::<Option<String>, _>("next_refresh_at"))?,
        status: status_from_db(&row.get::<String, _>("status"))?,
        quota_limit_reached: row.get::<i64, _>("quota_limit_reached") != 0,
        quota_verify_required: row.get::<i64, _>("quota_verify_required") != 0,
        quota_cooldown_until: parse_optional_rfc3339(
            row.get::<Option<String>, _>("quota_cooldown_until"),
        )?,
        cloudflare_cooldown_until: parse_optional_rfc3339(
            row.get::<Option<String>, _>("cloudflare_cooldown_until"),
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
            row.get::<Option<String>, _>("usage_window_started_at"),
        )?,
        window_reset_at: parse_optional_rfc3339(
            row.get::<Option<String>, _>("usage_window_reset_at"),
        )?,
        limit_window_seconds: optional_positive_i64_to_u64(
            row.get::<Option<i64>, _>("usage_limit_window_seconds"),
        ),
        added_at: row.get("added_at"),
        last_used_at: row.get("usage_last_used_at"),
    })
}

fn stored_account_from_row(
    store: &SqliteAccountStore,
    row: &SqliteRow,
) -> SqliteAccountStoreResult<StoredAccount> {
    let access_token_cipher = row.get::<String, _>("access_token_cipher");
    let access_token = store.secret_box.decrypt(&access_token_cipher)?;
    let refresh_token = row
        .get::<Option<String>, _>("refresh_token_cipher")
        .map(|cipher| store.secret_box.decrypt(&cipher))
        .transpose()?;

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
        next_refresh_at: parse_optional_rfc3339(row.get::<Option<String>, _>("next_refresh_at"))?,
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
            row.get::<Option<String>, _>("access_token_expires_at"),
        )?,
        status: status_from_db(&row.get::<String, _>("status"))?,
        added_at: row.get("added_at"),
        updated_at: row.get("updated_at"),
    })
}

fn map_account_store_error(error: impl ToString) -> AccountStoreError {
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

fn usage_from_row(row: &sqlx::sqlite::SqliteRow) -> SqliteAccountStoreResult<AccountUsageRecord> {
    Ok(AccountUsageRecord {
        account_id: row.get("account_id"),
        request_count: row.get("request_count"),
        empty_response_count: row.get("empty_response_count"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        reasoning_tokens: row.get("reasoning_tokens"),
        total_tokens: row.get("total_tokens"),
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
        limit_window_seconds: optional_positive_i64_to_u64(
            row.get::<Option<i64>, _>("limit_window_seconds"),
        ),
        last_used_at: parse_optional_rfc3339(row.get::<Option<String>, _>("last_used_at"))?,
    })
}

fn usage_list_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> SqliteAccountStoreResult<AccountUsageListRecord> {
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
        reasoning_tokens: row.get("reasoning_tokens"),
        total_tokens: row.get("total_tokens"),
        image_input_tokens: row.get("image_input_tokens"),
        image_output_tokens: row.get("image_output_tokens"),
        image_request_count: row.get("image_request_count"),
        image_request_failed_count: row.get("image_request_failed_count"),
        last_used_at: parse_optional_rfc3339(row.get::<Option<String>, _>("last_used_at"))?,
    })
}

fn quota_snapshot_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> SqliteAccountStoreResult<AccountQuotaSnapshot> {
    Ok(AccountQuotaSnapshot {
        account_id: row.get("id"),
        email: row.get("email"),
        quota_json: row.get("quota_json"),
        quota_fetched_at: parse_optional_rfc3339(row.get::<Option<String>, _>("quota_fetched_at"))?,
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

fn parse_optional_rfc3339(
    value: Option<String>,
) -> SqliteAccountStoreResult<Option<DateTime<Utc>>> {
    value.as_deref().map(parse_rfc3339).transpose()
}

fn parse_rfc3339(value: &str) -> SqliteAccountStoreResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn nonnegative_i64_to_u64(value: i64) -> u64 {
    value.max(0) as u64
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn optional_positive_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn to_page<T>(
    rows: Vec<SqliteRow>,
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
