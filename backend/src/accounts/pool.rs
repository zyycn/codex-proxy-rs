//! 账号池调度策略。

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Utc};
use indexmap::IndexMap;
use thiserror::Error;

use crate::accounts::quota::{quota_snapshot_limit_reached, quota_snapshot_reset_at};
use crate::accounts::refresh::{jwt_expiry, JwtExpiry};
use crate::accounts::scheduler::{
    candidates::{self, CandidateFilter, CandidateRequest},
    AccountScheduler, ScoreWeights,
};
use crate::accounts::store::AccountStore;
use crate::accounts::{
    account::{Account, AccountStatus},
    window::should_reset_usage_window,
};
use crate::telemetry::account_usage::store::{
    AccountModelUsageDelta, AccountUsageDelta, AccountUsageSnapshot, AccountUsageStore,
    AccountUsageWindow,
};
use crate::upstream::openai::protocol::events::{
    parse_rate_limit_headers, rate_limit_quota, TokenUsage as CodexTokenUsage,
};

pub use crate::accounts::scheduler::RotationStrategy;

mod filters;
mod state;

pub use filters::*;
pub use state::*;

/// 账号运行参数中不支持热更新的部分。
#[derive(Debug, Clone)]
pub struct AccountPoolStaticSettings {
    pub skip_quota_limited: bool,
    pub tier_priority: Vec<String>,
}

impl AccountPoolStaticSettings {
    /// 将设置快照转换为账号池选项。
    pub fn pool_options(&self, settings: &crate::settings::SettingsSnapshot) -> AccountPoolOptions {
        AccountPoolOptions {
            max_concurrent_per_account: settings.max_concurrent_per_account,
            rotation_strategy: match settings.rotation_strategy.as_str() {
                "smart" => RotationStrategy::Smart,
                "quota_reset_priority" => RotationStrategy::QuotaResetPriority,
                "round_robin" => RotationStrategy::RoundRobin,
                "sticky" => RotationStrategy::Sticky,
                _ => RotationStrategy::Smart,
            },
            skip_quota_limited: self.skip_quota_limited,
            tier_priority: self.tier_priority.clone(),
            ..AccountPoolOptions::default()
        }
    }
}

#[derive(Clone)]
pub struct RuntimeAccountPoolService {
    pool: Arc<tokio::sync::Mutex<AccountPool>>,
    store: Arc<dyn AccountStore>,
    usage_store: Arc<dyn AccountUsageStore>,
    request_interval: Arc<std::sync::RwLock<StdDuration>>,
}

impl RuntimeAccountPoolService {
    /// 构造运行时账号池服务。
    pub fn new(
        store: Arc<dyn AccountStore>,
        usage_store: Arc<dyn AccountUsageStore>,
        options: AccountPoolOptions,
        request_interval_ms: u64,
    ) -> Self {
        Self {
            pool: Arc::new(tokio::sync::Mutex::new(AccountPool::with_options(options))),
            store,
            usage_store,
            request_interval: Arc::new(std::sync::RwLock::new(StdDuration::from_millis(
                request_interval_ms,
            ))),
        }
    }

    /// 更新账号池运行参数。
    pub async fn apply_options(&self, mut options: AccountPoolOptions, request_interval_ms: u64) {
        let mut pool = self.pool.lock().await;
        options.model_plan_allowlist = pool.options.model_plan_allowlist.clone();
        options.fetched_model_plan_types = pool.options.fetched_model_plan_types.clone();
        pool.set_options(options);
        *self
            .request_interval
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            StdDuration::from_millis(request_interval_ms);
    }

    /// 持续接收运行时设置并更新账号池参数。
    pub async fn subscribe_settings(
        self: Arc<Self>,
        mut receiver: tokio::sync::watch::Receiver<crate::settings::SettingsSnapshot>,
        static_settings: AccountPoolStaticSettings,
    ) {
        while receiver.changed().await.is_ok() {
            let settings = receiver.borrow_and_update().clone();
            self.apply_options(
                static_settings.pool_options(&settings),
                settings.request_interval_ms,
            )
            .await;
        }
    }

    /// 更新模型到可用账号计划的调度约束。
    pub(crate) async fn apply_model_plan_routing(
        &self,
        model_plan_allowlist: BTreeMap<String, Vec<String>>,
        fetched_model_plan_types: BTreeSet<String>,
    ) {
        let mut pool = self.pool.lock().await;
        pool.options.model_plan_allowlist = model_plan_allowlist;
        pool.options.fetched_model_plan_types = fetched_model_plan_types;
    }

    /// 从持久化账号恢复运行时账号池。
    pub async fn restore_from_store(&self) -> Result<usize, RuntimeAccountPoolError> {
        let mut accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(|_| RuntimeAccountPoolError::Generic)?;
        let account_ids = accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let mut usage = self
            .usage_store
            .snapshots(&account_ids)
            .await
            .map_err(|_| RuntimeAccountPoolError::Generic)?;
        for account in &mut accounts {
            if let Some(snapshot) = usage.remove(&account.id) {
                apply_usage_snapshot(account, snapshot);
            }
        }
        let count = accounts.len();
        let mut pool = self.pool.lock().await;
        pool.clear();
        for account in accounts {
            pool.insert(account);
        }
        Ok(count)
    }

    /// 读取账号池容量摘要。
    pub async fn capacity_summary(&self, now: DateTime<Utc>) -> AccountCapacitySummary {
        let refresh = self
            .pool
            .lock()
            .await
            .capacity_summary_with_status_refresh(now);
        self.persist_runtime_account_states(refresh.refreshed_accounts)
            .await;
        refresh.summary
    }

    /// 使用当前时间读取账号池容量摘要。
    pub async fn capacity_summary_now(&self) -> AccountCapacitySummary {
        self.capacity_summary(Utc::now()).await
    }

    /// 释放账号在途槽位。
    pub async fn release(&self, account_id: &str) {
        self.release_slot(account_id, true).await;
    }

    /// 释放账号在途槽位，不累计请求用量。
    pub async fn release_without_request_usage(&self, account_id: &str) {
        self.release_slot(account_id, false).await;
    }

    async fn release_slot(&self, account_id: &str, record_request_usage: bool) {
        let released = if record_request_usage {
            self.pool.lock().await.release(account_id)
        } else {
            self.pool
                .lock()
                .await
                .release_without_request_usage(account_id)
        };
        let Some(released) = released else {
            return;
        };
        if !record_request_usage {
            return;
        }
        if let Err(error) = self.usage_store.record_request(account_id).await {
            tracing::warn!(
                account_id,
                error = %error,
                "failed to persist account request usage"
            );
        }
        let Some(model) = released.model else {
            tracing::debug!(
                account_id,
                "released account without active slot; skipped model request usage persistence"
            );
            return;
        };
        if let Err(error) = self
            .usage_store
            .record_model_usage_delta(
                account_id,
                &model,
                AccountModelUsageDelta {
                    requests: 1,
                    ..AccountModelUsageDelta::default()
                },
            )
            .await
        {
            tracing::warn!(
                account_id,
                model,
                error = %error,
                "failed to persist account model request usage"
            );
        }
    }

    /// 更新账号状态并同步内存池。
    pub async fn set_status(&self, account_id: &str, status: AccountStatus) -> bool {
        let persisted = match self.store.set_status(account_id, status).await {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist account status"
                );
                false
            }
        };
        let in_memory = self.pool.lock().await.set_status(account_id, status);
        persisted || in_memory
    }

    /// 标记账号 quota 冷却状态。
    pub async fn mark_quota_limited_until(&self, account_id: &str, until: DateTime<Utc>) -> bool {
        let persisted = match self.store.mark_quota_limited_until(account_id, until).await {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist quota cooldown state"
                );
                false
            }
        };
        let in_memory = self
            .pool
            .lock()
            .await
            .mark_quota_limited_until(account_id, until);
        persisted || in_memory
    }

    /// 按请求上下文获取可用账号。
    pub async fn acquire_with(&self, request: &AccountAcquireRequest) -> Option<AcquiredAccount> {
        let refresh = self.pool.lock().await.acquire_with_status_refresh(request);
        self.persist_runtime_account_states(refresh.refreshed_accounts)
            .await;
        refresh.acquired
    }

    /// 按订阅计划各选一个可用账号，用于刷新模型列表。
    pub async fn distinct_plan_accounts(&self, now: DateTime<Utc>) -> Vec<DistinctPlanAccount> {
        let refresh = self
            .pool
            .lock()
            .await
            .distinct_plan_accounts_with_status_refresh(now);
        self.persist_runtime_account_states(refresh.refreshed_accounts)
            .await;
        refresh.accounts
    }

    async fn persist_runtime_account_states(&self, refreshed_states: Vec<RefreshedAccountState>) {
        for refreshed in refreshed_states {
            if let Err(error) = self
                .store
                .sync_runtime_account_state(&refreshed.account)
                .await
            {
                tracing::warn!(
                    account_id = %refreshed.account.id,
                    error = %error,
                    "failed to persist refreshed runtime account state"
                );
            }
            if refreshed.sync_usage_window {
                if let Err(error) = self
                    .usage_store
                    .sync_runtime_window(
                        &refreshed.account.id,
                        account_usage_window(&refreshed.account),
                    )
                    .await
                {
                    tracing::warn!(
                        account_id = %refreshed.account.id,
                        error = %error,
                        "failed to persist refreshed runtime account usage window"
                    );
                }
            }
        }
    }

    /// 按账号上一个在途槽位控制请求间隔。
    pub async fn wait_for_request_interval(&self, acquired: &AcquiredAccount) {
        let request_interval = *self
            .request_interval
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if request_interval.is_zero() {
            return;
        }
        let Some(previous_slot_at) = acquired.previous_slot_at else {
            return;
        };
        let elapsed = Utc::now()
            .signed_duration_since(previous_slot_at)
            .to_std()
            .unwrap_or_default();
        if elapsed < request_interval {
            tokio::time::sleep(request_interval - elapsed).await;
        }
    }

    /// 记录上游 token 用量。
    pub async fn record_token_usage(&self, account_id: &str, model: &str, usage: &CodexTokenUsage) {
        self.record_response_usage(account_id, model, *usage, false)
            .await;
    }

    /// 记录 Responses 请求用量。
    pub async fn record_response_usage(
        &self,
        account_id: &str,
        model: &str,
        usage: CodexTokenUsage,
        image_generation_requested: bool,
    ) {
        let image_request_succeeded = image_generation_requested && usage.image_output_tokens > 0;
        let image_request_failed = image_generation_requested && !image_request_succeeded;
        let persisted_usage = AccountUsageDelta {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            total_tokens: usage.total_tokens,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            image_requests: u64::from(image_request_succeeded),
            image_request_failures: u64::from(image_request_failed),
            ..AccountUsageDelta::default()
        };
        if let Err(error) = self
            .usage_store
            .record_usage_delta(account_id, persisted_usage)
            .await
        {
            tracing::warn!(
                account_id,
                error = %error,
                "failed to persist account token usage"
            );
        }
        if let Err(error) = self
            .usage_store
            .record_model_usage_delta(
                account_id,
                model,
                AccountModelUsageDelta {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cached_tokens: usage.cached_tokens,
                    ..AccountModelUsageDelta::default()
                },
            )
            .await
        {
            tracing::warn!(
                account_id,
                model,
                error = %error,
                "failed to persist account model token usage"
            );
        }
        self.pool.lock().await.record_window_token_usage(
            account_id,
            AccountWindowUsageDelta {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cached_tokens: usage.cached_tokens,
                image_input_tokens: usage.image_input_tokens,
                image_output_tokens: usage.image_output_tokens,
                image_request_succeeded,
                image_request_failed,
            },
        );
    }

    /// 记录空响应尝试。
    pub async fn record_empty_response_attempt(
        &self,
        account_id: &str,
        model: &str,
        image_generation_requested: bool,
    ) {
        let usage = AccountUsageDelta {
            empty_responses: 1,
            image_request_failures: u64::from(image_generation_requested),
            ..AccountUsageDelta::default()
        };
        if let Err(error) = self.usage_store.record_usage_delta(account_id, usage).await {
            tracing::warn!(
                account_id,
                error = %error,
                "failed to persist empty response usage"
            );
        }
        if let Err(error) = self
            .usage_store
            .record_model_usage_delta(
                account_id,
                model,
                AccountModelUsageDelta {
                    errors: 1,
                    ..AccountModelUsageDelta::default()
                },
            )
            .await
        {
            tracing::warn!(
                account_id,
                model,
                error = %error,
                "failed to persist account model empty response usage"
            );
        }
        if image_generation_requested {
            self.pool.lock().await.record_window_token_usage(
                account_id,
                AccountWindowUsageDelta {
                    image_request_failed: true,
                    ..AccountWindowUsageDelta::default()
                },
            );
        }
    }

    /// 根据上游返回的 rate-limit header 被动同步 quota 状态。
    pub async fn sync_passive_rate_limit_headers(
        &self,
        account: &Account,
        headers: &[(String, String)],
    ) {
        self.sync_passive_rate_limit_headers_for_account(
            &account.id,
            account.plan_type.as_deref(),
            headers,
        )
        .await;
    }

    /// 根据上游返回的 rate-limit header 被动同步指定账号 quota 状态。
    pub async fn sync_passive_rate_limit_headers_for_account(
        &self,
        account_id: &str,
        plan_type: Option<&str>,
        headers: &[(String, String)],
    ) {
        let Some(rate_limits) = parse_rate_limit_headers(headers) else {
            return;
        };
        let existing_quota = match self.store.get_quota_json(account_id).await {
            Ok(Some(quota_json)) => serde_json::from_str::<serde_json::Value>(&quota_json).ok(),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    account_id = %account_id,
                    error = %error,
                    "failed to read existing quota json before passive rate-limit sync"
                );
                None
            }
        };
        let quota = rate_limit_quota(&rate_limits, plan_type, existing_quota.as_ref());
        self.apply_quota_snapshot(account_id, &quota).await;
    }

    /// 读取运行时账号快照。
    pub async fn account_snapshot(&self, account_id: &str) -> Option<Account> {
        self.pool.lock().await.get(account_id)
    }

    /// 应用 quota 快照到持久化存储和运行时账号池。
    pub async fn apply_quota_snapshot(&self, account_id: &str, quota: &serde_json::Value) -> bool {
        let limit_reached = quota_snapshot_limit_reached(quota);
        let reset_at = quota_snapshot_reset_at(quota);
        let cooldown_until = limit_reached.then_some(reset_at).flatten();
        let limit_window_seconds = reset_at
            .and_then(|_| crate::accounts::quota::quota_snapshot_limit_window_seconds(quota));
        let quota_json = quota.to_string();
        let persisted = match self
            .store
            .apply_quota_snapshot(account_id, &quota_json, limit_reached, cooldown_until)
            .await
        {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist verified quota snapshot"
                );
                false
            }
        };

        if let Some(reset_at) = reset_at {
            if let Err(error) = self
                .usage_store
                .sync_rate_limit_window(account_id, reset_at, limit_window_seconds)
                .await
            {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist verified quota window"
                );
            }
        }

        let in_memory = {
            let mut pool = self.pool.lock().await;
            let in_memory = pool.apply_quota_state(account_id, limit_reached, cooldown_until);
            if let Some(reset_at) = reset_at {
                pool.sync_rate_limit_window(account_id, reset_at, limit_window_seconds);
            }
            in_memory
        };

        persisted || in_memory
    }

    /// 从运行时账号池移除账号。
    pub async fn remove_account(&self, account_id: &str) -> bool {
        self.pool.lock().await.remove(account_id)
    }

    /// 从仓储同步单个账号到运行时账号池。
    pub async fn sync_account_from_store(
        &self,
        account_id: &str,
    ) -> Result<bool, RuntimeAccountPoolError> {
        let account = self
            .store
            .get_pool_account(account_id)
            .await
            .map_err(|_| RuntimeAccountPoolError::Generic)?;
        if let Some(mut account) = account {
            let account_ids = vec![account.id.clone()];
            let mut usage = self
                .usage_store
                .snapshots(&account_ids)
                .await
                .map_err(|_| RuntimeAccountPoolError::Generic)?;
            if let Some(snapshot) = usage.remove(&account.id) {
                apply_usage_snapshot(&mut account, snapshot);
            }
            self.pool.lock().await.insert(account);
            return Ok(true);
        }
        Ok(self.pool.lock().await.remove(account_id))
    }

    /// 设置 Cloudflare 冷却状态。
    pub async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let persisted = match self
            .store
            .set_cloudflare_cooldown_until(account_id, cooldown_until)
            .await
        {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist Cloudflare cooldown state"
                );
                false
            }
        };
        let in_memory = self
            .pool
            .lock()
            .await
            .set_cloudflare_cooldown_until(account_id, cooldown_until);
        persisted || in_memory
    }
}

fn apply_usage_snapshot(account: &mut Account, snapshot: AccountUsageSnapshot) {
    account.request_count = snapshot.request_count;
    account.empty_response_count = snapshot.empty_response_count;
    account.image_input_tokens = snapshot.image_input_tokens;
    account.image_output_tokens = snapshot.image_output_tokens;
    account.image_request_count = snapshot.image_request_count;
    account.image_request_failed_count = snapshot.image_request_failed_count;
    account.window_request_count = snapshot.window_request_count;
    account.window_input_tokens = snapshot.window_input_tokens;
    account.window_output_tokens = snapshot.window_output_tokens;
    account.window_cached_tokens = snapshot.window_cached_tokens;
    account.window_image_input_tokens = snapshot.window_image_input_tokens;
    account.window_image_output_tokens = snapshot.window_image_output_tokens;
    account.window_image_request_count = snapshot.window_image_request_count;
    account.window_image_request_failed_count = snapshot.window_image_request_failed_count;
    account.window_started_at = snapshot.window_started_at;
    account.window_reset_at = snapshot.window_reset_at.or(account.window_reset_at);
    account.limit_window_seconds = snapshot
        .limit_window_seconds
        .or(account.limit_window_seconds);
    account.last_used_at = snapshot.last_used_at.map(|value| value.to_rfc3339());
}

fn account_usage_window(account: &Account) -> AccountUsageWindow {
    AccountUsageWindow {
        request_count: account.window_request_count,
        input_tokens: account.window_input_tokens,
        output_tokens: account.window_output_tokens,
        cached_tokens: account.window_cached_tokens,
        image_input_tokens: account.window_image_input_tokens,
        image_output_tokens: account.window_image_output_tokens,
        image_request_count: account.window_image_request_count,
        image_request_failed_count: account.window_image_request_failed_count,
        started_at: account.window_started_at,
        reset_at: account.window_reset_at,
        limit_window_seconds: account.limit_window_seconds,
    }
}

/// 运行时账号池错误。
#[derive(Debug, Error)]
pub enum RuntimeAccountPoolError {
    /// 通用账号池错误。
    #[error("account pool error")]
    Generic,
}

pub(super) fn refresh_quota_window(account: &mut Account, now: DateTime<Utc>) {
    let window_expired = account
        .window_reset_at
        .is_some_and(|reset_at| now >= reset_at);
    let cooldown_expired = account
        .quota_cooldown_until
        .is_some_and(|cooldown_until| now >= cooldown_until);

    if window_expired {
        reset_window_counters(account);
        account.window_started_at = Some(now);
        account.window_reset_at =
            next_window_reset_at(account.window_reset_at, account.limit_window_seconds, now);
    }

    if account.quota_limit_reached && (cooldown_expired || window_expired) {
        account.quota_verify_required = true;
        account.quota_limit_reached = false;
        account.quota_cooldown_until = None;
        if account.status == AccountStatus::QuotaExhausted {
            account.status = AccountStatus::Active;
        }
    }
}

pub(super) fn status_after_quota_limit(
    status: AccountStatus,
    quota_limit_reached: bool,
) -> AccountStatus {
    match (status, quota_limit_reached) {
        (AccountStatus::Active, true) => AccountStatus::QuotaExhausted,
        (status, _) => status,
    }
}

pub(super) fn reset_window_counters(account: &mut Account) {
    account.window_request_count = 0;
    account.window_input_tokens = 0;
    account.window_output_tokens = 0;
    account.window_cached_tokens = 0;
    account.window_image_input_tokens = 0;
    account.window_image_output_tokens = 0;
    account.window_image_request_count = 0;
    account.window_image_request_failed_count = 0;
}

fn next_window_reset_at(
    reset_at: Option<DateTime<Utc>>,
    limit_window_seconds: Option<u64>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let reset_at = reset_at?;
    let window_seconds = limit_window_seconds?;
    if window_seconds == 0 {
        return None;
    }

    let elapsed_seconds = now
        .signed_duration_since(reset_at)
        .num_seconds()
        .max(0)
        .cast_unsigned();
    let windows_to_advance = elapsed_seconds / window_seconds + 1;
    let advance_seconds = window_seconds
        .saturating_mul(windows_to_advance)
        .min(i64::MAX as u64);
    Some(reset_at + Duration::seconds(advance_seconds as i64))
}

pub(super) fn refresh_cloudflare_cooldown(account: &mut Account, now: DateTime<Utc>) {
    if account
        .cloudflare_cooldown_until
        .is_some_and(|cooldown_until| now >= cooldown_until)
    {
        account.cloudflare_cooldown_until = None;
    }
}
