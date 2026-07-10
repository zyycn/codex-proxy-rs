//! 账号池存储态、槽位与状态变更。

use super::*;

#[derive(Debug)]
pub(super) struct AccountAcquireWithStatusRefresh {
    pub(super) acquired: Option<AcquiredAccount>,
    pub(super) refreshed_accounts: Vec<RefreshedAccountState>,
}

#[derive(Debug)]
pub(super) struct DistinctPlanAccountsWithStatusRefresh {
    pub(super) accounts: Vec<DistinctPlanAccount>,
    pub(super) refreshed_accounts: Vec<RefreshedAccountState>,
}

#[derive(Debug)]
pub(super) struct AccountCapacitySummaryWithStatusRefresh {
    pub(super) summary: AccountCapacitySummary,
    pub(super) refreshed_accounts: Vec<RefreshedAccountState>,
}

/// 账号池容量摘要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountCapacitySummary {
    /// 单账号最大并发请求数。
    pub max_concurrent_per_account: usize,
    /// 所有可用账号合计槽位数。
    pub total_slots: usize,
    /// 当前已占用槽位数。
    pub used_slots: usize,
    /// 当前可用槽位数。
    pub available_slots: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeAccountStateSnapshot {
    status: AccountStatus,
    quota_limit_reached: bool,
    quota_verify_required: bool,
    quota_cooldown_until: Option<DateTime<Utc>>,
    cloudflare_cooldown_until: Option<DateTime<Utc>>,
    window_request_count: u64,
    window_input_tokens: u64,
    window_output_tokens: u64,
    window_cached_tokens: u64,
    window_image_input_tokens: u64,
    window_image_output_tokens: u64,
    window_image_request_count: u64,
    window_image_request_failed_count: u64,
    window_started_at: Option<DateTime<Utc>>,
    window_reset_at: Option<DateTime<Utc>>,
    limit_window_seconds: Option<u64>,
}

impl From<&Account> for RuntimeAccountStateSnapshot {
    fn from(account: &Account) -> Self {
        Self {
            status: account.status,
            quota_limit_reached: account.quota_limit_reached,
            quota_verify_required: account.quota_verify_required,
            quota_cooldown_until: account.quota_cooldown_until,
            cloudflare_cooldown_until: account.cloudflare_cooldown_until,
            window_request_count: account.window_request_count,
            window_input_tokens: account.window_input_tokens,
            window_output_tokens: account.window_output_tokens,
            window_cached_tokens: account.window_cached_tokens,
            window_image_input_tokens: account.window_image_input_tokens,
            window_image_output_tokens: account.window_image_output_tokens,
            window_image_request_count: account.window_image_request_count,
            window_image_request_failed_count: account.window_image_request_failed_count,
            window_started_at: account.window_started_at,
            window_reset_at: account.window_reset_at,
            limit_window_seconds: account.limit_window_seconds,
        }
    }
}

impl RuntimeAccountStateSnapshot {
    fn usage_window_changed(&self, other: &Self) -> bool {
        self.window_request_count != other.window_request_count
            || self.window_input_tokens != other.window_input_tokens
            || self.window_output_tokens != other.window_output_tokens
            || self.window_cached_tokens != other.window_cached_tokens
            || self.window_image_input_tokens != other.window_image_input_tokens
            || self.window_image_output_tokens != other.window_image_output_tokens
            || self.window_image_request_count != other.window_image_request_count
            || self.window_image_request_failed_count != other.window_image_request_failed_count
            || self.window_started_at != other.window_started_at
            || self.window_reset_at != other.window_reset_at
            || self.limit_window_seconds != other.limit_window_seconds
    }
}

#[derive(Debug, Clone)]
pub(super) struct RefreshedAccountState {
    pub(super) account: Account,
    pub(super) sync_usage_window: bool,
}

/// 运行时窗口用量增量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountWindowUsageDelta {
    /// 输入 token 增量。
    pub input_tokens: u64,
    /// 输出 token 增量。
    pub output_tokens: u64,
    /// 缓存 token 增量。
    pub cached_tokens: u64,
    /// 图片输入 token 增量。
    pub image_input_tokens: u64,
    /// 图片输出 token 增量。
    pub image_output_tokens: u64,
    /// 是否记录一次成功图片请求。
    pub image_request_succeeded: bool,
    /// 是否记录一次失败图片请求。
    pub image_request_failed: bool,
}

/// 纯内存账号池：负责账号存储、在途槽位与运行时状态维护。
///
/// 账号**选择**已剥离到 [`crate::accounts::scheduler`]：候选过滤走
/// [`candidates::filter`]，策略选择走 [`AccountScheduler::select`]。账号池只提供选择
/// 所需的只读视图（候选切片 + `slot_count`），并持有调度器实例。
#[derive(Debug)]
pub struct AccountPool {
    accounts: IndexMap<String, Account>,
    slots: BTreeMap<String, VecDeque<AccountSlot>>,
    pub(super) options: AccountPoolOptions,
    scheduler: AccountScheduler,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AccountSlot {
    created_at: DateTime<Utc>,
    model: Option<String>,
}

/// 账号释放结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedAccountUsage {
    /// 被释放槽位对应的模型。槽位已被过期清理时为空。
    pub model: Option<String>,
}

impl Default for AccountPool {
    fn default() -> Self {
        Self::with_options(AccountPoolOptions::default())
    }
}

impl AccountPool {
    /// 使用指定配置创建账号池。
    pub fn with_options(options: AccountPoolOptions) -> Self {
        Self {
            accounts: IndexMap::new(),
            slots: BTreeMap::new(),
            options,
            scheduler: AccountScheduler::new(ScoreWeights::default()),
        }
    }

    /// 插入或替换账号。
    pub fn insert(&mut self, account: Account) {
        self.accounts.insert(account.id.clone(), account);
    }

    /// 获取账号池中的账号快照。
    pub fn get(&self, account_id: &str) -> Option<Account> {
        self.accounts.get(account_id).cloned()
    }

    /// 移除账号及其在途槽位与运行时反馈。
    pub fn remove(&mut self, account_id: &str) -> bool {
        let removed = self.accounts.shift_remove(account_id).is_some();
        self.slots.remove(account_id);
        self.scheduler.forget_feedback(account_id);
        removed
    }

    /// 清空账号池。
    pub fn clear(&mut self) {
        self.accounts.clear();
        self.slots.clear();
        self.scheduler.reset_cursor();
        self.scheduler.clear_feedback();
    }

    /// 替换账号池运行参数。
    pub fn set_options(&mut self, options: AccountPoolOptions) {
        if self.options.rotation_strategy != options.rotation_strategy {
            self.scheduler.reset_cursor();
        }
        self.options = options;
    }

    /// 回灌一次请求结果到调度器的运行时反馈（供 Smart 打分）。
    pub fn report_feedback(&self, account_id: &str, success: bool, first_token_ms: Option<u64>) {
        self.scheduler
            .report_feedback(account_id, success, first_token_ms);
    }

    /// 更新账号状态。
    pub fn set_status(&mut self, account_id: &str, status: AccountStatus) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        let status = status_after_quota_limit(status, account.quota_limit_reached);
        account.status = status;
        if status != AccountStatus::Active {
            self.slots.remove(account_id);
        }
        true
    }

    /// 标记账号配额限流直到指定时间。
    pub fn mark_quota_limited_until(
        &mut self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        let final_cooldown_until = account
            .quota_cooldown_until
            .filter(|existing| *existing > cooldown_until)
            .unwrap_or(cooldown_until);
        account.quota_limit_reached = true;
        account.quota_cooldown_until = Some(final_cooldown_until);
        account.status = status_after_quota_limit(account.status, true);
        account.window_reset_at = account
            .window_reset_at
            .filter(|existing| *existing > final_cooldown_until)
            .or(Some(final_cooldown_until));
        if let Ok(seconds) = (final_cooldown_until - Utc::now())
            .to_std()
            .map(|duration| duration.as_secs())
        {
            account.limit_window_seconds.get_or_insert(seconds);
        }
        self.slots.remove(account_id);
        true
    }

    /// 应用已经验证过的配额状态。
    pub fn apply_quota_state(
        &mut self,
        account_id: &str,
        limit_reached: bool,
        cooldown_until: Option<DateTime<Utc>>,
    ) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        account.quota_verify_required = false;
        account.quota_limit_reached = limit_reached;
        account.quota_cooldown_until = limit_reached.then_some(cooldown_until).flatten();
        match (account.status, limit_reached) {
            (AccountStatus::Active, true) => account.status = AccountStatus::QuotaExhausted,
            (AccountStatus::QuotaExhausted, false) => account.status = AccountStatus::Active,
            _ => {}
        }
        if let Some(cooldown_until) = account.quota_cooldown_until {
            account.window_reset_at = account
                .window_reset_at
                .filter(|existing| *existing > cooldown_until)
                .or(Some(cooldown_until));
        }
        if limit_reached {
            self.slots.remove(account_id);
        }
        true
    }

    /// 同步上游返回的限流窗口信息。
    pub fn sync_rate_limit_window(
        &mut self,
        account_id: &str,
        new_reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> bool {
        self.sync_rate_limit_window_at(account_id, new_reset_at, limit_window_seconds, Utc::now())
    }

    /// 记录账号运行时窗口 token 用量。
    pub fn record_window_token_usage(
        &mut self,
        account_id: &str,
        usage: AccountWindowUsageDelta,
    ) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        account.image_input_tokens = account
            .image_input_tokens
            .saturating_add(usage.image_input_tokens);
        account.image_output_tokens = account
            .image_output_tokens
            .saturating_add(usage.image_output_tokens);
        if usage.image_request_succeeded {
            account.image_request_count = account.image_request_count.saturating_add(1);
        }
        if usage.image_request_failed {
            account.image_request_failed_count =
                account.image_request_failed_count.saturating_add(1);
        }
        account.window_input_tokens = account
            .window_input_tokens
            .saturating_add(usage.input_tokens);
        account.window_output_tokens = account
            .window_output_tokens
            .saturating_add(usage.output_tokens);
        account.window_cached_tokens = account
            .window_cached_tokens
            .saturating_add(usage.cached_tokens);
        account.window_image_input_tokens = account
            .window_image_input_tokens
            .saturating_add(usage.image_input_tokens);
        account.window_image_output_tokens = account
            .window_image_output_tokens
            .saturating_add(usage.image_output_tokens);
        if usage.image_request_succeeded {
            account.window_image_request_count =
                account.window_image_request_count.saturating_add(1);
        }
        if usage.image_request_failed {
            account.window_image_request_failed_count =
                account.window_image_request_failed_count.saturating_add(1);
        }
        true
    }

    /// 标记账号处于 Cloudflare 冷却期。
    pub fn set_cloudflare_cooldown_until(
        &mut self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        account.cloudflare_cooldown_until = Some(cooldown_until);
        self.slots.remove(account_id);
        true
    }

    /// 使用完整调度请求获取账号。
    pub fn acquire_with(&mut self, request: &AccountAcquireRequest) -> Option<AcquiredAccount> {
        self.acquire_with_status_refresh(request).acquired
    }

    pub(super) fn acquire_with_status_refresh(
        &mut self,
        request: &AccountAcquireRequest,
    ) -> AccountAcquireWithStatusRefresh {
        self.cleanup_stale_slots(request.now);
        let refreshed_accounts = self.refresh_account_statuses(request.now);
        let candidates = self.filter_candidates(request);
        // 亲和优先：required 硬锁定该账号；preferred 若在候选内则直接选中；
        // 二者都不命中时，回退到调度器按当前策略选择。
        let selected = if let Some(required_account_id) = &request.required_account_id {
            candidates.first().cloned().map(|account| {
                (
                    account,
                    self.previous_slot_at(required_account_id),
                    "required",
                )
            })
        } else if let Some(preferred_account_id) = &request.preferred_account_id {
            candidates
                .iter()
                .find(|account| account.id == *preferred_account_id)
                .cloned()
                .map(|account| {
                    (
                        account,
                        self.previous_slot_at(preferred_account_id),
                        "preferred",
                    )
                })
        } else {
            None
        }
        .or_else(|| {
            let account = self.select_candidate(&candidates, request.now)?;
            let previous = self.previous_slot_at(&account.id);
            Some((account, previous, self.options.rotation_strategy.as_str()))
        });
        let Some((selected, previous_slot_at, selection_source)) = selected else {
            return AccountAcquireWithStatusRefresh {
                acquired: None,
                refreshed_accounts,
            };
        };
        tracing::info!(
            model = %request.model,
            rotation_strategy = self.options.rotation_strategy.as_str(),
            selection_source,
            account_id = %selected.id,
            candidate_count = candidates.len(),
            slot_count = self.slot_count(&selected.id),
            request_count = selected.request_count,
            window_request_count = selected.window_request_count,
            window_reset_at = ?selected.window_reset_at,
            last_used_at = selected.last_used_at.as_deref().unwrap_or_default(),
            quota_limit_reached = selected.quota_limit_reached,
            quota_cooldown_until = ?selected.quota_cooldown_until,
            previous_slot_at = ?previous_slot_at,
            "account selected for upstream request"
        );

        let selected_id = selected.id.clone();
        self.push_slot(&selected_id, request.now, Some(&request.model));
        AccountAcquireWithStatusRefresh {
            acquired: Some(AcquiredAccount {
                account: selected,
                previous_slot_at,
            }),
            refreshed_accounts,
        }
    }

    fn refresh_account_statuses(&mut self, now: DateTime<Utc>) -> Vec<RefreshedAccountState> {
        let mut refreshed_accounts = Vec::new();
        let mut expired_account_ids = Vec::new();
        for (account_id, account) in &mut self.accounts {
            let before = RuntimeAccountStateSnapshot::from(&*account);
            if account.status == AccountStatus::Active && access_token_expired(account, now) {
                account.status = AccountStatus::Expired;
                expired_account_ids.push(account_id.clone());
            } else {
                refresh_quota_window(account, now);
                refresh_cloudflare_cooldown(account, now);
            }
            let after = RuntimeAccountStateSnapshot::from(&*account);
            if before != after {
                let sync_usage_window = before.usage_window_changed(&after);
                refreshed_accounts.push(RefreshedAccountState {
                    account: account.clone(),
                    sync_usage_window,
                });
            }
        }
        for account_id in &expired_account_ids {
            self.slots.remove(account_id);
        }
        refreshed_accounts
    }

    /// 释放指定账号的一个在途槽位。
    pub fn release(&mut self, account_id: &str) -> Option<ReleasedAccountUsage> {
        self.release_slot(account_id, true)
    }

    /// 释放指定账号的一个在途槽位，不累计请求用量。
    pub fn release_without_request_usage(
        &mut self,
        account_id: &str,
    ) -> Option<ReleasedAccountUsage> {
        self.release_slot(account_id, false)
    }

    fn release_slot(
        &mut self,
        account_id: &str,
        record_request_usage: bool,
    ) -> Option<ReleasedAccountUsage> {
        self.accounts.get(account_id)?;

        let mut remove_slots = false;
        let slot = self.slots.get_mut(account_id).and_then(|slots| {
            let slot = slots.pop_front();
            remove_slots = slots.is_empty();
            slot
        });
        if remove_slots {
            self.slots.remove(account_id);
        }
        if record_request_usage {
            self.mark_request_usage(account_id, Utc::now());
        }
        Some(ReleasedAccountUsage {
            model: slot.and_then(|slot| slot.model),
        })
    }

    /// 计算账号池容量摘要。
    pub fn capacity_summary(&mut self, now: DateTime<Utc>) -> AccountCapacitySummary {
        self.capacity_summary_with_status_refresh(now).summary
    }

    pub(super) fn capacity_summary_with_status_refresh(
        &mut self,
        now: DateTime<Utc>,
    ) -> AccountCapacitySummaryWithStatusRefresh {
        self.cleanup_stale_slots(now);
        let refreshed_accounts = self.refresh_account_statuses(now);
        let is_capacity_account = |account: &Account| {
            account.status == AccountStatus::Active
                && candidates::quota_available_at(account, now, self.options.skip_quota_limited)
        };
        let active_accounts = self
            .accounts
            .values()
            .filter(|account| is_capacity_account(account))
            .count();
        let total_slots = active_accounts * self.options.max_concurrent_per_account;
        let used_slots = self
            .slots
            .iter()
            .filter(|(account_id, _)| {
                self.accounts
                    .get(*account_id)
                    .is_some_and(&is_capacity_account)
            })
            .map(|(_, slots)| slots.len().min(self.options.max_concurrent_per_account))
            .sum();

        AccountCapacitySummaryWithStatusRefresh {
            summary: AccountCapacitySummary {
                max_concurrent_per_account: self.options.max_concurrent_per_account,
                total_slots,
                used_slots,
                available_slots: total_slots.saturating_sub(used_slots),
            },
            refreshed_accounts,
        }
    }

    /// 按订阅计划各选一个可用账号，用于刷新模型列表。
    pub fn distinct_plan_accounts(&mut self, now: DateTime<Utc>) -> Vec<DistinctPlanAccount> {
        self.distinct_plan_accounts_with_status_refresh(now)
            .accounts
    }

    pub(super) fn distinct_plan_accounts_with_status_refresh(
        &mut self,
        now: DateTime<Utc>,
    ) -> DistinctPlanAccountsWithStatusRefresh {
        self.cleanup_stale_slots(now);
        let refreshed_accounts = self.refresh_account_statuses(now);
        let mut by_plan = IndexMap::<String, Vec<Account>>::new();

        let max_concurrent = self.options.max_concurrent_per_account;
        let skip_quota_limited = self.options.skip_quota_limited;
        {
            let slot_count = |account_id: &str| self.slots.get(account_id).map_or(0, VecDeque::len);
            for account in self.accounts.values() {
                if !candidates::is_model_refresh_available(
                    account,
                    max_concurrent,
                    skip_quota_limited,
                    &slot_count,
                    now,
                ) {
                    continue;
                }
                let Some(plan_type) = account.plan_type.as_ref() else {
                    continue;
                };
                by_plan
                    .entry(plan_type.clone())
                    .or_default()
                    .push(account.clone());
            }
        }

        let strategy = self.options.rotation_strategy;
        let mut accounts = Vec::new();
        for (plan_type, group) in by_plan {
            let Some(account) = self.select_candidate(&group, now) else {
                continue;
            };
            tracing::info!(
                plan_type,
                rotation_strategy = strategy.as_str(),
                account_id = %account.id,
                candidate_count = group.len(),
                slot_count = self.slot_count(&account.id),
                request_count = account.request_count,
                window_request_count = account.window_request_count,
                window_reset_at = ?account.window_reset_at,
                last_used_at = account.last_used_at.as_deref().unwrap_or_default(),
                quota_limit_reached = account.quota_limit_reached,
                quota_cooldown_until = ?account.quota_cooldown_until,
                "account selected for model refresh"
            );
            self.push_slot(&account.id, now, None);
            accounts.push(DistinctPlanAccount { plan_type, account });
        }

        DistinctPlanAccountsWithStatusRefresh {
            accounts,
            refreshed_accounts,
        }
    }

    fn slot_count(&self, account_id: &str) -> usize {
        self.slots.get(account_id).map_or(0, VecDeque::len)
    }

    fn previous_slot_at(&self, account_id: &str) -> Option<DateTime<Utc>> {
        self.slots
            .get(account_id)
            .and_then(|slots| slots.back())
            .map(|slot| slot.created_at)
    }

    fn push_slot(&mut self, account_id: &str, now: DateTime<Utc>, model: Option<&str>) {
        self.slots
            .entry(account_id.to_string())
            .or_default()
            .push_back(AccountSlot {
                created_at: now,
                model: model.map(str::to_string),
            });
    }

    fn mark_request_usage(&mut self, account_id: &str, now: DateTime<Utc>) -> Option<Account> {
        let account = self.accounts.get_mut(account_id)?;
        account.last_used_at = Some(now.to_rfc3339());
        account.request_count = account.request_count.saturating_add(1);
        account.window_request_count = account.window_request_count.saturating_add(1);
        if account.window_started_at.is_none() {
            account.window_started_at = Some(now);
            if let Some(seconds) = account.limit_window_seconds {
                let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
                account.window_reset_at = Some(now + Duration::seconds(seconds));
            }
        }
        Some(account.clone())
    }

    fn sync_rate_limit_window_at(
        &mut self,
        account_id: &str,
        new_reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
        now: DateTime<Utc>,
    ) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        if should_reset_usage_window(
            account.window_reset_at,
            account.limit_window_seconds,
            new_reset_at,
            limit_window_seconds,
        ) {
            reset_window_counters(account);
            account.window_started_at = Some(now);
        }
        account.window_reset_at = Some(new_reset_at);
        if let Some(limit_window_seconds) = limit_window_seconds {
            account.limit_window_seconds = Some(limit_window_seconds);
        }
        true
    }

    fn cleanup_stale_slots(&mut self, now: DateTime<Utc>) {
        let ttl = self.options.stale_slot_ttl;
        self.slots.retain(|_, slots| {
            slots.retain(|slot| now.signed_duration_since(slot.created_at) <= ttl);
            !slots.is_empty()
        });
    }

    /// 过滤出本次请求的可用候选集合（委托 [`candidates::filter`]）。
    fn filter_candidates(&self, request: &AccountAcquireRequest) -> Vec<Account> {
        let slot_count = |account_id: &str| self.slots.get(account_id).map_or(0, VecDeque::len);
        candidates::filter(
            self.accounts.values(),
            &CandidateFilter {
                max_concurrent_per_account: self.options.max_concurrent_per_account,
                skip_quota_limited: self.options.skip_quota_limited,
                tier_priority: &self.options.tier_priority,
                model_plan_allowlist: &self.options.model_plan_allowlist,
                fetched_model_plan_types: &self.options.fetched_model_plan_types,
                slot_count: &slot_count,
            },
            &CandidateRequest {
                model: &request.model,
                exclude_account_ids: &request.exclude_account_ids,
                required_account_id: request.required_account_id.as_deref(),
                now: request.now,
            },
        )
    }

    /// 按当前策略从候选集合中选择一个账号（委托 [`AccountScheduler::select`]）。
    fn select_candidate(&self, candidates: &[Account], now: DateTime<Utc>) -> Option<Account> {
        let slot_count = |account_id: &str| self.slots.get(account_id).map_or(0, VecDeque::len);
        self.scheduler
            .select(self.options.rotation_strategy, candidates, &slot_count, now)
    }
}

fn access_token_expired(account: &Account, now: DateTime<Utc>) -> bool {
    if account
        .access_token_expires_at
        .is_some_and(|expires_at| now >= expires_at)
    {
        return true;
    }

    match jwt_expiry(&account.access_token, now) {
        JwtExpiry::Expired => true,
        JwtExpiry::Valid => false,
        JwtExpiry::MissingOrInvalid => account.access_token_expires_at.is_none(),
    }
}
