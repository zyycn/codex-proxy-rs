//! 刷新租约存储、JWT 解码与账号 claims 验证。

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::{Arc, RwLock},
    time::Duration as StdDuration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{Map, Value};
use tokio::sync::{watch, Semaphore};

use crate::fleet::account::{Account, AccountStatus};
use crate::upstream::openai::token_client::{RefreshFailure, TokenPair, TokenRefresher};

pub(crate) const PERMANENT_FAILURE_CONFIRMATION_THRESHOLD: usize = 2;
pub(crate) const RECOVERY_DELAY_SECONDS: i64 = 10 * 60;
pub(crate) const RETRY_DELAY_JITTER: f64 = 0.30;
pub(crate) const RECOVERY_DELAY_JITTER: f64 = 0.20;

const MAX_REFRESH_ATTEMPTS: usize = 5;
const REFRESH_RETRY_BASE_DELAY_MILLIS: u64 = 5_000;
const REFRESH_RETRY_MAX_DELAY_MILLIS: u64 = 300_000;

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

/// 单次刷新扫描摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenRefreshSummary {
    /// 扫描账号数。
    pub scanned: usize,
    /// 成功刷新 token 的账号数。
    pub refreshed: usize,
    /// 仅更新状态的账号数。
    pub status_updated: usize,
    /// 无需刷新的账号数。
    pub skipped: usize,
    /// 传输失败账号数。
    pub failed: usize,
}

/// 账号级刷新定时器调度摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTimerSummary {
    /// 扫描账号数。
    pub scanned: usize,
    /// 已调度的未来刷新定时器数。
    pub scheduled: usize,
    /// 已调度的立即刷新定时器数。
    pub immediate: usize,
    /// 无需调度的账号数。
    pub skipped: usize,
    /// 被替换的既有定时器数。
    pub replaced: usize,
}

impl TokenTimerSummary {
    /// 返回本轮调度发生变化的定时器数量。
    pub fn changed(&self) -> usize {
        self.scheduled + self.immediate
    }
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

    /// 持续接收运行时设置并更新刷新策略。
    pub async fn subscribe_settings(
        self,
        mut receiver: watch::Receiver<crate::settings::SettingsSnapshot>,
    ) {
        while receiver.changed().await.is_ok() {
            let settings = receiver.borrow_and_update();
            self.update(RefreshPolicy {
                refresh_margin_seconds: settings.refresh_margin_seconds,
                refresh_concurrency: settings.refresh_concurrency,
            });
        }
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

    /// 在给定时间点按到期策略刷新账号。
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
        now: DateTime<Utc>,
    ) -> Result<Account, RefreshError> {
        if !self.should_refresh_account_at(account, now) {
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

    /// 判断账号在给定时间点是否需要刷新。
    pub fn should_refresh_account_at(&self, account: &Account, now: DateTime<Utc>) -> bool {
        token_refresh_status_eligible(account.status)
            && self.should_refresh_before_expiry(account, now)
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

pub(crate) fn token_refresh_status_eligible(status: AccountStatus) -> bool {
    matches!(
        status,
        AccountStatus::Active | AccountStatus::QuotaExhausted
    )
}

/// 将新的 token 对应用到账号快照上。
pub fn apply_token_pair(account: &Account, token_pair: TokenPair) -> Account {
    let mut refreshed = account.clone();
    refreshed.access_token = token_pair.access_token;

    // 刷新响应不返回 refresh_token 时，继续保留已有值，避免永久失去刷新能力。
    if let Some(refresh_token) = token_pair.refresh_token {
        refreshed.refresh_token = Some(refresh_token);
    }

    refreshed
}

/// 将刷新失败映射为账号状态变更。
pub fn apply_refresh_failure(account: &Account, failure: RefreshFailure) -> Account {
    let mut updated = account.clone();
    updated.status = match failure {
        RefreshFailure::InvalidGrant => AccountStatus::Expired,
        RefreshFailure::Banned => AccountStatus::Banned,
        RefreshFailure::RetryableTransport | RefreshFailure::Transport => account.status,
    };
    updated
}

pub(crate) fn is_permanent_refresh_failure_status(status: AccountStatus) -> bool {
    matches!(status, AccountStatus::Expired | AccountStatus::Banned)
}

pub(crate) fn default_refresh_retry_delays() -> Vec<StdDuration> {
    (0..MAX_REFRESH_ATTEMPTS.saturating_sub(1))
        .map(|attempt_index| {
            let multiplier = 3_u64.saturating_pow(attempt_index as u32);
            let millis = REFRESH_RETRY_BASE_DELAY_MILLIS
                .saturating_mul(multiplier)
                .min(REFRESH_RETRY_MAX_DELAY_MILLIS);
            StdDuration::from_millis(millis)
        })
        .collect()
}

pub(crate) fn stable_jittered_duration(
    account_id: &str,
    base: StdDuration,
    variance: f64,
    salt: &str,
) -> StdDuration {
    if base.is_zero() {
        return StdDuration::ZERO;
    }

    let mut hasher = DefaultHasher::new();
    account_id.hash(&mut hasher);
    salt.hash(&mut hasher);
    let unit = hasher.finish() as f64 / u64::MAX as f64;
    let factor = (1.0 - variance) + unit * variance * 2.0;
    let millis = (base.as_millis() as f64 * factor)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64;
    StdDuration::from_millis(millis)
}

pub(crate) fn scheduled_at_from_delay(now: DateTime<Utc>, delay: StdDuration) -> DateTime<Utc> {
    match chrono::Duration::from_std(delay) {
        Ok(delay) => now + delay,
        Err(_) => now,
    }
}

pub(crate) fn duration_until(now: DateTime<Utc>, scheduled_at: DateTime<Utc>) -> StdDuration {
    (scheduled_at - now).to_std().unwrap_or(StdDuration::ZERO)
}

pub(crate) fn same_refresh_time(left: Option<DateTime<Utc>>, right: Option<DateTime<Utc>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.timestamp() == right.timestamp(),
        (None, None) => true,
        _ => false,
    }
}
