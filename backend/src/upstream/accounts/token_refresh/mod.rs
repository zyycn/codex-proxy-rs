//! 刷新租约存储、JWT 解码与账号 claims 验证。

use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    hash::{Hash, Hasher},
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{Map, Value};
use tokio::sync::Semaphore;

use crate::upstream::accounts::model::{Account, AccountStatus};

/// 一次刷新返回的 token 对。
#[derive(Debug, Clone)]
pub struct TokenPair {
    /// 新的访问令牌。
    pub access_token: String,
    /// 可选的新刷新令牌。
    pub refresh_token: Option<String>,
}

// ---------------------------------------------------------------------------
// JWT 解码
// ---------------------------------------------------------------------------

/// JWT 过期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtExpiry {
    /// token 已过期。
    Expired,
    /// token 仍然有效。
    Valid,
    /// token 缺失、格式错误或不包含可解析的 exp。
    MissingOrInvalid,
}

/// 按给定时间点判断 JWT 的 `exp` 是否已过期。
pub fn jwt_expiry(token: &str, now: DateTime<Utc>) -> JwtExpiry {
    let Some(exp) = jwt_exp(token) else {
        return JwtExpiry::MissingOrInvalid;
    };
    if now.timestamp() >= exp {
        JwtExpiry::Expired
    } else {
        JwtExpiry::Valid
    }
}

/// 读取 JWT `exp` 并转换成 UTC 时间。
pub fn jwt_expiration(token: &str) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(jwt_exp(token)?, 0).single()
}

fn jwt_exp(token: &str) -> Option<i64> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value = serde_json::from_slice::<Value>(&decoded).ok()?;
    value.get("exp")?.as_i64()
}

// ---------------------------------------------------------------------------
// 账号 claims 解码（手动创建/导入时验证 JWT）
// ---------------------------------------------------------------------------

/// 手动创建账号时从 JWT 提取的 claims。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualAccountClaims {
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 邮箱。
    pub email: Option<String>,
    /// 订阅计划类型。
    pub plan_type: Option<String>,
    /// access token 过期时间。
    pub expires_at: DateTime<Utc>,
}

/// 从 JWT 中解码 payload 部分。
pub fn decode_jwt_payload(token: &str) -> Option<Map<String, Value>> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    if payload.is_empty() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .as_object()
        .cloned()
}

/// 从 JWT 中提取手动创建账号所需的 claims。
pub fn manual_account_claims(
    token: &str,
    now: DateTime<Utc>,
) -> Result<ManualAccountClaims, &'static str> {
    let payload = decode_jwt_payload(token).ok_or("Invalid JWT format")?;
    let exp = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or("Token is expired")?;
    if now.timestamp() >= exp {
        return Err("Token is expired");
    }
    let expires_at = DateTime::<Utc>::from_timestamp(exp, 0).ok_or("Invalid JWT exp claim")?;
    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
        .ok_or("Token missing OpenAI auth claim")?;
    let account_id = string_claim(auth, "chatgpt_account_id");
    let profile = payload
        .get("https://api.openai.com/profile")
        .and_then(Value::as_object);
    let user_id = string_claim(auth, "chatgpt_user_id")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_user_id")))
        .or_else(|| string_claim(auth, "user_id"))
        .or_else(|| profile.and_then(|profile| string_claim(profile, "user_id")));
    let plan_type = string_claim(auth, "chatgpt_plan_type")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_plan_type")));
    let email = profile.and_then(|profile| string_claim(profile, "email"));

    Ok(ManualAccountClaims {
        account_id,
        user_id,
        email,
        plan_type,
        expires_at,
    })
}

fn string_claim(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

// ---------------------------------------------------------------------------
// 刷新策略和上游端口
// ---------------------------------------------------------------------------

/// 刷新任务的调度策略。
#[derive(Debug, Clone, Copy)]
pub struct RefreshPolicy {
    /// 提前多久开始刷新访问令牌。
    pub refresh_margin_seconds: u64,
    /// 允许并发执行的刷新任务数。
    pub refresh_concurrency: u32,
}

/// 运行时共享刷新策略。
#[derive(Clone)]
pub struct RuntimeRefreshPolicy {
    policy: Arc<RwLock<RefreshPolicy>>,
    semaphore: Arc<RwLock<Arc<Semaphore>>>,
}

impl RuntimeRefreshPolicy {
    /// 构造共享刷新策略。
    pub fn new(policy: RefreshPolicy) -> Self {
        Self {
            policy: Arc::new(RwLock::new(policy)),
            semaphore: Arc::new(RwLock::new(Arc::new(Semaphore::new(
                policy.refresh_concurrency.max(1) as usize,
            )))),
        }
    }

    /// 返回当前策略快照。
    pub fn snapshot(&self) -> RefreshPolicy {
        *self
            .policy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 更新当前策略。
    pub fn update(&self, policy: RefreshPolicy) {
        *self
            .policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = policy;
        *self
            .semaphore
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Arc::new(Semaphore::new(policy.refresh_concurrency.max(1) as usize));
    }

    /// 返回当前提前刷新秒数。
    pub fn refresh_margin_seconds(&self) -> u64 {
        self.snapshot().refresh_margin_seconds
    }

    fn semaphore(&self) -> Arc<Semaphore> {
        self.semaphore
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl From<RefreshPolicy> for RuntimeRefreshPolicy {
    fn from(policy: RefreshPolicy) -> Self {
        Self::new(policy)
    }
}

/// 按账号稳定扰动刷新提前量，范围与 TS 版 `jitter(margin, 0.15)` 一致。
pub(crate) fn jittered_refresh_at(
    account_id: &str,
    expires_at: DateTime<Utc>,
    margin_seconds: u64,
) -> DateTime<Utc> {
    expires_at - Duration::seconds(jittered_refresh_margin_seconds(account_id, margin_seconds))
}

fn jittered_refresh_margin_seconds(account_id: &str, margin_seconds: u64) -> i64 {
    if margin_seconds == 0 {
        return 0;
    }

    let mut hasher = DefaultHasher::new();
    account_id.hash(&mut hasher);
    let unit = hasher.finish() as f64 / u64::MAX as f64;
    let factor = 0.85 + unit * 0.30;
    (margin_seconds as f64 * factor)
        .round()
        .clamp(0.0, i64::MAX as f64) as i64
}

/// 触发刷新动作的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTrigger {
    /// 在访问令牌即将过期前触发刷新。
    BeforeExpiry,
    /// 在上游返回未授权后立即刷新。
    Unauthorized,
}

/// 上游刷新失败后的领域结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RefreshFailure {
    /// 刷新令牌无效或已过期。
    #[error("refresh token is invalid or expired")]
    InvalidGrant,
    /// 账号被上游封禁。
    #[error("account is banned")]
    Banned,
    /// 刷新请求在到达服务端前失败，可安全复用当前 refresh token 重试。
    #[error("refresh transport failed before server processing")]
    RetryableTransport,
    /// 刷新请求在传输层失败，refresh token 可能已经被服务端消费。
    #[error("refresh transport failed after possible server processing")]
    Transport,
}

/// 调度器自身的执行错误。
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// 并发限制信号量已关闭。
    #[error("refresh task semaphore closed")]
    ConcurrencyClosed,
    /// 刷新请求在到达服务端前失败，可安全复用当前 refresh token 重试。
    #[error("refresh transport failed before server processing")]
    RetryableTransport,
    /// 刷新请求在传输层失败，refresh token 可能已经被服务端消费。
    #[error("refresh transport failed after possible server processing")]
    Transport,
}

/// 刷新令牌的上游端口。
#[async_trait]
pub trait TokenRefresher: Send + Sync + 'static {
    /// 使用给定刷新令牌换取新的 token 对。
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure>;
}

/// 负责执行单账号刷新策略的调度器。
#[derive(Clone)]
pub struct RefreshScheduler<C> {
    policy: RuntimeRefreshPolicy,
    client: Arc<C>,
}

impl<C> RefreshScheduler<C>
where
    C: TokenRefresher,
{
    /// 使用策略和上游刷新端口构造调度器。
    pub fn new(policy: impl Into<RuntimeRefreshPolicy>, client: C) -> Self {
        Self {
            policy: policy.into(),
            client: Arc::new(client),
        }
    }

    /// 更新刷新策略。
    pub fn update_policy(&self, policy: RefreshPolicy) {
        self.policy.update(policy);
    }

    /// 在给定时间点按触发原因刷新账号。
    ///
    /// 当账号不需要刷新时返回原账号快照；当刷新失败但属于可映射的领域错误时，
    /// 返回更新过状态的账号。
    ///
    /// # Errors
    ///
    /// 当并发控制已关闭或刷新传输失败时返回 [`RefreshError`]。
    pub async fn refresh_account_at(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> Result<Account, RefreshError> {
        if !self.should_refresh(account, trigger, now) {
            return Ok(account.clone());
        }

        let Some(refresh_token) = account.refresh_token.as_deref() else {
            let mut expired = account.clone();
            expired.status = AccountStatus::Expired;
            return Ok(expired);
        };

        let semaphore = self.policy.semaphore();
        let _permit = semaphore
            .acquire()
            .await
            .map_err(|_| RefreshError::ConcurrencyClosed)?;

        match self.client.refresh(refresh_token).await {
            Ok(token_pair) => Ok(apply_token_pair(account, token_pair)),
            Err(RefreshFailure::RetryableTransport) => Err(RefreshError::RetryableTransport),
            Err(RefreshFailure::Transport) => Err(RefreshError::Transport),
            Err(error) => Ok(apply_refresh_failure(account, error)),
        }
    }

    /// 判断账号在给定触发原因下是否需要刷新。
    pub fn should_refresh_account_at(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> bool {
        self.should_refresh(account, trigger, now)
    }

    fn should_refresh(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> bool {
        match trigger {
            RefreshTrigger::Unauthorized => can_refresh_after_auth_failure(account.status),
            RefreshTrigger::BeforeExpiry => {
                can_refresh_before_expiry(account.status)
                    && self.should_refresh_before_expiry(account, now)
            }
        }
    }

    fn should_refresh_before_expiry(&self, account: &Account, now: DateTime<Utc>) -> bool {
        if let Some(next_refresh_at) = account.next_refresh_at {
            return next_refresh_at <= now;
        }

        account
            .access_token_expires_at
            .is_some_and(|expires_at| self.refresh_at(account, expires_at) <= now)
    }

    fn refresh_at(&self, account: &Account, expires_at: DateTime<Utc>) -> DateTime<Utc> {
        let policy = self.policy.snapshot();
        jittered_refresh_at(
            account.id.as_str(),
            expires_at,
            policy.refresh_margin_seconds,
        )
    }
}

fn can_refresh_after_auth_failure(status: AccountStatus) -> bool {
    matches!(
        status,
        AccountStatus::Active | AccountStatus::Expired | AccountStatus::QuotaExhausted
    )
}

fn can_refresh_before_expiry(status: AccountStatus) -> bool {
    matches!(
        status,
        AccountStatus::Active | AccountStatus::QuotaExhausted
    )
}

fn status_after_successful_token_refresh(status: AccountStatus) -> AccountStatus {
    match status {
        AccountStatus::Expired => AccountStatus::Active,
        status => status,
    }
}

/// 将新的 token 对应用到账号快照上。
pub fn apply_token_pair(account: &Account, token_pair: TokenPair) -> Account {
    let mut refreshed = account.clone();
    refreshed.access_token = token_pair.access_token;

    // 刷新响应不返回 refresh_token 时，继续保留已有值，避免永久失去刷新能力。
    if let Some(refresh_token) = token_pair.refresh_token {
        refreshed.refresh_token = Some(refresh_token);
    }

    refreshed.status = status_after_successful_token_refresh(account.status);
    refreshed
}

/// 将刷新失败映射为账号状态变更。
pub fn apply_refresh_failure(account: &Account, failure: RefreshFailure) -> Account {
    let mut updated = account.clone();
    updated.status = match failure {
        RefreshFailure::InvalidGrant => AccountStatus::Expired,
        RefreshFailure::Banned => AccountStatus::Banned,
        RefreshFailure::RetryableTransport => AccountStatus::Active,
        RefreshFailure::Transport => AccountStatus::Active,
    };
    updated
}

// ---------------------------------------------------------------------------
// 刷新租约存储（SQLite）
// ---------------------------------------------------------------------------

use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use thiserror::Error;

/// SQLite 刷新租约存储结果。
pub type RefreshLeaseStoreResult<T> = Result<T, RefreshLeaseStoreError>;

/// SQLite 刷新租约存储错误。
#[derive(Debug, Error)]
pub enum RefreshLeaseStoreError {
    /// SQLite 操作失败。
    #[error("sqlite refresh lease query failed: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// SQLite 刷新租约存储。
#[derive(Clone)]
pub struct RefreshLeaseStore {
    pool: SqlitePool,
}

impl RefreshLeaseStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 尝试获取账号刷新租约。
    pub async fn try_acquire(
        &self,
        account_id: &str,
        owner: &str,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> RefreshLeaseStoreResult<bool> {
        let result = sqlx::query(
            r"
insert into account_refresh_leases (account_id, owner, expires_at, updated_at)
values (?, ?, ?, ?)
on conflict(account_id) do update set
  owner = excluded.owner,
  expires_at = excluded.expires_at,
  updated_at = excluded.updated_at
where account_refresh_leases.expires_at <= ?
  or account_refresh_leases.owner = ?
",
        )
        .bind(account_id)
        .bind(owner)
        .bind(expires_at.to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(owner)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 释放账号刷新租约。
    pub async fn release(&self, account_id: &str, owner: &str) -> RefreshLeaseStoreResult<bool> {
        let result =
            sqlx::query("delete from account_refresh_leases where account_id = ? and owner = ?")
                .bind(account_id)
                .bind(owner)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 返回给定账号集合中仍有效的刷新租约。
    pub async fn active_account_ids(
        &self,
        account_ids: &[String],
        now: DateTime<Utc>,
    ) -> RefreshLeaseStoreResult<HashSet<String>> {
        if account_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let mut builder = QueryBuilder::<Sqlite>::new(
            "select account_id from account_refresh_leases where datetime(expires_at) > datetime(",
        );
        builder.push_bind(now.to_rfc3339());
        builder.push(") and account_id in (");
        let mut separated = builder.separated(", ");
        for account_id in account_ids {
            separated.push_bind(account_id);
        }
        separated.push_unseparated(")");

        let rows = builder
            .build_query_as::<(String,)>()
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(|(account_id,)| account_id).collect())
    }
}

mod runtime;

pub use runtime::{
    RuntimeTokenRefreshService, TokenRefreshServiceError, TokenRefreshServiceResult,
    TokenRefreshSummary, TokenTimerSummary,
};
