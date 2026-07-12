//! 运行时 token refresh 业务服务。

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::{sync::Mutex, task::JoinHandle, time::sleep};
use tokio_util::task::TaskTracker;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::fleet::{
    account::{Account, AccountStatus},
    store::{
        AccountClaimsUpdate, AccountStore, PgAccountStore, PgAccountStoreError, StoredAccount,
    },
};
use crate::upstream::openai::token_client::TokenRefresher;

use super::{
    lease::{RedisRefreshLeaseStore, RedisRefreshLeaseStoreError},
    policy::{
        default_refresh_retry_delays, duration_until, is_permanent_refresh_failure_status,
        jittered_refresh_at, jwt_expiration, same_refresh_time, scheduled_at_from_delay,
        stable_jittered_duration, token_refresh_status_eligible, RefreshError, RefreshPolicy,
        RefreshScheduler, RuntimeRefreshPolicy, TokenTimerSummary,
        PERMANENT_FAILURE_CONFIRMATION_THRESHOLD, RECOVERY_DELAY_JITTER, RECOVERY_DELAY_SECONDS,
        RETRY_DELAY_JITTER,
    },
};

const REFRESH_LEASE_TTL_SECONDS: i64 = 5 * 60;
const REFRESH_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(4);

/// 运行时 token refresh 服务。
pub struct TokenRefreshService<C>
where
    C: TokenRefresher,
{
    store: PgAccountStore,
    scheduler: Arc<RefreshScheduler<C>>,
    policy: RuntimeRefreshPolicy,
    refresh_leases: Option<RedisRefreshLeaseStore>,
    lease_owner: String,
    retry_delays: Vec<Duration>,
    in_flight: Arc<StdMutex<HashSet<String>>>,
    timers: Arc<Mutex<HashMap<String, ScheduledRefreshTimer>>>,
    tasks: TaskTracker,
    shutting_down: Arc<AtomicBool>,
}

struct ScheduledRefreshTimer {
    id: Uuid,
    scheduled_at: DateTime<Utc>,
    handle: JoinHandle<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduleTimerResult {
    Scheduled,
    Replaced,
    Unchanged,
    ShuttingDown,
}

struct InFlightRefreshGuard {
    in_flight: Arc<StdMutex<HashSet<String>>>,
    account_id: String,
}

impl Drop for InFlightRefreshGuard {
    fn drop(&mut self) {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.account_id);
    }
}

struct RefreshLeaseGuard {
    store: Option<RedisRefreshLeaseStore>,
    tasks: Option<TaskTracker>,
    account_id: String,
    owner: String,
    armed: bool,
}

impl RefreshLeaseGuard {
    fn local(account_id: &str, owner: &str) -> Self {
        Self {
            store: None,
            tasks: None,
            account_id: account_id.to_string(),
            owner: owner.to_string(),
            armed: false,
        }
    }

    fn distributed(
        store: RedisRefreshLeaseStore,
        tasks: TaskTracker,
        account_id: &str,
        owner: &str,
    ) -> Self {
        Self {
            store: Some(store),
            tasks: Some(tasks),
            account_id: account_id.to_string(),
            owner: owner.to_string(),
            armed: true,
        }
    }

    async fn release(mut self) -> Result<(), RedisRefreshLeaseStoreError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        store.release(&self.account_id, &self.owner).await?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for RefreshLeaseGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (Some(store), Some(tasks), Ok(runtime)) = (
            self.store.clone(),
            self.tasks.clone(),
            tokio::runtime::Handle::try_current(),
        ) else {
            return;
        };
        let account_id = self.account_id.clone();
        let owner = self.owner.clone();
        drop(tasks.spawn_on(
            async move {
                if let Err(error) = store.release(&account_id, &owner).await {
                    warn!(account_id, error = %error, "failed to release cancelled token refresh lease");
                }
            },
            &runtime,
        ));
    }
}

enum TokenRefreshOutcome {
    Refreshed(Box<Account>),
    StatusUpdated,
    Skipped,
    Failed,
}

/// 令牌刷新任务错误。
#[derive(Debug, Error)]
pub enum TokenRefreshServiceError {
    /// 账号存储读取失败。
    #[error("failed to list accounts for token refresh: {0}")]
    AccountStore(#[from] crate::fleet::store::AccountStoreError),
    /// 账号存储写入失败。
    #[error("failed to persist token refresh result: {0}")]
    Store(#[from] PgAccountStoreError),
    /// 刷新租约存储失败。
    #[error("failed to coordinate token refresh lease: {0}")]
    Lease(#[from] RedisRefreshLeaseStoreError),
    /// core 刷新调度失败。
    #[error("token refresh scheduler failed: {0}")]
    Refresh(#[from] RefreshError),
    /// 刷新期间账号不存在。
    #[error("account disappeared during token refresh: {0}")]
    AccountNotFound(String),
}

/// 令牌刷新任务结果。
pub type TokenRefreshServiceResult<T> = Result<T, TokenRefreshServiceError>;

impl<C> Clone for TokenRefreshService<C>
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
            tasks: self.tasks.clone(),
            shutting_down: self.shutting_down.clone(),
        }
    }
}

impl<C> TokenRefreshService<C>
where
    C: TokenRefresher,
{
    /// 构造运行时 token refresh 服务。
    pub fn new(store: PgAccountStore, policy: impl Into<RuntimeRefreshPolicy>, client: C) -> Self {
        let policy = policy.into();
        Self {
            store,
            scheduler: Arc::new(RefreshScheduler::new(policy.clone(), client)),
            policy,
            refresh_leases: None,
            lease_owner: refresh_lease_owner(),
            retry_delays: default_refresh_retry_delays(),
            in_flight: Arc::new(StdMutex::new(HashSet::new())),
            timers: Arc::new(Mutex::new(HashMap::new())),
            tasks: TaskTracker::new(),
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 使用刷新租约存储保护账号刷新。
    pub fn with_refresh_lease_store(mut self, refresh_leases: RedisRefreshLeaseStore) -> Self {
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        if self.shutting_down.load(Ordering::Acquire) {
            return Ok(TokenTimerSummary::default());
        }
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(TokenRefreshServiceError::AccountStore)?;
        let mut summary = TokenTimerSummary {
            scanned: accounts.len(),
            ..TokenTimerSummary::default()
        };
        for account in accounts {
            if self.clear_refresh_schedule_if_ineligible(&account).await? {
                summary.skipped += 1;
                continue;
            }

            let Some(delay) = self.timer_delay_for_account(&account, now) else {
                summary.skipped += 1;
                continue;
            };
            let scheduled_at = scheduled_at_from_delay(now, delay);
            self.persist_next_refresh_at_if_changed(&account, Some(scheduled_at))
                .await?;
            let schedule_result = self
                .schedule_account_timer(account.id.clone(), delay, scheduled_at)
                .await;
            match schedule_result {
                ScheduleTimerResult::Unchanged => {
                    summary.skipped += 1;
                    continue;
                }
                ScheduleTimerResult::Replaced => summary.replaced += 1,
                ScheduleTimerResult::Scheduled => {}
                ScheduleTimerResult::ShuttingDown => {
                    summary.skipped += 1;
                    continue;
                }
            }

            if delay.is_zero() {
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

    async fn clear_refresh_schedule_if_ineligible(
        &self,
        account: &Account,
    ) -> TokenRefreshServiceResult<bool> {
        if account.refresh_token.is_some() && token_refresh_status_eligible(account.status) {
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
        scheduled_at: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = ScheduleTimerResult> + Send + '_>> {
        Box::pin(async move {
            let mut timers = self.timers.lock().await;
            if self.shutting_down.load(Ordering::Acquire) {
                return ScheduleTimerResult::ShuttingDown;
            }
            if timers
                .get(&account_id)
                .is_some_and(|timer| timer.scheduled_at == scheduled_at)
            {
                return ScheduleTimerResult::Unchanged;
            }
            let replaced = timers.remove(&account_id).is_some_and(|timer| {
                timer.handle.abort();
                true
            });
            let task = self.clone();
            let timer_account_id = account_id.clone();
            let timer_id = Uuid::new_v4();
            let handle = self.tasks.spawn(async move {
                if !delay.is_zero() {
                    sleep(delay).await;
                }
                let owns_timer = {
                    let mut timers = task.timers.lock().await;
                    if timers
                        .get(&timer_account_id)
                        .is_some_and(|timer| timer.id == timer_id)
                    {
                        timers.remove(&timer_account_id);
                        true
                    } else {
                        false
                    }
                };
                if !owns_timer || task.shutting_down.load(Ordering::Acquire) {
                    return;
                }
                if let Err(error) = task
                    .refresh_scheduled_account(&timer_account_id, scheduled_at)
                    .await
                {
                    warn!(
                        account_id = %timer_account_id,
                        error = %error,
                        "scheduled token refresh failed"
                    );
                }
            });

            timers.insert(
                account_id,
                ScheduledRefreshTimer {
                    id: timer_id,
                    scheduled_at,
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
        let timer = self.timers.lock().await.remove(account_id);
        if let Some(timer) = timer {
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

    /// 停止新调度，取消尚未触发的 timer，并等待已进入刷新流程的任务结束。
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.clear_scheduled_timers().await;
        self.tasks.close();
        if tokio::time::timeout(REFRESH_TASK_SHUTDOWN_TIMEOUT, self.tasks.wait())
            .await
            .is_err()
        {
            warn!(
                remaining_tasks = self.tasks.len(),
                timeout_secs = REFRESH_TASK_SHUTDOWN_TIMEOUT.as_secs(),
                "waiting for in-flight token refresh tasks timed out"
            );
        }
    }

    async fn refresh_scheduled_account(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<TokenRefreshOutcome> {
        let Some(account) = self.store.get(account_id).await? else {
            return Ok(TokenRefreshOutcome::Skipped);
        };
        let account = stored_account_to_refresh_account(account);
        if self.clear_refresh_schedule_if_ineligible(&account).await? {
            return Ok(TokenRefreshOutcome::Skipped);
        }

        if account.next_refresh_at.is_some_and(|value| value > now) {
            return Ok(TokenRefreshOutcome::Skipped);
        }

        let Some(_in_flight) = self.try_mark_in_flight(&account.id) else {
            return Ok(TokenRefreshOutcome::Skipped);
        };

        let Some(refresh_lease) = self.try_acquire_refresh_lease(&account.id, now).await? else {
            return Ok(TokenRefreshOutcome::Skipped);
        };

        let outcome = self
            .refresh_account_with_status_transitions(&account, now)
            .await;

        refresh_lease.release().await?;

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
        self.schedule_account_timer(account.id.clone(), delay, scheduled_at)
            .await;
    }

    async fn refresh_account_with_status_transitions(
        &self,
        account: &Account,
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<TokenRefreshOutcome> {
        let max_attempts = self.retry_delays.len() + 1;
        let mut permanent_failures = 0;

        for attempt_index in 0..max_attempts {
            let Some(attempt) = self.prepare_refresh_attempt(account, now).await? else {
                return Ok(TokenRefreshOutcome::Skipped);
            };

            if attempt.refresh_token.is_none() {
                persist_status(&self.store, &attempt.id, AccountStatus::Expired).await?;
                persist_next_refresh_at(&self.store, &attempt.id, None).await?;
                return Ok(TokenRefreshOutcome::StatusUpdated);
            }

            match self.scheduler.refresh_account_at(&attempt, now).await {
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
                    persist_status(&self.store, &attempt.id, attempt.status).await?;
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
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<Option<Account>> {
        let Some(stored) = self.store.get(&account.id).await? else {
            return Err(TokenRefreshServiceError::AccountNotFound(
                account.id.clone(),
            ));
        };
        let attempt = stored_account_to_refresh_account(stored);
        if !token_refresh_status_eligible(attempt.status)
            || attempt.next_refresh_at.is_some_and(|value| value > now)
            || attempt.refresh_token != account.refresh_token
        {
            return Ok(None);
        }

        if !self.scheduler.should_refresh_account_at(&attempt, now) {
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

    fn try_mark_in_flight(&self, account_id: &str) -> Option<InFlightRefreshGuard> {
        let inserted = self
            .in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(account_id.to_string());
        inserted.then(|| InFlightRefreshGuard {
            in_flight: self.in_flight.clone(),
            account_id: account_id.to_string(),
        })
    }

    async fn try_acquire_refresh_lease(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> TokenRefreshServiceResult<Option<RefreshLeaseGuard>> {
        let Some(refresh_leases) = self.refresh_leases.as_ref() else {
            return Ok(Some(RefreshLeaseGuard::local(
                account_id,
                &self.lease_owner,
            )));
        };
        let acquired = refresh_leases
            .try_acquire(
                account_id,
                &self.lease_owner,
                now + chrono::Duration::seconds(REFRESH_LEASE_TTL_SECONDS),
                now,
            )
            .await
            .map_err(TokenRefreshServiceError::Lease)?;
        Ok(acquired.then(|| {
            RefreshLeaseGuard::distributed(
                refresh_leases.clone(),
                self.tasks.clone(),
                account_id,
                &self.lease_owner,
            )
        }))
    }
}

async fn persist_token_update(
    store: &PgAccountStore,
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
    store: &PgAccountStore,
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
    store: &PgAccountStore,
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

fn refresh_lease_owner() -> String {
    format!("runtime-token-refresh:{}", Uuid::new_v4().simple())
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
