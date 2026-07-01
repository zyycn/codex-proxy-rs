//! 账号池调度策略。

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc as StdArc,
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Utc};
use indexmap::IndexMap;
use thiserror::Error;

use crate::upstream::accounts::quota::{quota_snapshot_limit_reached, quota_snapshot_reset_at};
use crate::upstream::accounts::store::AccountStore;
use crate::upstream::accounts::token_refresh::{jwt_expiry, JwtExpiry};
use crate::upstream::accounts::{
    model::{Account, AccountModelUsageDelta, AccountStatus, AccountUsageDelta},
    service::AccountService,
};
use crate::upstream::protocol::events::{
    parse_rate_limit_headers, rate_limit_quota, TokenUsage as CodexTokenUsage,
};

/// Token usage statistics.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub image_input_tokens: i64,
    pub image_output_tokens: i64,
}

/// 账号轮转策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationStrategy {
    /// 优先选择当前窗口用量最低的账号。
    LeastUsed,
    /// 按候选账号顺序循环选择。
    RoundRobin,
    /// 优先选择最近使用过的账号。
    Sticky,
}

impl RotationStrategy {
    fn as_str(self) -> &'static str {
        match self {
            Self::LeastUsed => "least_used",
            Self::RoundRobin => "round_robin",
            Self::Sticky => "sticky",
        }
    }
}

/// 账号池配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPoolOptions {
    /// 单账号允许的最大并发请求数。
    pub max_concurrent_per_account: usize,
    /// 在途槽位未释放时的过期时间。
    pub stale_slot_ttl: Duration,
    /// 账号选择轮转策略。
    pub rotation_strategy: RotationStrategy,
    /// 是否跳过已标记配额耗尽的账号。
    pub skip_quota_limited: bool,
    /// 订阅层级优先级，越靠前优先级越高。
    pub tier_priority: Vec<String>,
    /// 模型到允许订阅计划的映射。
    pub model_plan_allowlist: BTreeMap<String, Vec<String>>,
    /// 已成功拉取过模型列表的订阅计划。
    pub fetched_model_plan_types: BTreeSet<String>,
    /// 模型到指定账号 ID 的显式路由。
    pub model_account_routes: BTreeMap<String, Vec<String>>,
}

impl Default for AccountPoolOptions {
    fn default() -> Self {
        Self {
            max_concurrent_per_account: 3,
            stale_slot_ttl: Duration::minutes(5),
            rotation_strategy: RotationStrategy::LeastUsed,
            skip_quota_limited: true,
            tier_priority: Vec::new(),
            model_plan_allowlist: BTreeMap::new(),
            fetched_model_plan_types: BTreeSet::new(),
            model_account_routes: BTreeMap::new(),
        }
    }
}

/// 账号获取请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountAcquireRequest {
    /// 本次请求使用的模型。
    pub model: String,
    /// 本次请求需要排除的账号 ID。
    pub exclude_account_ids: Vec<String>,
    /// 会话亲和性建议的优先账号 ID。
    pub preferred_account_id: Option<String>,
    /// 本次调度使用的当前时间。
    pub now: DateTime<Utc>,
}

impl AccountAcquireRequest {
    /// 构造账号获取请求。
    pub fn new(model: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            model: model.into(),
            exclude_account_ids: Vec::new(),
            preferred_account_id: None,
            now,
        }
    }

    /// 设置需要排除的账号 ID。
    pub fn with_exclude_account_ids(
        mut self,
        account_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.exclude_account_ids = account_ids.into_iter().map(Into::into).collect();
        self
    }

    /// 设置会话亲和性建议的优先账号 ID。
    pub fn with_preferred_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.preferred_account_id = Some(account_id.into());
        self
    }
}

/// 成功获取到的账号及调度元数据。
#[derive(Debug, Clone)]
pub struct AcquiredAccount {
    /// 被选中的账号快照。
    pub account: Account,
    /// 同一账号上一个在途槽位的创建时间。
    pub previous_slot_at: Option<DateTime<Utc>>,
}

/// 模型刷新时每个订阅计划选中的账号。
#[derive(Debug, Clone)]
pub struct DistinctPlanAccount {
    /// 订阅计划类型。
    pub plan_type: String,
    /// 被选中的账号快照。
    pub account: Account,
}

#[derive(Debug)]
struct AccountAcquireWithStatusRefresh {
    acquired: Option<AcquiredAccount>,
    expired_account_ids: Vec<String>,
}

#[derive(Debug)]
struct DistinctPlanAccountsWithStatusRefresh {
    accounts: Vec<DistinctPlanAccount>,
    expired_account_ids: Vec<String>,
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

/// 纯内存账号池，负责账号调度和运行时状态维护。
#[derive(Debug)]
pub struct AccountPool {
    accounts: IndexMap<String, Account>,
    slots: BTreeMap<String, VecDeque<AccountSlot>>,
    options: AccountPoolOptions,
    rotation_cursor: usize,
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
            rotation_cursor: 0,
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

    /// 移除账号及其在途槽位。
    pub fn remove(&mut self, account_id: &str) -> bool {
        let removed = self.accounts.shift_remove(account_id).is_some();
        self.slots.remove(account_id);
        removed
    }

    /// 清空账号池。
    pub fn clear(&mut self) {
        self.accounts.clear();
        self.slots.clear();
        self.rotation_cursor = 0;
    }

    /// 替换账号池运行参数。
    pub fn set_options(&mut self, options: AccountPoolOptions) {
        if self.options.rotation_strategy != options.rotation_strategy {
            self.rotation_cursor = 0;
        }
        self.options = options;
    }

    /// 更新账号状态。
    pub fn set_status(&mut self, account_id: &str, status: AccountStatus) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
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

    fn acquire_with_status_refresh(
        &mut self,
        request: &AccountAcquireRequest,
    ) -> AccountAcquireWithStatusRefresh {
        self.cleanup_stale_slots(request.now);
        let expired_account_ids = self.refresh_account_statuses(request.now);
        let candidates = self.candidates(request);
        let selected = if let Some(preferred_account_id) = &request.preferred_account_id {
            let preferred = candidates
                .iter()
                .find(|account| account.id == *preferred_account_id)
                .cloned();
            preferred.map(|account| {
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
            Some(match self.options.rotation_strategy {
                RotationStrategy::LeastUsed => {
                    let account = self.select_least_used(&candidates)?;
                    let previous = self.previous_slot_at(&account.id);
                    (account, previous, "least_used")
                }
                RotationStrategy::RoundRobin => {
                    let account = self.select_round_robin(&candidates)?;
                    let previous = self.previous_slot_at(&account.id);
                    (account, previous, "round_robin")
                }
                RotationStrategy::Sticky => {
                    let account = Self::select_sticky(&candidates)?;
                    let previous = self.previous_slot_at(&account.id);
                    (account, previous, "sticky")
                }
            })
        });
        let Some((selected, previous_slot_at, selection_source)) = selected else {
            return AccountAcquireWithStatusRefresh {
                acquired: None,
                expired_account_ids,
            };
        };
        tracing::info!(
            model = %request.model,
            rotation_strategy = self.options.rotation_strategy.as_str(),
            selection_source,
            account_id = %selected.id,
            candidate_count = candidates.len(),
            request_count = selected.request_count,
            window_request_count = selected.window_request_count,
            window_reset_at = ?selected.window_reset_at,
            last_used_at = selected.last_used_at.as_deref().unwrap_or_default(),
            quota_limit_reached = selected.quota_limit_reached,
            quota_cooldown_until = ?selected.quota_cooldown_until,
            previous_slot_at = ?previous_slot_at,
            rotation_cursor = self.rotation_cursor,
            "account selected for upstream request"
        );

        let selected_id = selected.id.clone();
        self.push_slot(&selected_id, request.now, Some(&request.model));
        AccountAcquireWithStatusRefresh {
            acquired: Some(AcquiredAccount {
                account: selected,
                previous_slot_at,
            }),
            expired_account_ids,
        }
    }

    fn refresh_account_statuses(&mut self, now: DateTime<Utc>) -> Vec<String> {
        let mut expired_account_ids = Vec::new();
        for (account_id, account) in &mut self.accounts {
            if account.status == AccountStatus::Active && access_token_expired(account, now) {
                account.status = AccountStatus::Expired;
                expired_account_ids.push(account_id.clone());
            } else {
                refresh_quota_window(account, now);
                refresh_cloudflare_cooldown(account, now);
            }
        }
        for account_id in &expired_account_ids {
            self.slots.remove(account_id);
        }
        expired_account_ids
    }

    /// 释放指定账号的一个在途槽位。
    pub fn release(&mut self, account_id: &str) -> Option<ReleasedAccountUsage> {
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
        self.mark_request_usage(account_id, Utc::now());
        Some(ReleasedAccountUsage {
            model: slot.and_then(|slot| slot.model),
        })
    }

    /// 计算账号池容量摘要。
    pub fn capacity_summary(&mut self, now: DateTime<Utc>) -> AccountCapacitySummary {
        self.cleanup_stale_slots(now);
        self.refresh_account_statuses(now);
        let is_capacity_account = |account: &Account| {
            account.status == AccountStatus::Active
                && AccountService::quota_available_at(account, now, self.options.skip_quota_limited)
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

        AccountCapacitySummary {
            max_concurrent_per_account: self.options.max_concurrent_per_account,
            total_slots,
            used_slots,
            available_slots: total_slots.saturating_sub(used_slots),
        }
    }

    /// 按订阅计划各选一个可用账号，用于刷新模型列表。
    pub fn distinct_plan_accounts(&mut self, now: DateTime<Utc>) -> Vec<DistinctPlanAccount> {
        self.distinct_plan_accounts_with_status_refresh(now)
            .accounts
    }

    fn distinct_plan_accounts_with_status_refresh(
        &mut self,
        now: DateTime<Utc>,
    ) -> DistinctPlanAccountsWithStatusRefresh {
        self.cleanup_stale_slots(now);
        let expired_account_ids = self.refresh_account_statuses(now);
        let mut by_plan = IndexMap::<String, Vec<Account>>::new();

        for account in self.accounts.values() {
            if !self.is_model_refresh_available(account, now) {
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

        let mut accounts = Vec::new();
        for (plan_type, group) in by_plan {
            let selected = match self.options.rotation_strategy {
                RotationStrategy::LeastUsed => self.select_least_used(&group),
                RotationStrategy::RoundRobin => self.select_round_robin(&group),
                RotationStrategy::Sticky => Self::select_sticky(&group),
            };
            let Some(account) = selected else {
                continue;
            };
            tracing::info!(
                plan_type,
                rotation_strategy = self.options.rotation_strategy.as_str(),
                account_id = %account.id,
                candidate_count = group.len(),
                request_count = account.request_count,
                window_request_count = account.window_request_count,
                window_reset_at = ?account.window_reset_at,
                last_used_at = account.last_used_at.as_deref().unwrap_or_default(),
                quota_limit_reached = account.quota_limit_reached,
                quota_cooldown_until = ?account.quota_cooldown_until,
                rotation_cursor = self.rotation_cursor,
                "account selected for model refresh"
            );
            self.push_slot(&account.id, now, None);
            accounts.push(DistinctPlanAccount { plan_type, account });
        }

        DistinctPlanAccountsWithStatusRefresh {
            accounts,
            expired_account_ids,
        }
    }

    fn select_least_used(&mut self, candidates: &[Account]) -> Option<Account> {
        let mut sorted = candidates.iter().collect::<Vec<_>>();
        sorted.sort_by(|a, b| compare_least_used(a, b));
        let best = *sorted.first()?;
        let tied_count = sorted
            .iter()
            .take_while(|account| compare_least_used(best, account) == Ordering::Equal)
            .count();
        let index = self.rotation_cursor % tied_count;
        self.rotation_cursor = self.rotation_cursor.wrapping_add(1);
        Some((*sorted[index]).clone())
    }

    fn select_sticky(candidates: &[Account]) -> Option<Account> {
        let mut selected = candidates.first()?;
        for candidate in &candidates[1..] {
            if compare_last_used(
                candidate.last_used_at.as_deref(),
                selected.last_used_at.as_deref(),
            ) == Ordering::Greater
            {
                selected = candidate;
            }
        }
        Some((*selected).clone())
    }

    fn select_round_robin(&mut self, candidates: &[Account]) -> Option<Account> {
        if candidates.is_empty() {
            return None;
        }
        self.rotation_cursor %= candidates.len();
        let index = self.rotation_cursor;
        self.rotation_cursor = self.rotation_cursor.wrapping_add(1);
        Some(candidates[index].clone())
    }

    fn candidates(&self, request: &AccountAcquireRequest) -> Vec<Account> {
        let mut candidates = self
            .accounts
            .values()
            .filter(|account| self.is_base_available(account, request))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(best_tier) = self.best_available_tier(&candidates) {
            candidates.retain(|account| account.plan_type.as_deref() == Some(best_tier.as_str()));
        }
        candidates
    }

    fn is_base_available(&self, account: &Account, request: &AccountAcquireRequest) -> bool {
        account.status == AccountStatus::Active
            && self.slot_count(&account.id) < self.options.max_concurrent_per_account
            && AccountService::quota_available_at(
                account,
                request.now,
                self.options.skip_quota_limited,
            )
            && self.is_model_account_allowed(account, &request.model)
            && self.is_model_allowed(account, &request.model)
            && !request
                .exclude_account_ids
                .iter()
                .any(|account_id| account_id == &account.id)
            && AccountService::cloudflare_available_at(account, request.now)
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

    fn is_model_refresh_available(&self, account: &Account, now: DateTime<Utc>) -> bool {
        account.status == AccountStatus::Active
            && self.slot_count(&account.id) < self.options.max_concurrent_per_account
            && AccountService::quota_available_at(account, now, self.options.skip_quota_limited)
            && AccountService::cloudflare_available_at(account, now)
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
        if should_reset_window_counters(account, new_reset_at, limit_window_seconds) {
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

    fn is_model_allowed(&self, account: &Account, model: &str) -> bool {
        let Some(allowed_plans) = self.options.model_plan_allowlist.get(model) else {
            return true;
        };
        let Some(plan_type) = account.plan_type.as_deref() else {
            return false;
        };
        allowed_plans.iter().any(|plan| plan == plan_type)
            || !self.options.fetched_model_plan_types.contains(plan_type)
    }

    fn is_model_account_allowed(&self, account: &Account, model: &str) -> bool {
        let Some(account_ids) = self.options.model_account_routes.get(model) else {
            return true;
        };
        account_ids
            .iter()
            .any(|account_id| account_id == &account.id)
    }

    fn best_available_tier(&self, candidates: &[Account]) -> Option<String> {
        self.options.tier_priority.iter().find_map(|tier| {
            candidates
                .iter()
                .find(|account| account.plan_type.as_deref() == Some(tier.as_str()))
                .and_then(|account| account.plan_type.clone())
        })
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

/// 运行时账号池服务。
#[derive(Clone)]
pub struct RuntimeAccountPoolService {
    pool: StdArc<tokio::sync::Mutex<AccountPool>>,
    store: StdArc<dyn AccountStore>,
    request_interval: StdArc<std::sync::RwLock<StdDuration>>,
}

impl RuntimeAccountPoolService {
    /// 构造运行时账号池服务。
    pub fn new(
        store: StdArc<dyn AccountStore>,
        options: AccountPoolOptions,
        request_interval_ms: u64,
    ) -> Self {
        Self {
            pool: StdArc::new(tokio::sync::Mutex::new(AccountPool::with_options(options))),
            store,
            request_interval: StdArc::new(std::sync::RwLock::new(StdDuration::from_millis(
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
    pub async fn restore_from_repository(&self) -> Result<usize, RuntimeAccountPoolError> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(|_| RuntimeAccountPoolError::Generic)?;
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
        self.pool.lock().await.capacity_summary(now)
    }

    /// 使用当前时间读取账号池容量摘要。
    pub async fn capacity_summary_now(&self) -> AccountCapacitySummary {
        self.capacity_summary(Utc::now()).await
    }

    /// 释放账号在途槽位。
    pub async fn release(&self, account_id: &str) {
        let released = self.pool.lock().await.release(account_id);
        let Some(released) = released else {
            return;
        };
        if let Err(error) = self.store.record_request(account_id).await {
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
            .store
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
        self.persist_expired_statuses(refresh.expired_account_ids)
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
        self.persist_expired_statuses(refresh.expired_account_ids)
            .await;
        refresh.accounts
    }

    async fn persist_expired_statuses(&self, account_ids: Vec<String>) {
        for account_id in account_ids {
            if let Err(error) = self
                .store
                .set_status(&account_id, AccountStatus::Expired)
                .await
            {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist runtime-expired account status"
                );
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
            .store
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
            .store
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
        if let Err(error) = self.store.record_usage_delta(account_id, usage).await {
            tracing::warn!(
                account_id,
                error = %error,
                "failed to persist empty response usage"
            );
        }
        if let Err(error) = self
            .store
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
        let in_memory =
            self.pool
                .lock()
                .await
                .apply_quota_state(account_id, limit_reached, cooldown_until);

        if let Some(reset_at) = reset_at {
            let limit_window_seconds =
                crate::upstream::accounts::quota::quota_snapshot_limit_window_seconds(quota);
            if let Err(error) = self
                .store
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

    /// 从运行时账号池移除账号。
    pub async fn remove_account(&self, account_id: &str) -> bool {
        self.pool.lock().await.remove(account_id)
    }

    /// 从仓储同步单个账号到运行时账号池。
    pub async fn sync_account_from_repository(
        &self,
        account_id: &str,
    ) -> Result<bool, RuntimeAccountPoolError> {
        let account = self
            .store
            .get_pool_account(account_id)
            .await
            .map_err(|_| RuntimeAccountPoolError::Generic)?;
        let mut pool = self.pool.lock().await;
        if let Some(account) = account {
            pool.insert(account);
            return Ok(true);
        }
        Ok(pool.remove(account_id))
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

/// 运行时账号池错误。
#[derive(Debug, Error)]
pub enum RuntimeAccountPoolError {
    /// 通用账号池错误。
    #[error("account pool error")]
    Generic,
}

fn refresh_quota_window(account: &mut Account, now: DateTime<Utc>) {
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

fn should_reset_window_counters(
    account: &Account,
    new_reset_at: DateTime<Utc>,
    limit_window_seconds: Option<u64>,
) -> bool {
    let Some(old_reset_at) = account.window_reset_at else {
        return false;
    };
    if old_reset_at == new_reset_at {
        return false;
    }
    let drift = old_reset_at
        .signed_duration_since(new_reset_at)
        .num_seconds()
        .unsigned_abs();
    let window_seconds = limit_window_seconds
        .or(account.limit_window_seconds)
        .unwrap_or(0);
    let threshold = if window_seconds > 0 {
        window_seconds / 2
    } else {
        3_600
    };
    drift >= threshold
}

fn reset_window_counters(account: &mut Account) {
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

fn refresh_cloudflare_cooldown(account: &mut Account, now: DateTime<Utc>) {
    if account
        .cloudflare_cooldown_until
        .is_some_and(|cooldown_until| now >= cooldown_until)
    {
        account.cloudflare_cooldown_until = None;
    }
}

fn compare_least_used(a: &Account, b: &Account) -> Ordering {
    a.quota_limit_reached
        .cmp(&b.quota_limit_reached)
        .then_with(|| compare_window_reset(a.window_reset_at, b.window_reset_at))
        .then_with(|| a.request_count.cmp(&b.request_count))
        .then_with(|| compare_last_used(a.last_used_at.as_deref(), b.last_used_at.as_deref()))
}

fn compare_window_reset(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_) | None, None) | (None, Some(_)) => Ordering::Equal,
    }
}

fn compare_last_used(a: Option<&str>, b: Option<&str>) -> Ordering {
    last_used_millis(a).cmp(&last_used_millis(b))
}

fn last_used_millis(value: Option<&str>) -> i64 {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|datetime| datetime.timestamp_millis())
        .unwrap_or(0)
}
