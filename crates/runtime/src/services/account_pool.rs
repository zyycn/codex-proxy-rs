use super::*;

pub(super) fn account_pool_options(config: &AppConfig) -> AccountPoolOptions {
    AccountPoolOptions {
        max_concurrent_per_account: config.auth.max_concurrent_per_account,
        stale_slot_ttl: Duration::minutes(5),
        rotation_strategy: match config.auth.rotation_strategy.as_str() {
            "round_robin" => RotationStrategy::RoundRobin,
            "sticky" => RotationStrategy::Sticky,
            _ => RotationStrategy::LeastUsed,
        },
        skip_quota_limited: config.quota.skip_exhausted,
        tier_priority: config.auth.tier_priority.clone(),
        model_plan_allowlist: BTreeMap::new(),
    }
}

/// 运行时账号池服务。
#[derive(Clone)]
pub struct RuntimeAccountPoolService {
    accounts: Arc<dyn AccountStore>,
    pool: Arc<tokio::sync::Mutex<AccountPool>>,
    request_interval: StdDuration,
}

impl RuntimeAccountPoolService {
    /// 构造运行时账号池服务。
    pub fn new(
        accounts: Arc<dyn AccountStore>,
        options: AccountPoolOptions,
        request_interval_ms: u64,
    ) -> Self {
        Self {
            accounts,
            pool: Arc::new(tokio::sync::Mutex::new(AccountPool::with_options(options))),
            request_interval: StdDuration::from_millis(request_interval_ms),
        }
    }

    /// 从账号存储恢复账号池内容。
    pub async fn restore_from_repository(&self) -> Result<usize, RuntimeAccountPoolError> {
        let accounts = self.accounts.list_pool_accounts().await?;
        let restored = accounts.len();
        let mut pool = self.pool.lock().await;
        pool.clear();
        for account in accounts {
            pool.insert(account);
        }
        Ok(restored)
    }

    /// 从账号存储同步单个账号到运行时账号池；账号已不存在时从池中移除。
    pub async fn sync_account_from_repository(
        &self,
        account_id: &str,
    ) -> Result<bool, RuntimeAccountPoolError> {
        let account = self.accounts.get_pool_account(account_id).await?;
        let mut pool = self.pool.lock().await;
        if let Some(account) = account {
            pool.insert(account);
            return Ok(true);
        }
        Ok(pool.remove(account_id))
    }

    /// 获取运行时账号池中的账号快照。
    pub async fn account_snapshot(&self, account_id: &str) -> Option<Account> {
        self.pool.lock().await.get(account_id)
    }

    /// 从运行时账号池移除账号。
    pub async fn remove_account(&self, account_id: &str) -> bool {
        self.pool.lock().await.remove(account_id)
    }

    /// 清空运行时账号池。
    pub async fn clear(&self) {
        self.pool.lock().await.clear();
    }

    /// 从账号池获取指定模型可用账号。
    pub async fn acquire(&self, model: &str, now: DateTime<Utc>) -> Option<AcquiredAccount> {
        self.acquire_with(AccountAcquireRequest::new(model, now))
            .await
    }

    /// 使用完整获取请求从账号池获取账号。
    pub async fn acquire_with(&self, request: AccountAcquireRequest) -> Option<AcquiredAccount> {
        let acquired = self.pool.lock().await.acquire_with(request)?;
        if let Err(error) = self.accounts.record_request(&acquired.account.id).await {
            tracing::warn!(
                account_id = acquired.account.id,
                error = %error,
                "failed to persist account request usage"
            );
        }
        Some(acquired)
    }

    /// 等待同一账号前一个在途请求满足配置的发送间隔。
    pub async fn wait_for_request_interval(&self, acquired: &AcquiredAccount) {
        if self.request_interval.is_zero() {
            return;
        }
        let Some(previous_slot_at) = acquired.previous_slot_at else {
            return;
        };
        let elapsed = Utc::now()
            .signed_duration_since(previous_slot_at)
            .to_std()
            .unwrap_or_default();
        if elapsed < self.request_interval {
            sleep(self.request_interval - elapsed).await;
        }
    }

    /// 释放账号的一个在途槽位。
    pub async fn release(&self, account_id: &str) {
        self.pool.lock().await.release(account_id);
    }

    /// Return a snapshot of runtime account-pool capacity.
    pub async fn capacity_summary(&self, now: DateTime<Utc>) -> AccountCapacitySummary {
        self.pool.lock().await.capacity_summary(now)
    }

    /// Return a snapshot of runtime account-pool capacity using the current time.
    pub async fn capacity_summary_now(&self) -> AccountCapacitySummary {
        self.capacity_summary(Utc::now()).await
    }

    /// 标记账号因配额限流进入冷却。
    pub async fn mark_quota_limited_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let persisted = match self
            .accounts
            .mark_quota_limited_until(account_id, cooldown_until)
            .await
        {
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
            .mark_quota_limited_until(account_id, cooldown_until);
        persisted || in_memory
    }

    /// 应用已经验证过的账号配额快照。
    pub async fn apply_quota_snapshot(&self, account_id: &str, quota: &Value) -> bool {
        let limit_reached = quota_snapshot_limit_reached(quota);
        let reset_at = quota_snapshot_reset_at(quota);
        let cooldown_until = limit_reached.then_some(reset_at).flatten();
        let quota_json = quota.to_string();
        let persisted = match self
            .accounts
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
        let in_memory =
            self.pool
                .lock()
                .await
                .apply_quota_state(account_id, limit_reached, cooldown_until);

        if let Some(reset_at) = reset_at {
            let limit_window_seconds = quota_snapshot_limit_window_seconds(quota);
            if let Err(error) = self
                .accounts
                .sync_rate_limit_window(account_id, reset_at, limit_window_seconds)
                .await
            {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist verified quota window"
                );
            }
            self.pool.lock().await.sync_rate_limit_window(
                account_id,
                reset_at,
                limit_window_seconds,
            );
        }

        persisted || in_memory
    }

    /// 标记账号处于 Cloudflare 冷却期。
    pub async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let persisted = match self
            .accounts
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

    /// 更新账号状态。
    pub async fn set_status(&self, account_id: &str, status: AccountStatus) -> bool {
        let persisted = match self.accounts.set_status(account_id, status).await {
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

    /// 清零运行时账号池中的累计和窗口用量。
    pub async fn reset_usage(&self, account_id: &str) -> bool {
        self.pool.lock().await.reset_usage(account_id)
    }

    /// 记录账号成功响应的 token 用量。
    pub async fn record_token_usage(&self, account_id: &str, usage: TokenUsage) {
        self.record_response_usage(account_id, usage, false).await;
    }

    /// 记录 Responses 成功响应的 token 与工具用量。
    pub async fn record_response_usage(
        &self,
        account_id: &str,
        usage: TokenUsage,
        image_generation_requested: bool,
    ) {
        let image_request_succeeded = image_generation_requested && usage.image_output_tokens > 0;
        let image_request_failed = image_generation_requested && !image_request_succeeded;
        let mut persisted_usage = UsageService::account_delta_from_token_usage(usage);
        persisted_usage.image_requests = bool_to_u64(image_request_succeeded);
        persisted_usage.image_request_failures = bool_to_u64(image_request_failed);
        if let Err(error) = self
            .accounts
            .record_usage_delta(account_id, persisted_usage)
            .await
        {
            tracing::warn!(
                account_id,
                error = %error,
                "failed to persist account token usage"
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

    /// 记录 Responses 空响应尝试。
    pub async fn record_empty_response_attempt(
        &self,
        account_id: &str,
        image_generation_requested: bool,
    ) {
        let usage = codex_proxy_core::accounts::usage::AccountUsageDelta {
            empty_responses: 1,
            image_request_failures: bool_to_u64(image_generation_requested),
            ..codex_proxy_core::accounts::usage::AccountUsageDelta::default()
        };
        if let Err(error) = self.accounts.record_usage_delta(account_id, usage).await {
            tracing::warn!(
                account_id,
                error = %error,
                "failed to persist empty response usage"
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

    /// 将上游成功响应头里的 rate-limit 状态被动写回配额和窗口缓存。
    pub async fn sync_passive_rate_limit_headers(
        &self,
        account: &Account,
        headers: &[(String, String)],
    ) {
        let Some(rate_limits) = parse_rate_limit_headers(headers) else {
            return;
        };
        let existing_quota = match self.accounts.get_quota_json(&account.id).await {
            Ok(Some(quota_json)) => serde_json::from_str::<Value>(&quota_json).ok(),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    account_id = %account.id,
                    error = %error,
                    "failed to read existing quota json before passive rate-limit sync"
                );
                None
            }
        };
        let quota = rate_limit_quota(
            &rate_limits,
            account.plan_type.as_deref(),
            existing_quota.as_ref(),
        );
        self.apply_quota_snapshot(&account.id, &quota).await;
    }
}

/// 运行时账号池错误。
#[derive(Debug, Error)]
pub enum RuntimeAccountPoolError {
    /// 账号存储访问失败。
    #[error("account store error: {0}")]
    Store(#[from] AccountStoreError),
}

#[derive(Clone)]
pub(crate) struct CloudflareRecovery {
    cookies: SqliteCookieStore,
    path_block_tracker: CloudflarePathBlockTracker,
    challenge_cooldowns: CloudflareChallengeCooldownTracker,
}

impl CloudflareRecovery {
    pub(super) fn new(
        cookies: SqliteCookieStore,
        path_block_tracker: CloudflarePathBlockTracker,
        challenge_cooldowns: CloudflareChallengeCooldownTracker,
    ) -> Self {
        Self {
            cookies,
            path_block_tracker,
            challenge_cooldowns,
        }
    }

    pub(super) async fn cookie_header_for_request(
        &self,
        account_id: &str,
        request_path: &str,
    ) -> Option<String> {
        match self
            .cookies
            .cookie_header_for_request(account_id, "chatgpt.com", request_path)
            .await
        {
            Ok(cookie_header) => cookie_header,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to read account cookies for upstream request"
                );
                None
            }
        }
    }

    pub(super) async fn capture_set_cookie_headers(&self, account_id: &str, headers: &[String]) {
        for header in headers {
            if let Err(error) = self.cookies.capture_set_cookie(account_id, header).await {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist upstream set-cookie header"
                );
            }
        }
    }

    pub(super) async fn apply_challenge(
        &self,
        account_pool: &RuntimeAccountPoolService,
        account_id: &str,
    ) {
        self.delete_account_cookies(account_id, "Cloudflare challenge")
            .await;
        let cooldown = self
            .challenge_cooldowns
            .record_challenge(account_id, Utc::now())
            .await;
        account_pool
            .set_cloudflare_cooldown_until(account_id, cooldown.cooldown_until)
            .await;
        tracing::warn!(
            account_id,
            challenge_count = cooldown.challenge_count,
            delay_seconds = cooldown.delay_seconds,
            "upstream returned Cloudflare challenge"
        );
    }

    pub(super) async fn apply_path_block(
        &self,
        account_pool: &RuntimeAccountPoolService,
        account_id: &str,
    ) {
        self.delete_account_cookies(account_id, "Cloudflare path-block")
            .await;
        let now = Utc::now();
        let count = self
            .path_block_tracker
            .record_path_block(account_id, now)
            .await;
        if self
            .path_block_tracker
            .should_disable(account_id, now)
            .await
        {
            account_pool
                .set_status(account_id, AccountStatus::Disabled)
                .await;
        }
        tracing::warn!(
            account_id,
            path_block_count = count,
            "upstream returned Cloudflare path-block"
        );
    }

    pub(super) async fn reset_account_recovery(&self, account_id: &str) {
        self.path_block_tracker.reset(account_id).await;
        self.challenge_cooldowns.reset(account_id).await;
    }

    async fn delete_account_cookies(&self, account_id: &str, reason: &str) {
        if let Err(error) = self.cookies.delete_account_cookies(account_id).await {
            tracing::warn!(
                account_id,
                reason,
                error = %error,
                "failed to delete account cookies after Cloudflare recovery signal"
            );
        }
    }
}

fn bool_to_u64(value: bool) -> u64 {
    if value {
        1
    } else {
        0
    }
}
