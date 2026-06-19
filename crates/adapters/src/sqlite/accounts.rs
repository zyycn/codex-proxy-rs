//! SQLite 账号仓储适配器。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use thiserror::Error;

use codex_proxy_core::accounts::{
    model::{Account, AccountStatus},
    ports::{AccountStore, AccountStoreError, AccountStoreResult},
    usage::AccountUsageDelta,
};
use codex_proxy_platform::{
    crypto::{CryptoError, SecretBox},
    json::{decode_cursor, encode_cursor, Page},
};

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

const LIST_ACCOUNT_METADATA_AFTER_CURSOR_SQL: &str = r"
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

const LIST_ACCOUNT_METADATA_SQL: &str = r"
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

const LIST_POOL_ACCOUNTS_SQL: &str = r"
select
  a.id,
  a.email,
  a.account_id,
  a.user_id,
  a.label,
  a.plan_type,
  a.access_token_cipher,
  a.refresh_token_cipher,
  a.access_token_expires_at,
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
  a.account_id,
  a.user_id,
  a.label,
  a.plan_type,
  a.access_token_cipher,
  a.refresh_token_cipher,
  a.access_token_expires_at,
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
) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
on conflict(account_id) do update set
  request_count = request_count + excluded.request_count,
  empty_response_count = empty_response_count + excluded.empty_response_count,
  input_tokens = input_tokens + excluded.input_tokens,
  output_tokens = output_tokens + excluded.output_tokens,
  cached_tokens = cached_tokens + excluded.cached_tokens,
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
  account_id = ?,
  user_id = ?,
  plan_type = ?,
  access_token_cipher = ?,
  refresh_token_cipher = ?,
  access_token_expires_at = ?,
  status = ?,
  updated_at = ?
where id = ?";

const UPDATE_ACCOUNT_CLAIMS_PRESERVING_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  account_id = ?,
  user_id = ?,
  plan_type = ?,
  access_token_cipher = ?,
  access_token_expires_at = ?,
  status = ?,
  updated_at = ?
where id = ?";

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
    /// 新状态。
    pub status: AccountStatus,
}

/// 已存储账号数据。
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
    /// 账号状态。
    pub status: AccountStatus,
    /// 创建时间。
    pub added_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 已存储账号元数据，不包含任何密文或明文 token。
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// 创建时间。
    pub added_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 账号用量增量。
#[derive(Debug, Clone, Copy)]
pub struct UsageDelta {
    /// 请求数增量。
    pub request_count: i64,
    /// 输入 token 增量。
    pub input_tokens: i64,
    /// 输出 token 增量。
    pub output_tokens: i64,
    /// 缓存 token 增量。
    pub cached_tokens: i64,
    /// 图片输入 token 增量。
    pub image_input_tokens: i64,
    /// 图片输出 token 增量。
    pub image_output_tokens: i64,
    /// 图片请求数增量。
    pub image_request_count: i64,
    /// 图片请求失败数增量。
    pub image_request_failed_count: i64,
    /// 空响应数增量。
    pub empty_response_count: i64,
}

impl Default for UsageDelta {
    fn default() -> Self {
        Self {
            request_count: 1,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            image_request_count: 0,
            image_request_failed_count: 0,
            empty_response_count: 0,
        }
    }
}

/// 账号用量记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageRecord {
    /// 账号 ID。
    pub account_id: String,
    /// 请求数。
    pub request_count: i64,
    /// 空响应数。
    pub empty_response_count: i64,
    /// 输入 token 数。
    pub input_tokens: i64,
    /// 输出 token 数。
    pub output_tokens: i64,
    /// 缓存 token 数。
    pub cached_tokens: i64,
    /// 图片输入 token 数。
    pub image_input_tokens: i64,
    /// 图片输出 token 数。
    pub image_output_tokens: i64,
    /// 图片请求数。
    pub image_request_count: i64,
    /// 图片请求失败数。
    pub image_request_failed_count: i64,
    /// 当前窗口请求数。
    pub window_request_count: i64,
    /// 当前窗口输入 token 数。
    pub window_input_tokens: i64,
    /// 当前窗口输出 token 数。
    pub window_output_tokens: i64,
    /// 当前窗口缓存 token 数。
    pub window_cached_tokens: i64,
    /// 当前窗口图片输入 token 数。
    pub window_image_input_tokens: i64,
    /// 当前窗口图片输出 token 数。
    pub window_image_output_tokens: i64,
    /// 当前窗口图片请求数。
    pub window_image_request_count: i64,
    /// 当前窗口图片请求失败数。
    pub window_image_request_failed_count: i64,
    /// 当前窗口起始时间。
    pub window_started_at: Option<DateTime<Utc>>,
    /// 当前窗口重置时间。
    pub window_reset_at: Option<DateTime<Utc>>,
    /// 限流窗口秒数。
    pub limit_window_seconds: Option<u64>,
    /// 最近使用时间。
    pub last_used_at: Option<DateTime<Utc>>,
}

/// 管理端账号用量列表记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageListRecord {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// 请求数。
    pub request_count: i64,
    /// 空响应数。
    pub empty_response_count: i64,
    /// 输入 token 数。
    pub input_tokens: i64,
    /// 输出 token 数。
    pub output_tokens: i64,
    /// 缓存 token 数。
    pub cached_tokens: i64,
    /// 图片输入 token 数。
    pub image_input_tokens: i64,
    /// 图片输出 token 数。
    pub image_output_tokens: i64,
    /// 图片请求数。
    pub image_request_count: i64,
    /// 图片请求失败数。
    pub image_request_failed_count: i64,
    /// 最近使用时间。
    pub last_used_at: Option<DateTime<Utc>>,
}

/// 管理端账号用量汇总。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountUsageSummary {
    /// 有用量记录的账号数。
    pub account_count: i64,
    /// 请求总数。
    pub request_count: i64,
    /// 空响应总数。
    pub empty_response_count: i64,
    /// 输入 token 总数。
    pub input_tokens: i64,
    /// 输出 token 总数。
    pub output_tokens: i64,
    /// 缓存 token 总数。
    pub cached_tokens: i64,
    /// 图片输入 token 总数。
    pub image_input_tokens: i64,
    /// 图片输出 token 总数。
    pub image_output_tokens: i64,
    /// 图片请求总数。
    pub image_request_count: i64,
    /// 图片请求失败总数。
    pub image_request_failed_count: i64,
}

/// 账号配额快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountQuotaSnapshot {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 配额 JSON。
    pub quota_json: String,
    /// 配额拉取时间。
    pub quota_fetched_at: Option<DateTime<Utc>>,
}

/// SQLite 账号仓储。
#[derive(Clone)]
pub struct SqliteAccountStore {
    pool: SqlitePool,
    secret_box: SecretBox,
}

#[derive(Debug)]
struct TokenWrite {
    access_token_cipher: String,
    refresh_token_cipher: Option<String>,
    access_token_expires_at: Option<String>,
    updated_at: String,
}

impl SqliteAccountStore {
    /// 构造适配器。
    pub fn new(pool: SqlitePool, secret_box: SecretBox) -> Self {
        Self { pool, secret_box }
    }

    /// 插入账号。
    pub async fn insert(&self, account: NewAccount) -> SqliteAccountStoreResult<()> {
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

    /// 按 ID 读取账号。
    pub async fn get(&self, id: &str) -> SqliteAccountStoreResult<Option<StoredAccount>> {
        let row = sqlx::query(SELECT_STORED_ACCOUNT_BY_ID_SQL)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| self.account_from_row(&row)).transpose()
    }

    /// 按 ChatGPT 账号身份读取账号。
    pub async fn find_by_chatgpt_identity(
        &self,
        account_id: &str,
        user_id: Option<&str>,
    ) -> SqliteAccountStoreResult<Option<StoredAccount>> {
        let row = sqlx::query(SELECT_STORED_ACCOUNT_BY_CHATGPT_IDENTITY_SQL)
            .bind(account_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| self.account_from_row(&row)).transpose()
    }

    /// 用 JWT claims 更新账号，并在缺少新 refresh token 时保留旧值。
    pub async fn update_from_claims(
        &self,
        id: &str,
        update: AccountClaimsUpdate,
    ) -> SqliteAccountStoreResult<bool> {
        let TokenWrite {
            access_token_cipher,
            refresh_token_cipher,
            access_token_expires_at,
            updated_at,
        } = self.prepare_token_write(
            &update.access_token,
            update.refresh_token.as_ref(),
            update.access_token_expires_at,
        )?;
        let status = status_to_db(update.status);

        let result = if let Some(refresh_token_cipher) = refresh_token_cipher {
            sqlx::query(UPDATE_ACCOUNT_CLAIMS_WITH_REFRESH_SQL)
                .bind(update.email)
                .bind(update.account_id)
                .bind(update.user_id)
                .bind(update.plan_type)
                .bind(access_token_cipher)
                .bind(refresh_token_cipher)
                .bind(access_token_expires_at)
                .bind(status)
                .bind(updated_at)
                .bind(id)
                .execute(&self.pool)
                .await?
        } else {
            sqlx::query(UPDATE_ACCOUNT_CLAIMS_PRESERVING_REFRESH_SQL)
                .bind(update.email)
                .bind(update.account_id)
                .bind(update.user_id)
                .bind(update.plan_type)
                .bind(access_token_cipher)
                .bind(access_token_expires_at)
                .bind(status)
                .bind(updated_at)
                .bind(id)
                .execute(&self.pool)
                .await?
        };
        Ok(result.rows_affected() > 0)
    }

    /// 按创建时间倒序分页列出账号。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteAccountStoreResult<Page<StoredAccount>> {
        let fetch_limit = i64::from(limit) + 1;
        let rows = if let Some(cursor) = cursor {
            let (created_at, id) =
                decode_cursor(&cursor).ok_or(SqliteAccountStoreError::InvalidCursor)?;
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

    /// 按创建时间倒序分页列出账号元数据，不解密 token。
    pub async fn list_metadata(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteAccountStoreResult<Page<StoredAccountMetadata>> {
        let fetch_limit = i64::from(limit) + 1;
        let rows = if let Some(cursor) = cursor {
            let (created_at, id) =
                decode_cursor(&cursor).ok_or(SqliteAccountStoreError::InvalidCursor)?;
            sqlx::query(LIST_ACCOUNT_METADATA_AFTER_CURSOR_SQL)
                .bind(&created_at)
                .bind(created_at)
                .bind(id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query(LIST_ACCOUNT_METADATA_SQL)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        };

        let has_next = rows.len() > limit as usize;
        let take_count = rows.len().min(limit as usize);
        let items = rows
            .into_iter()
            .take(take_count)
            .map(|row| account_metadata_from_row(&row))
            .collect::<SqliteAccountStoreResult<Vec<_>>>()?;
        let next_cursor = if has_next {
            items
                .last()
                .map(|account| encode_cursor(&account.added_at.to_rfc3339(), &account.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    /// 更新账号状态。
    pub async fn set_status(
        &self,
        id: &str,
        status: AccountStatus,
    ) -> SqliteAccountStoreResult<bool> {
        let result = sqlx::query("update accounts set status = ?, updated_at = ? where id = ?")
            .bind(status_to_db(status))
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 更新账号标签。
    pub async fn set_label(
        &self,
        id: &str,
        label: Option<String>,
    ) -> SqliteAccountStoreResult<bool> {
        let result = sqlx::query("update accounts set label = ?, updated_at = ? where id = ?")
            .bind(label)
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 删除账号。
    pub async fn delete(&self, id: &str) -> SqliteAccountStoreResult<bool> {
        let result = sqlx::query("delete from accounts where id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 删除全部账号及账号关联状态。
    pub async fn delete_all(&self) -> SqliteAccountStoreResult<u64> {
        sqlx::query("delete from account_cookies")
            .execute(&self.pool)
            .await?;
        sqlx::query("delete from account_usage")
            .execute(&self.pool)
            .await?;
        sqlx::query("delete from account_refresh_leases")
            .execute(&self.pool)
            .await?;
        let result = sqlx::query("delete from accounts")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 记录账号用量。
    pub async fn record_usage(
        &self,
        account_id: &str,
        usage: UsageDelta,
    ) -> SqliteAccountStoreResult<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(RECORD_USAGE_SQL)
            .bind(account_id)
            .bind(usage.request_count)
            .bind(usage.empty_response_count)
            .bind(usage.input_tokens)
            .bind(usage.output_tokens)
            .bind(usage.cached_tokens)
            .bind(usage.image_input_tokens)
            .bind(usage.image_output_tokens)
            .bind(usage.image_request_count)
            .bind(usage.image_request_failed_count)
            .bind(usage.request_count)
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

    /// 读取账号用量。
    pub async fn get_usage(
        &self,
        account_id: &str,
    ) -> SqliteAccountStoreResult<Option<AccountUsageRecord>> {
        let row = sqlx::query(GET_USAGE_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| usage_from_row(&row)).transpose()
    }

    /// 分页列出账号用量。
    pub async fn list_usage(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteAccountStoreResult<Page<AccountUsageListRecord>> {
        let fetch_limit = i64::from(limit) + 1;
        let rows = if let Some(cursor) = cursor {
            let (last_used_at, account_id) =
                decode_cursor(&cursor).ok_or(SqliteAccountStoreError::InvalidCursor)?;
            sqlx::query(LIST_USAGE_AFTER_CURSOR_SQL)
                .bind(&last_used_at)
                .bind(last_used_at)
                .bind(account_id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query(LIST_USAGE_SQL)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
        };

        let has_next = rows.len() > limit as usize;
        let take_count = rows.len().min(limit as usize);
        let items = rows
            .into_iter()
            .take(take_count)
            .map(|row| usage_list_from_row(&row))
            .collect::<SqliteAccountStoreResult<Vec<_>>>()?;
        let next_cursor = if has_next {
            items.last().map(|usage| {
                encode_cursor(
                    usage
                        .last_used_at
                        .as_ref()
                        .map(DateTime::to_rfc3339)
                        .as_deref()
                        .unwrap_or(""),
                    &usage.account_id,
                )
            })
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    /// 汇总账号用量。
    pub async fn usage_summary(&self) -> SqliteAccountStoreResult<AccountUsageSummary> {
        let row = sqlx::query(USAGE_SUMMARY_SQL).fetch_one(&self.pool).await?;
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

    /// 清零账号累计和当前窗口用量，保留窗口重置时间。
    pub async fn reset_usage(&self, account_id: &str) -> SqliteAccountStoreResult<bool> {
        let result = sqlx::query(RESET_USAGE_SQL)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 列出已缓存的账号配额快照。
    pub async fn list_quota_snapshots(
        &self,
    ) -> SqliteAccountStoreResult<Vec<AccountQuotaSnapshot>> {
        let rows = sqlx::query(LIST_QUOTA_SNAPSHOTS_SQL)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(quota_snapshot_from_row).collect()
    }

    /// 写入账号配额 JSON 快照。
    pub async fn update_quota_json(
        &self,
        account_id: &str,
        quota_json: &str,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(UPDATE_QUOTA_JSON_SQL)
            .bind(quota_json)
            .bind(&now)
            .bind(now)
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
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 标记账号进入 Cloudflare 冷却期。
    pub async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> SqliteAccountStoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(SET_CLOUDFLARE_COOLDOWN_UNTIL_SQL)
            .bind(cooldown_until.to_rfc3339())
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 读取账号配额 JSON 快照。
    pub async fn get_quota_json(
        &self,
        account_id: &str,
    ) -> SqliteAccountStoreResult<Option<String>> {
        let row = sqlx::query("select quota_json from accounts where id = ?")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|row| row.get("quota_json")))
    }

    /// 同步账号当前 rate-limit 统计窗口。
    pub async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> SqliteAccountStoreResult<()> {
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
        let reset_at_db = reset_at.to_rfc3339();
        let limit_window_seconds_db = limit_window_seconds.map(u64_to_i64_saturating);

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

    fn account_from_row(
        &self,
        row: &sqlx::sqlite::SqliteRow,
    ) -> SqliteAccountStoreResult<StoredAccount> {
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

    fn prepare_token_write(
        &self,
        access_token: &SecretString,
        refresh_token: Option<&SecretString>,
        access_token_expires_at: Option<DateTime<Utc>>,
    ) -> SqliteAccountStoreResult<TokenWrite> {
        Ok(TokenWrite {
            access_token_cipher: self.secret_box.encrypt(access_token)?,
            refresh_token_cipher: refresh_token
                .map(|token| self.secret_box.encrypt(token))
                .transpose()?,
            access_token_expires_at: access_token_expires_at.map(|value| value.to_rfc3339()),
            updated_at: Utc::now().to_rfc3339(),
        })
    }
}

fn account_metadata_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> SqliteAccountStoreResult<StoredAccountMetadata> {
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

fn map_account_store_error(error: impl ToString) -> AccountStoreError {
    AccountStoreError::OperationFailed {
        message: error.to_string(),
    }
}

fn sqlite_usage_delta(usage: AccountUsageDelta) -> UsageDelta {
    UsageDelta {
        request_count: u64_to_i64_saturating(usage.requests),
        input_tokens: u64_to_i64_saturating(usage.input_tokens),
        output_tokens: u64_to_i64_saturating(usage.output_tokens),
        cached_tokens: u64_to_i64_saturating(usage.cached_tokens),
        image_input_tokens: u64_to_i64_saturating(usage.image_input_tokens),
        image_output_tokens: u64_to_i64_saturating(usage.image_output_tokens),
        image_request_count: u64_to_i64_saturating(usage.image_requests),
        image_request_failed_count: u64_to_i64_saturating(usage.image_request_failures),
        empty_response_count: u64_to_i64_saturating(usage.empty_responses),
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

fn usage_from_row(row: &sqlx::sqlite::SqliteRow) -> SqliteAccountStoreResult<AccountUsageRecord> {
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
