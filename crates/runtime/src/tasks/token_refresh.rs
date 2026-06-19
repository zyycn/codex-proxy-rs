//! 令牌刷新任务。

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use codex_proxy_adapters::sqlite::{
    accounts::{AccountClaimsUpdate, SqliteAccountStore, SqliteAccountStoreError, StoredAccount},
    refresh_leases::{SqliteRefreshLeaseStore, SqliteRefreshLeaseStoreError},
};
use codex_proxy_core::{
    accounts::{
        jwt::jwt_expiration,
        model::{Account, AccountStatus},
        ports::{AccountStore, AccountStoreError},
    },
    auth::{
        oauth::{RefreshError, RefreshPolicy, RefreshScheduler, RefreshTrigger},
        ports::TokenRefresher,
    },
};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::{interval, sleep},
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::coordinator::SchedulerHandle;

const DEFAULT_INTERVAL_SECS: u64 = 60;
const REFRESH_LEASE_TTL_SECONDS: i64 = 5 * 60;
const MAX_REFRESH_ATTEMPTS: usize = 5;
const REFRESH_RETRY_BASE_DELAY_MILLIS: u64 = 5_000;
const REFRESH_RETRY_MAX_DELAY_MILLIS: u64 = 300_000;
const PERMANENT_FAILURE_CONFIRMATION_THRESHOLD: usize = 2;
const RECOVERY_DELAY_SECONDS: i64 = 10 * 60;

/// 令牌刷新任务。
pub struct TokenRefreshTask<C>
where
    C: TokenRefresher,
{
    store: SqliteAccountStore,
    scheduler: Arc<RefreshScheduler<C>>,
    refresh_margin_seconds: u64,
    interval_secs: u64,
    refresh_leases: Option<SqliteRefreshLeaseStore>,
    lease_owner: String,
    retry_delays: Vec<Duration>,
    in_flight: Arc<Mutex<HashSet<String>>>,
    recovery_not_before: Arc<Mutex<HashMap<String, DateTime<Utc>>>>,
    timers: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
}

impl<C> Clone for TokenRefreshTask<C>
where
    C: TokenRefresher,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            scheduler: self.scheduler.clone(),
            refresh_margin_seconds: self.refresh_margin_seconds,
            interval_secs: self.interval_secs,
            refresh_leases: self.refresh_leases.clone(),
            lease_owner: self.lease_owner.clone(),
            retry_delays: self.retry_delays.clone(),
            in_flight: self.in_flight.clone(),
            recovery_not_before: self.recovery_not_before.clone(),
            timers: self.timers.clone(),
        }
    }
}

impl<C> TokenRefreshTask<C>
where
    C: TokenRefresher,
{
    /// 构造默认令牌刷新任务。
    pub fn new(store: SqliteAccountStore, policy: RefreshPolicy, client: C) -> Self {
        Self {
            store,
            scheduler: Arc::new(RefreshScheduler::new(policy, client)),
            refresh_margin_seconds: policy.refresh_margin_seconds,
            interval_secs: DEFAULT_INTERVAL_SECS,
            refresh_leases: None,
            lease_owner: refresh_lease_owner(),
            retry_delays: default_refresh_retry_delays(),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            recovery_not_before: Arc::new(Mutex::new(HashMap::new())),
            timers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 使用自定义间隔构造令牌刷新任务。
    pub fn with_interval_secs(
        store: SqliteAccountStore,
        policy: RefreshPolicy,
        client: C,
        interval_secs: u64,
    ) -> Self {
        Self {
            store,
            scheduler: Arc::new(RefreshScheduler::new(policy, client)),
            refresh_margin_seconds: policy.refresh_margin_seconds,
            interval_secs,
            refresh_leases: None,
            lease_owner: refresh_lease_owner(),
            retry_delays: default_refresh_retry_delays(),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            recovery_not_before: Arc::new(Mutex::new(HashMap::new())),
            timers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 使用刷新租约存储保护账号刷新。
    pub fn with_refresh_lease_store(mut self, refresh_leases: SqliteRefreshLeaseStore) -> Self {
        self.refresh_leases = Some(refresh_leases);
        self
    }

    /// 使用自定义刷新重试延迟。
    pub fn with_retry_delays<I>(mut self, retry_delays: I) -> Self
    where
        I: IntoIterator<Item = Duration>,
    {
        self.retry_delays = retry_delays.into_iter().collect();
        self
    }

    /// 启动后台刷新任务。
    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(self.interval_secs));
            info!(interval_secs = self.interval_secs, "token 刷新任务已启动");

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.schedule_account_timers_once().await {
                            Ok(summary) if summary.changed() > 0 => {
                                info!(
                                    scheduled = summary.scheduled,
                                    immediate = summary.immediate,
                                    recovery_scheduled = summary.recovery_scheduled,
                                    replaced = summary.replaced,
                                    "token 刷新定时器已调度"
                                );
                            }
                            Ok(summary) => {
                                debug!(
                                    scanned = summary.scanned,
                                    skipped = summary.skipped,
                                    "没有需要调度刷新的 token"
                                );
                            }
                            Err(error) => {
                                warn!(error = %error, "token 刷新定时器调度失败");
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        self.clear_scheduled_timers().await;
                        info!("token 刷新任务已关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    /// 执行一次到期刷新扫描。
    pub async fn refresh_due_accounts_once(&self) -> TokenRefreshTaskResult<TokenRefreshSummary> {
        self.refresh_due_accounts_once_at(Utc::now()).await
    }

    /// 执行一次账号级刷新定时器调度。
    pub async fn schedule_account_timers_once(&self) -> TokenRefreshTaskResult<TokenTimerSummary> {
        self.schedule_account_timers_once_at(Utc::now()).await
    }

    /// 在指定时间点执行一次账号级刷新定时器调度。
    pub async fn schedule_account_timers_once_at(
        &self,
        now: DateTime<Utc>,
    ) -> TokenRefreshTaskResult<TokenTimerSummary> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(TokenRefreshTaskError::AccountStore)?;
        let mut summary = TokenTimerSummary {
            scanned: accounts.len(),
            ..TokenTimerSummary::default()
        };
        let mut expired_recovery_index = 0;

        for account in accounts {
            if account.refresh_token.is_none()
                || matches!(
                    account.status,
                    AccountStatus::Disabled | AccountStatus::Banned
                )
            {
                summary.skipped += 1;
                continue;
            }

            let timer = match account.status {
                AccountStatus::Refreshing => Some((Duration::ZERO, RefreshTrigger::Unauthorized)),
                AccountStatus::Expired => {
                    let delay = Duration::from_secs(30 + expired_recovery_index * 2);
                    expired_recovery_index += 1;
                    Some((delay, RefreshTrigger::Unauthorized))
                }
                _ => self
                    .timer_delay_for_account(&account, now)
                    .map(|delay| (delay, RefreshTrigger::BeforeExpiry)),
            };

            let Some((delay, trigger)) = timer else {
                summary.skipped += 1;
                continue;
            };

            if delay.is_zero() && account.status != AccountStatus::Expired {
                if self.clear_scheduled_timer(&account.id).await {
                    summary.replaced += 1;
                }
                let outcome = self
                    .refresh_scheduled_account(&account.id, trigger, now)
                    .await?;
                if matches!(outcome, TokenRefreshOutcome::Skipped) {
                    summary.skipped += 1;
                } else {
                    summary.immediate += 1;
                }
                continue;
            }

            let scheduled_at = scheduled_at_from_delay(now, delay);
            if self
                .schedule_account_timer(account.id.clone(), delay, trigger, scheduled_at)
                .await
            {
                summary.replaced += 1;
            }

            if account.status == AccountStatus::Expired {
                summary.recovery_scheduled += 1;
            } else if delay.is_zero() {
                summary.immediate += 1;
            } else {
                summary.scheduled += 1;
            }
        }

        Ok(summary)
    }

    /// 返回当前账号级刷新定时器数量。
    pub async fn scheduled_timer_count(&self) -> usize {
        self.timers.lock().await.len()
    }

    /// 在指定时间点执行一次到期刷新扫描。
    pub async fn refresh_due_accounts_once_at(
        &self,
        now: DateTime<Utc>,
    ) -> TokenRefreshTaskResult<TokenRefreshSummary> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(TokenRefreshTaskError::AccountStore)?;
        let mut summary = TokenRefreshSummary {
            scanned: accounts.len(),
            ..TokenRefreshSummary::default()
        };

        for account in accounts {
            if self.is_recovery_delayed(&account.id, now).await {
                summary.skipped += 1;
                continue;
            }

            let Some(trigger) = self.refresh_trigger_for_account(&account, now) else {
                summary.skipped += 1;
                continue;
            };

            if !self.try_mark_in_flight(&account.id).await {
                summary.skipped += 1;
                continue;
            }

            let lease_acquired = match self.try_acquire_refresh_lease(&account.id, now).await {
                Ok(acquired) => acquired,
                Err(error) => {
                    self.release_in_flight(&account.id).await;
                    return Err(error);
                }
            };
            if !lease_acquired {
                self.release_in_flight(&account.id).await;
                summary.skipped += 1;
                continue;
            }

            let outcome = self
                .refresh_account_with_status_transitions(&account, trigger, now)
                .await;

            self.release_in_flight(&account.id).await;
            self.release_refresh_lease(&account.id).await?;

            let outcome = outcome?;
            self.schedule_next_refresh_after_outcome(&outcome, Utc::now())
                .await;

            match outcome {
                TokenRefreshOutcome::Refreshed(_) => summary.refreshed += 1,
                TokenRefreshOutcome::StatusUpdated => summary.status_updated += 1,
                TokenRefreshOutcome::Skipped => summary.skipped += 1,
                TokenRefreshOutcome::Failed => summary.failed += 1,
            };
        }

        Ok(summary)
    }

    fn timer_delay_for_account(&self, account: &Account, now: DateTime<Utc>) -> Option<Duration> {
        let expires_at = account
            .access_token_expires_at
            .or_else(|| jwt_expiration(&account.access_token))?;
        let margin_seconds = self.refresh_margin_seconds.min(i64::MAX as u64) as i64;
        let refresh_at = expires_at - chrono::Duration::seconds(margin_seconds);

        if refresh_at <= now {
            return Some(Duration::ZERO);
        }

        (refresh_at - now).to_std().ok()
    }

    fn schedule_account_timer(
        &self,
        account_id: String,
        delay: Duration,
        trigger: RefreshTrigger,
        scheduled_at: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async move {
            let replaced = self.clear_scheduled_timer(&account_id).await;
            let task = self.clone();
            let timer_account_id = account_id.clone();
            let handle = tokio::spawn(async move {
                if !delay.is_zero() {
                    sleep(delay).await;
                }
                task.timers.lock().await.remove(&timer_account_id);
                if let Err(error) = task
                    .refresh_scheduled_account(&timer_account_id, trigger, scheduled_at)
                    .await
                {
                    warn!(
                        account_id = %timer_account_id,
                        error = %error,
                        "scheduled token refresh failed"
                    );
                }
            });

            self.timers.lock().await.insert(account_id, handle);
            replaced
        })
    }

    async fn clear_scheduled_timer(&self, account_id: &str) -> bool {
        if let Some(handle) = self.timers.lock().await.remove(account_id) {
            handle.abort();
            true
        } else {
            false
        }
    }

    async fn clear_scheduled_timers(&self) {
        let mut timers = self.timers.lock().await;
        for (_, handle) in timers.drain() {
            handle.abort();
        }
    }

    async fn refresh_scheduled_account(
        &self,
        account_id: &str,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> TokenRefreshTaskResult<TokenRefreshOutcome> {
        if self.is_recovery_delayed(account_id, now).await {
            return Ok(TokenRefreshOutcome::Skipped);
        }

        let Some(account) = self.store.get(account_id).await? else {
            return Err(TokenRefreshTaskError::AccountNotFound(
                account_id.to_string(),
            ));
        };
        let account = stored_account_to_refresh_account(account);

        if !self.try_mark_in_flight(&account.id).await {
            return Ok(TokenRefreshOutcome::Skipped);
        }

        let lease_acquired = match self.try_acquire_refresh_lease(&account.id, now).await {
            Ok(acquired) => acquired,
            Err(error) => {
                self.release_in_flight(&account.id).await;
                return Err(error);
            }
        };
        if !lease_acquired {
            self.release_in_flight(&account.id).await;
            return Ok(TokenRefreshOutcome::Skipped);
        }

        let outcome = self
            .refresh_account_with_status_transitions(&account, trigger, now)
            .await;

        self.release_in_flight(&account.id).await;
        self.release_refresh_lease(&account.id).await?;

        let outcome = outcome?;
        self.schedule_next_refresh_after_outcome(&outcome, Utc::now())
            .await;
        Ok(outcome)
    }

    async fn schedule_next_refresh_after_outcome(
        &self,
        outcome: &TokenRefreshOutcome,
        now: DateTime<Utc>,
    ) {
        let TokenRefreshOutcome::Refreshed(account) = outcome else {
            return;
        };

        let Some(delay) = self.timer_delay_for_account(account, now) else {
            return;
        };

        if delay.is_zero() {
            debug!(
                account_id = %account.id,
                "refreshed token is already within refresh margin; skipping next timer"
            );
            return;
        }

        let scheduled_at = scheduled_at_from_delay(now, delay);
        self.schedule_account_timer(
            account.id.clone(),
            delay,
            RefreshTrigger::BeforeExpiry,
            scheduled_at,
        )
        .await;
    }

    fn refresh_trigger_for_account(
        &self,
        account: &Account,
        now: DateTime<Utc>,
    ) -> Option<RefreshTrigger> {
        if account.status == AccountStatus::Refreshing {
            return Some(RefreshTrigger::Unauthorized);
        }

        self.scheduler
            .should_refresh_account_at(account, RefreshTrigger::BeforeExpiry, now)
            .then_some(RefreshTrigger::BeforeExpiry)
    }

    async fn refresh_account_with_status_transitions(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> TokenRefreshTaskResult<TokenRefreshOutcome> {
        let attempt = self.prepare_refresh_attempt(account).await?;

        if attempt.refresh_token.is_none() {
            persist_status(&self.store, &account.id, AccountStatus::Expired).await?;
            self.clear_recovery_delay(&account.id).await;
            return Ok(TokenRefreshOutcome::StatusUpdated);
        }

        let max_attempts = self.retry_delays.len() + 1;
        let mut permanent_failures = 0;

        for attempt_index in 0..max_attempts {
            match self
                .scheduler
                .refresh_account_at(&attempt, trigger, now)
                .await
            {
                Ok(mut updated) if account_requires_token_write(account, &updated) => {
                    updated.access_token_expires_at =
                        jwt_expiration(&updated.access_token).or(updated.access_token_expires_at);
                    persist_token_update(&self.store, &updated).await?;
                    self.clear_recovery_delay(&account.id).await;
                    return Ok(TokenRefreshOutcome::Refreshed(Box::new(updated)));
                }
                Ok(updated) if account.status != updated.status => {
                    if is_permanent_refresh_status(updated.status) {
                        permanent_failures += 1;
                        if permanent_failures < PERMANENT_FAILURE_CONFIRMATION_THRESHOLD
                            && self.sleep_before_retry(attempt_index).await
                        {
                            continue;
                        }
                    }

                    persist_status(&self.store, &account.id, updated.status).await?;
                    self.clear_recovery_delay(&account.id).await;
                    return Ok(TokenRefreshOutcome::StatusUpdated);
                }
                Ok(_) => return Ok(TokenRefreshOutcome::Skipped),
                Err(RefreshError::Transport) if self.sleep_before_retry(attempt_index).await => {
                    continue;
                }
                Err(RefreshError::Transport) => {
                    persist_status(&self.store, &account.id, AccountStatus::Active).await?;
                    self.schedule_recovery(&account.id, now).await;
                    return Ok(TokenRefreshOutcome::Failed);
                }
                Err(error) => return Err(TokenRefreshTaskError::Refresh(error)),
            }
        }

        Ok(TokenRefreshOutcome::Skipped)
    }

    async fn prepare_refresh_attempt(&self, account: &Account) -> TokenRefreshTaskResult<Account> {
        if account.status != AccountStatus::Refreshing {
            persist_status(&self.store, &account.id, AccountStatus::Refreshing).await?;
        }

        let mut attempt = account.clone();
        attempt.status = AccountStatus::Active;
        Ok(attempt)
    }

    async fn sleep_before_retry(&self, attempt_index: usize) -> bool {
        let Some(delay) = self.retry_delays.get(attempt_index).copied() else {
            return false;
        };

        if !delay.is_zero() {
            sleep(delay).await;
        }
        true
    }

    async fn try_mark_in_flight(&self, account_id: &str) -> bool {
        self.in_flight.lock().await.insert(account_id.to_string())
    }

    async fn release_in_flight(&self, account_id: &str) {
        self.in_flight.lock().await.remove(account_id);
    }

    async fn is_recovery_delayed(&self, account_id: &str, now: DateTime<Utc>) -> bool {
        let mut recovery_not_before = self.recovery_not_before.lock().await;
        let Some(not_before) = recovery_not_before.get(account_id).copied() else {
            return false;
        };

        if now < not_before {
            return true;
        }

        recovery_not_before.remove(account_id);
        false
    }

    async fn schedule_recovery(&self, account_id: &str, now: DateTime<Utc>) {
        self.recovery_not_before.lock().await.insert(
            account_id.to_string(),
            now + chrono::Duration::seconds(RECOVERY_DELAY_SECONDS),
        );
    }

    async fn clear_recovery_delay(&self, account_id: &str) {
        self.recovery_not_before.lock().await.remove(account_id);
    }

    async fn try_acquire_refresh_lease(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> TokenRefreshTaskResult<bool> {
        let Some(refresh_leases) = self.refresh_leases.as_ref() else {
            return Ok(true);
        };
        refresh_leases
            .try_acquire(
                account_id,
                &self.lease_owner,
                now + chrono::Duration::seconds(REFRESH_LEASE_TTL_SECONDS),
                now,
            )
            .await
            .map_err(TokenRefreshTaskError::Lease)
    }

    async fn release_refresh_lease(&self, account_id: &str) -> TokenRefreshTaskResult<()> {
        let Some(refresh_leases) = self.refresh_leases.as_ref() else {
            return Ok(());
        };
        refresh_leases
            .release(account_id, &self.lease_owner)
            .await
            .map(|_| ())
            .map_err(TokenRefreshTaskError::Lease)
    }
}

enum TokenRefreshOutcome {
    Refreshed(Box<Account>),
    StatusUpdated,
    Skipped,
    Failed,
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
    /// 已调度的过期账号恢复定时器数。
    pub recovery_scheduled: usize,
    /// 无需调度的账号数。
    pub skipped: usize,
    /// 被替换的既有定时器数。
    pub replaced: usize,
}

impl TokenTimerSummary {
    fn changed(&self) -> usize {
        self.scheduled + self.immediate + self.recovery_scheduled
    }
}

/// 令牌刷新任务错误。
#[derive(Debug, Error)]
pub enum TokenRefreshTaskError {
    /// 账号存储读取失败。
    #[error("failed to list accounts for token refresh: {0}")]
    AccountStore(#[from] AccountStoreError),
    /// 账号存储写入失败。
    #[error("failed to persist token refresh result: {0}")]
    Store(#[from] SqliteAccountStoreError),
    /// 刷新租约存储失败。
    #[error("failed to coordinate token refresh lease: {0}")]
    Lease(#[from] SqliteRefreshLeaseStoreError),
    /// core 刷新调度失败。
    #[error("token refresh scheduler failed: {0}")]
    Refresh(#[from] RefreshError),
    /// 刷新期间账号不存在。
    #[error("account disappeared during token refresh: {0}")]
    AccountNotFound(String),
}

/// 令牌刷新任务结果。
pub type TokenRefreshTaskResult<T> = Result<T, TokenRefreshTaskError>;

async fn persist_token_update(
    store: &SqliteAccountStore,
    account: &Account,
) -> TokenRefreshTaskResult<()> {
    let updated = store
        .update_from_claims(
            &account.id,
            AccountClaimsUpdate {
                email: account.email.clone(),
                account_id: account.account_id.clone(),
                user_id: account.user_id.clone(),
                plan_type: account.plan_type.clone(),
                access_token: SecretString::new(account.access_token.clone().into()),
                refresh_token: account
                    .refresh_token
                    .clone()
                    .map(|token| SecretString::new(token.into())),
                access_token_expires_at: jwt_expiration(&account.access_token)
                    .or(account.access_token_expires_at),
                status: account.status,
            },
        )
        .await?;
    if updated {
        Ok(())
    } else {
        Err(TokenRefreshTaskError::AccountNotFound(account.id.clone()))
    }
}

async fn persist_status(
    store: &SqliteAccountStore,
    account_id: &str,
    status: AccountStatus,
) -> TokenRefreshTaskResult<()> {
    let updated = store.set_status(account_id, status).await?;
    if updated {
        Ok(())
    } else {
        Err(TokenRefreshTaskError::AccountNotFound(
            account_id.to_string(),
        ))
    }
}

fn account_requires_token_write(previous: &Account, updated: &Account) -> bool {
    previous.access_token != updated.access_token
        || previous.refresh_token != updated.refresh_token
        || (updated.status == AccountStatus::Active && previous.status != updated.status)
}

fn is_permanent_refresh_status(status: AccountStatus) -> bool {
    matches!(
        status,
        AccountStatus::Expired
            | AccountStatus::QuotaExhausted
            | AccountStatus::Disabled
            | AccountStatus::Banned
    )
}

fn refresh_lease_owner() -> String {
    format!("runtime-token-refresh:{}", Uuid::new_v4().simple())
}

fn default_refresh_retry_delays() -> Vec<Duration> {
    (0..MAX_REFRESH_ATTEMPTS.saturating_sub(1))
        .map(|attempt_index| {
            let multiplier = 3_u64.saturating_pow(attempt_index as u32);
            let millis = REFRESH_RETRY_BASE_DELAY_MILLIS
                .saturating_mul(multiplier)
                .min(REFRESH_RETRY_MAX_DELAY_MILLIS);
            Duration::from_millis(millis)
        })
        .collect()
}

fn scheduled_at_from_delay(now: DateTime<Utc>, delay: Duration) -> DateTime<Utc> {
    match chrono::Duration::from_std(delay) {
        Ok(delay) => now + delay,
        Err(_) => now,
    }
}

fn stored_account_to_refresh_account(stored: StoredAccount) -> Account {
    Account {
        id: stored.id,
        email: stored.email,
        account_id: stored.account_id,
        user_id: stored.user_id,
        label: stored.label,
        plan_type: stored.plan_type,
        access_token: stored.access_token.expose_secret().to_string(),
        refresh_token: stored
            .refresh_token
            .map(|token| token.expose_secret().to_string()),
        access_token_expires_at: stored.access_token_expires_at,
        status: stored.status,
        quota_limit_reached: false,
        quota_verify_required: false,
        quota_cooldown_until: None,
        cloudflare_cooldown_until: None,
        request_count: 0,
        empty_response_count: 0,
        image_input_tokens: 0,
        image_output_tokens: 0,
        image_request_count: 0,
        image_request_failed_count: 0,
        window_request_count: 0,
        window_input_tokens: 0,
        window_output_tokens: 0,
        window_cached_tokens: 0,
        window_image_input_tokens: 0,
        window_image_output_tokens: 0,
        window_image_request_count: 0,
        window_image_request_failed_count: 0,
        window_started_at: None,
        window_reset_at: None,
        limit_window_seconds: None,
        added_at: stored.added_at.to_rfc3339(),
        last_used_at: None,
    }
}
