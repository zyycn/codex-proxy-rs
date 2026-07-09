//! 运行时 token refresh 业务服务。

use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    future::Future,
    hash::{Hash, Hasher},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::{sync::Mutex, task::JoinHandle, time::sleep};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::upstream::accounts::{
    model::{Account, AccountStatus},
    store::{
        AccountClaimsUpdate, AccountStore, SqliteAccountStore, SqliteAccountStoreError,
        StoredAccount,
    },
    token_refresh::{
        jittered_refresh_at, jwt_expiration, RefreshError, RefreshLeaseStore,
        RefreshLeaseStoreError, RefreshPolicy, RefreshScheduler, RefreshTrigger,
        RuntimeRefreshPolicy, TokenRefresher,
    },
};

const REFRESH_LEASE_TTL_SECONDS: i64 = 5 * 60;
const MAX_REFRESH_ATTEMPTS: usize = 5;
const REFRESH_RETRY_BASE_DELAY_MILLIS: u64 = 5_000;
const REFRESH_RETRY_MAX_DELAY_MILLIS: u64 = 300_000;
const PERMANENT_FAILURE_CONFIRMATION_THRESHOLD: usize = 2;
const RECOVERY_DELAY_SECONDS: i64 = 10 * 60;
const RETRY_DELAY_JITTER: f64 = 0.30;
const RECOVERY_DELAY_JITTER: f64 = 0.20;

/// 运行时 token refresh 服务。
pub struct RuntimeTokenRefreshService<C>
where
    C: TokenRefresher,
{
    store: SqliteAccountStore,
    scheduler: Arc<RefreshScheduler<C>>,
    policy: RuntimeRefreshPolicy,
    refresh_leases: Option<RefreshLeaseStore>,
    lease_owner: String,
    retry_delays: Vec<Duration>,
    in_flight: Arc<Mutex<HashSet<String>>>,
    timers: Arc<Mutex<HashMap<String, ScheduledRefreshTimer>>>,
}

struct ScheduledRefreshTimer {
    scheduled_at: DateTime<Utc>,
    trigger: RefreshTrigger,
    handle: JoinHandle<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduleTimerResult {
    Scheduled,
    Replaced,
    Unchanged,
}

impl<C> Clone for RuntimeTokenRefreshService<C>
where
    C: TokenRefresher,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            scheduler: self.scheduler.clone(),
            policy: self.policy.clone(),
            refresh_leases: self.refresh_leases.clone(),
            lease_owner: self.lease_owner.clone(),
            retry_delays: self.retry_delays.clone(),
            in_flight: self.in_flight.clone(),
            timers: self.timers.clone(),
        }
    }
}

impl<C> RuntimeTokenRefreshService<C>
where
    C: TokenRefresher,
{
    /// 构造运行时 token refresh 服务。
    pub fn new(
        store: SqliteAccountStore,
        policy: impl Into<RuntimeRefreshPolicy>,
        client: C,
    ) -> Self {
        let policy = policy.into();
        Self {
            store,
            scheduler: Arc::new(RefreshScheduler::new(policy.clone(), client)),
            policy,
            refresh_leases: None,
            lease_owner: refresh_lease_owner(),
            retry_delays: default_refresh_retry_delays(),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            timers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 使用刷新租约存储保护账号刷新。
    pub fn with_refresh_lease_store(mut self, refresh_leases: RefreshLeaseStore) -> Self {
        self.refresh_leases = Some(refresh_leases);
        self
    }

    /// 更新刷新策略。
    pub fn update_policy(&self, policy: RefreshPolicy) {
        self.policy.update(policy);
    }

    /// 使用自定义刷新重试延迟。
    pub fn with_retry_delays<I>(mut self, retry_delays: I) -> Self
    where
        I: IntoIterator<Item = Duration>,
    {
        self.retry_delays = retry_delays.into_iter().collect();
        self
    }

    /// 返回给定账号集合中当前正在执行 token 刷新的账号 ID。
    pub async fn refreshing_account_ids(
        &self,
        account_ids: &[String],
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<HashSet<String>> {
        if account_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let requested_ids = account_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut refreshing = self
            .in_flight
            .lock()
            .await
            .iter()
            .filter(|account_id| requested_ids.contains(account_id.as_str()))
            .cloned()
            .collect::<HashSet<_>>();

        if let Some(refresh_leases) = self.refresh_leases.as_ref() {
            refreshing.extend(
                refresh_leases
                    .active_account_ids(account_ids, now)
                    .await
                    .map_err(TokenRefreshServiceError::Lease)?,
            );
        }

        Ok(refreshing)
    }

    /// 执行一次账号级刷新定时器调度。
    pub async fn schedule_account_timers_once(
        &self,
    ) -> TokenRefreshServiceResult<TokenTimerSummary> {
        self.schedule_account_timers_once_at(Utc::now()).await
    }

    /// 在指定时间点执行一次账号级刷新定时器调度。
    pub async fn schedule_account_timers_once_at(
        &self,
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<TokenTimerSummary> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(TokenRefreshServiceError::AccountStore)?;
        let mut summary = TokenTimerSummary {
            scanned: accounts.len(),
            ..TokenTimerSummary::default()
        };
        let mut expired_recovery_index = 0;

        for account in accounts {
            if self.skip_account_without_refresh_token(&account).await? {
                summary.skipped += 1;
                continue;
            }

            if matches!(
                account.status,
                AccountStatus::Disabled | AccountStatus::Banned
            ) {
                summary.skipped += 1;
                continue;
            }

            let timer = match account.status {
                AccountStatus::Expired => {
                    let delay = match account.next_refresh_at {
                        Some(next_refresh_at) if next_refresh_at > now => {
                            duration_until(now, next_refresh_at)
                        }
                        _ => Duration::from_secs(30 + expired_recovery_index * 2),
                    };
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
            self.persist_next_refresh_at_if_changed(&account, Some(scheduled_at))
                .await?;
            let schedule_result = self
                .schedule_account_timer(account.id.clone(), delay, trigger, scheduled_at)
                .await;
            match schedule_result {
                ScheduleTimerResult::Unchanged => {
                    summary.skipped += 1;
                    continue;
                }
                ScheduleTimerResult::Replaced => summary.replaced += 1,
                ScheduleTimerResult::Scheduled => {}
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

    /// 立即刷新被上游认证失败命中的账号。
    pub async fn trigger_account_refresh_now(
        &self,
        account_id: &str,
    ) -> TokenRefreshServiceResult<()> {
        let Some(account) = self.store.get(account_id).await? else {
            return Err(TokenRefreshServiceError::AccountNotFound(
                account_id.to_string(),
            ));
        };
        if account.refresh_token.is_none() {
            self.clear_scheduled_timer(account_id).await;
            if account.next_refresh_at.is_some() {
                persist_next_refresh_at(&self.store, account_id, None).await?;
            }
            return Ok(());
        }

        if matches!(
            account.status,
            AccountStatus::Disabled | AccountStatus::Banned
        ) {
            return Ok(());
        }

        self.clear_scheduled_timer(account_id).await;
        persist_next_refresh_at(&self.store, account_id, None).await?;
        self.refresh_scheduled_account(account_id, RefreshTrigger::Unauthorized, Utc::now())
            .await
            .map(|_| ())
    }

    /// 在指定时间点执行一次到期刷新扫描。
    pub async fn refresh_due_accounts_once_at(
        &self,
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<TokenRefreshSummary> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(TokenRefreshServiceError::AccountStore)?;
        let mut summary = TokenRefreshSummary {
            scanned: accounts.len(),
            ..TokenRefreshSummary::default()
        };

        for account in accounts {
            if self.skip_account_without_refresh_token(&account).await? {
                summary.skipped += 1;
                continue;
            }

            if account.next_refresh_at.is_some_and(|value| value > now) {
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
        if let Some(next_refresh_at) = account.next_refresh_at.filter(|value| *value > now) {
            return Some(duration_until(now, next_refresh_at));
        }

        let refresh_at = self.computed_refresh_at(account)?;

        if refresh_at <= now {
            return Some(Duration::ZERO);
        }

        (refresh_at - now).to_std().ok()
    }

    async fn skip_account_without_refresh_token(
        &self,
        account: &Account,
    ) -> TokenRefreshServiceResult<bool> {
        if account.refresh_token.is_some() {
            return Ok(false);
        }

        self.clear_scheduled_timer(&account.id).await;
        if account.next_refresh_at.is_some() {
            self.persist_next_refresh_at_if_changed(account, None)
                .await?;
        }
        Ok(true)
    }

    fn computed_refresh_at(&self, account: &Account) -> Option<DateTime<Utc>> {
        let expires_at = account
            .access_token_expires_at
            .or_else(|| jwt_expiration(&account.access_token))?;
        Some(jittered_refresh_at(
            &account.id,
            expires_at,
            self.policy.refresh_margin_seconds(),
        ))
    }

    async fn persist_next_refresh_at_if_changed(
        &self,
        account: &Account,
        next_refresh_at: Option<DateTime<Utc>>,
    ) -> TokenRefreshServiceResult<()> {
        if same_refresh_time(account.next_refresh_at, next_refresh_at) {
            return Ok(());
        }
        persist_next_refresh_at(&self.store, &account.id, next_refresh_at).await
    }

    fn schedule_account_timer(
        &self,
        account_id: String,
        delay: Duration,
        trigger: RefreshTrigger,
        scheduled_at: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = ScheduleTimerResult> + Send + '_>> {
        Box::pin(async move {
            {
                let timers = self.timers.lock().await;
                if timers.get(&account_id).is_some_and(|timer| {
                    timer.scheduled_at == scheduled_at && timer.trigger == trigger
                }) {
                    return ScheduleTimerResult::Unchanged;
                }
            }

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

            self.timers.lock().await.insert(
                account_id,
                ScheduledRefreshTimer {
                    scheduled_at,
                    trigger,
                    handle,
                },
            );
            if replaced {
                ScheduleTimerResult::Replaced
            } else {
                ScheduleTimerResult::Scheduled
            }
        })
    }

    async fn clear_scheduled_timer(&self, account_id: &str) -> bool {
        if let Some(timer) = self.timers.lock().await.remove(account_id) {
            timer.handle.abort();
            true
        } else {
            false
        }
    }

    /// 清理全部已调度的账号级刷新定时器。
    pub async fn clear_scheduled_timers(&self) {
        let mut timers = self.timers.lock().await;
        for (_, timer) in timers.drain() {
            timer.handle.abort();
        }
    }

    async fn refresh_scheduled_account(
        &self,
        account_id: &str,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<TokenRefreshOutcome> {
        let Some(account) = self.store.get(account_id).await? else {
            return Err(TokenRefreshServiceError::AccountNotFound(
                account_id.to_string(),
            ));
        };
        let account = stored_account_to_refresh_account(account);
        if self.skip_account_without_refresh_token(&account).await? {
            return Ok(TokenRefreshOutcome::Skipped);
        }

        if account.next_refresh_at.is_some_and(|value| value > now) {
            return Ok(TokenRefreshOutcome::Skipped);
        }

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
        if account.status == AccountStatus::Expired {
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
    ) -> TokenRefreshServiceResult<TokenRefreshOutcome> {
        let max_attempts = self.retry_delays.len() + 1;
        let mut permanent_failures = 0;

        for attempt_index in 0..max_attempts {
            let Some(attempt) = self.prepare_refresh_attempt(account, trigger, now).await? else {
                return Ok(TokenRefreshOutcome::Skipped);
            };

            if attempt.refresh_token.is_none() {
                persist_status(&self.store, &attempt.id, AccountStatus::Expired).await?;
                persist_next_refresh_at(&self.store, &attempt.id, None).await?;
                return Ok(TokenRefreshOutcome::StatusUpdated);
            }

            match self
                .scheduler
                .refresh_account_at(&attempt, trigger, now)
                .await
            {
                Ok(updated) if is_permanent_refresh_failure_status(updated.status) => {
                    permanent_failures += 1;
                    if permanent_failures < PERMANENT_FAILURE_CONFIRMATION_THRESHOLD
                        && self.sleep_before_retry(&attempt.id, attempt_index).await
                    {
                        continue;
                    }

                    persist_status(&self.store, &attempt.id, updated.status).await?;
                    persist_next_refresh_at(&self.store, &attempt.id, None).await?;
                    return Ok(TokenRefreshOutcome::StatusUpdated);
                }
                Ok(mut updated) => {
                    updated.access_token_expires_at =
                        jwt_expiration(&updated.access_token).or(updated.access_token_expires_at);
                    updated.next_refresh_at = self.computed_refresh_at(&updated);
                    let refresh_token_rotated = updated.refresh_token != attempt.refresh_token;
                    persist_token_update(&self.store, &updated).await?;
                    info!(
                        account_id = %updated.id,
                        trigger = ?trigger,
                        attempt = attempt_index + 1,
                        status = %updated.status,
                        access_token_expires_at = ?updated.access_token_expires_at,
                        next_refresh_at = ?updated.next_refresh_at,
                        refresh_token_present = updated.refresh_token.is_some(),
                        refresh_token_rotated,
                        "token refresh succeeded"
                    );
                    return Ok(TokenRefreshOutcome::Refreshed(Box::new(updated)));
                }
                Err(RefreshError::RetryableTransport | RefreshError::Transport)
                    if self.sleep_before_retry(&attempt.id, attempt_index).await => {}
                Err(RefreshError::RetryableTransport | RefreshError::Transport) => {
                    persist_status(
                        &self.store,
                        &attempt.id,
                        status_after_temporary_refresh_failure(attempt.status),
                    )
                    .await?;
                    let recovery_delay = stable_jittered_duration(
                        &attempt.id,
                        Duration::from_secs(RECOVERY_DELAY_SECONDS as u64),
                        RECOVERY_DELAY_JITTER,
                        "recovery",
                    );
                    let next_refresh_at = scheduled_at_from_delay(now, recovery_delay);
                    persist_next_refresh_at(&self.store, &attempt.id, Some(next_refresh_at))
                        .await?;
                    return Ok(TokenRefreshOutcome::Failed);
                }
                Err(error) => return Err(TokenRefreshServiceError::Refresh(error)),
            }
        }

        Ok(TokenRefreshOutcome::Skipped)
    }

    async fn prepare_refresh_attempt(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<Option<Account>> {
        let Some(stored) = self.store.get(&account.id).await? else {
            return Err(TokenRefreshServiceError::AccountNotFound(
                account.id.clone(),
            ));
        };
        let attempt = stored_account_to_refresh_account(stored);
        if matches!(
            attempt.status,
            AccountStatus::Disabled | AccountStatus::Banned
        ) || attempt.next_refresh_at.is_some_and(|value| value > now)
            || attempt.refresh_token != account.refresh_token
        {
            return Ok(None);
        }

        if !self
            .scheduler
            .should_refresh_account_at(&attempt, trigger, now)
        {
            return Ok(None);
        }

        Ok(Some(attempt))
    }

    async fn sleep_before_retry(&self, account_id: &str, attempt_index: usize) -> bool {
        let Some(delay) = self.retry_delays.get(attempt_index).copied() else {
            return false;
        };

        let delay = stable_jittered_duration(
            account_id,
            delay,
            RETRY_DELAY_JITTER,
            &format!("retry:{attempt_index}"),
        );
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

    async fn try_acquire_refresh_lease(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<bool> {
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
            .map_err(TokenRefreshServiceError::Lease)
    }

    async fn release_refresh_lease(&self, account_id: &str) -> TokenRefreshServiceResult<()> {
        let Some(refresh_leases) = self.refresh_leases.as_ref() else {
            return Ok(());
        };
        refresh_leases
            .release(account_id, &self.lease_owner)
            .await
            .map(|_| ())
            .map_err(TokenRefreshServiceError::Lease)
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
    /// 返回本轮调度发生变化的定时器数量。
    pub fn changed(&self) -> usize {
        self.scheduled + self.immediate + self.recovery_scheduled
    }
}

/// 令牌刷新任务错误。
#[derive(Debug, Error)]
pub enum TokenRefreshServiceError {
    /// 账号存储读取失败。
    #[error("failed to list accounts for token refresh: {0}")]
    AccountStore(#[from] crate::upstream::accounts::store::AccountStoreError),
    /// 账号存储写入失败。
    #[error("failed to persist token refresh result: {0}")]
    Store(#[from] SqliteAccountStoreError),
    /// 刷新租约存储失败。
    #[error("failed to coordinate token refresh lease: {0}")]
    Lease(#[from] RefreshLeaseStoreError),
    /// core 刷新调度失败。
    #[error("token refresh scheduler failed: {0}")]
    Refresh(#[from] RefreshError),
    /// 刷新期间账号不存在。
    #[error("account disappeared during token refresh: {0}")]
    AccountNotFound(String),
}

/// 令牌刷新任务结果。
pub type TokenRefreshServiceResult<T> = Result<T, TokenRefreshServiceError>;

async fn persist_token_update(
    store: &SqliteAccountStore,
    account: &Account,
) -> TokenRefreshServiceResult<()> {
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
                next_refresh_at: account.next_refresh_at,
                status: account.status,
            },
        )
        .await?;
    if updated {
        Ok(())
    } else {
        Err(TokenRefreshServiceError::AccountNotFound(
            account.id.clone(),
        ))
    }
}

async fn persist_status(
    store: &SqliteAccountStore,
    account_id: &str,
    status: AccountStatus,
) -> TokenRefreshServiceResult<()> {
    let updated = store.set_status(account_id, status).await?;
    if updated {
        Ok(())
    } else {
        Err(TokenRefreshServiceError::AccountNotFound(
            account_id.to_string(),
        ))
    }
}

async fn persist_next_refresh_at(
    store: &SqliteAccountStore,
    account_id: &str,
    next_refresh_at: Option<DateTime<Utc>>,
) -> TokenRefreshServiceResult<()> {
    let updated = store
        .set_next_refresh_at(account_id, next_refresh_at)
        .await?;
    if updated {
        Ok(())
    } else {
        Err(TokenRefreshServiceError::AccountNotFound(
            account_id.to_string(),
        ))
    }
}

fn is_permanent_refresh_failure_status(status: AccountStatus) -> bool {
    matches!(status, AccountStatus::Expired | AccountStatus::Banned)
}

fn status_after_temporary_refresh_failure(status: AccountStatus) -> AccountStatus {
    match status {
        AccountStatus::Expired | AccountStatus::QuotaExhausted => status,
        AccountStatus::Active => AccountStatus::Active,
        AccountStatus::Disabled | AccountStatus::Banned => status,
    }
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

fn stable_jittered_duration(
    account_id: &str,
    base: Duration,
    variance: f64,
    salt: &str,
) -> Duration {
    if base.is_zero() {
        return Duration::ZERO;
    }

    let mut hasher = DefaultHasher::new();
    account_id.hash(&mut hasher);
    salt.hash(&mut hasher);
    let unit = hasher.finish() as f64 / u64::MAX as f64;
    let factor = (1.0 - variance) + unit * variance * 2.0;
    let millis = (base.as_millis() as f64 * factor)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64;
    Duration::from_millis(millis)
}

fn scheduled_at_from_delay(now: DateTime<Utc>, delay: Duration) -> DateTime<Utc> {
    match chrono::Duration::from_std(delay) {
        Ok(delay) => now + delay,
        Err(_) => now,
    }
}

fn duration_until(now: DateTime<Utc>, scheduled_at: DateTime<Utc>) -> Duration {
    (scheduled_at - now).to_std().unwrap_or(Duration::ZERO)
}

fn same_refresh_time(left: Option<DateTime<Utc>>, right: Option<DateTime<Utc>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.timestamp() == right.timestamp(),
        (None, None) => true,
        _ => false,
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
        next_refresh_at: stored.next_refresh_at,
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
        added_at: stored.added_at,
        last_used_at: None,
    }
}
