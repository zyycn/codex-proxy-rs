//! 账号池调度策略。

use std::{
    cmp::Ordering,
    collections::{BTreeMap, VecDeque},
};

use chrono::{DateTime, Duration, Utc};

use crate::accounts::{
    jwt::{jwt_expiry, JwtExpiry},
    model::{Account, AccountStatus},
    service::AccountService,
};

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

/// 账号池状态摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountPoolStatusSummary {
    /// 账号总数。
    pub total: usize,
    /// 可调度账号数。
    pub active: usize,
    /// token 过期账号数。
    pub expired: usize,
    /// 配额耗尽账号数。
    pub quota_exhausted: usize,
    /// 处于限流冷却的账号数。
    pub rate_limited: usize,
    /// 正在刷新账号数。
    pub refreshing: usize,
    /// 禁用账号数。
    pub disabled: usize,
    /// 封禁账号数。
    pub banned: usize,
}

/// 纯内存账号池，负责账号调度和运行时状态维护。
#[derive(Debug)]
pub struct AccountPool {
    accounts: BTreeMap<String, Account>,
    slots: BTreeMap<String, VecDeque<DateTime<Utc>>>,
    options: AccountPoolOptions,
    round_robin_cursor: usize,
    least_used_cursor: usize,
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
            accounts: BTreeMap::new(),
            slots: BTreeMap::new(),
            options,
            round_robin_cursor: 0,
            least_used_cursor: 0,
        }
    }

    /// 插入或替换账号。
    pub fn insert(&mut self, account: Account) {
        self.accounts.insert(account.id.clone(), account);
    }

    /// 移除账号及其在途槽位。
    pub fn remove(&mut self, account_id: &str) -> bool {
        let removed = self.accounts.remove(account_id).is_some();
        self.slots.remove(account_id);
        removed
    }

    /// 清空账号池。
    pub fn clear(&mut self) {
        self.accounts.clear();
        self.slots.clear();
        self.round_robin_cursor = 0;
        self.least_used_cursor = 0;
    }

    /// 替换模型计划 allowlist。
    pub fn set_model_plan_allowlist(&mut self, allowlist: BTreeMap<String, Vec<String>>) {
        self.options.model_plan_allowlist = allowlist;
    }

    /// 更新账号标签。
    pub fn set_label(&mut self, account_id: &str, label: Option<String>) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        account.label = label;
        true
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

    /// 重置账号累计和窗口用量。
    pub fn reset_usage(&mut self, account_id: &str) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        account.last_used_at = None;
        account.request_count = 0;
        account.empty_response_count = 0;
        account.image_input_tokens = 0;
        account.image_output_tokens = 0;
        account.image_request_count = 0;
        account.image_request_failed_count = 0;
        account.window_request_count = 0;
        account.window_input_tokens = 0;
        account.window_output_tokens = 0;
        account.window_cached_tokens = 0;
        account.window_image_input_tokens = 0;
        account.window_image_output_tokens = 0;
        account.window_image_request_count = 0;
        account.window_image_request_failed_count = 0;
        account.window_started_at = account.window_reset_at.map(|_| Utc::now());
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

    /// 设置账号是否需要额外配额校验。
    pub fn set_quota_verify_required(&mut self, account_id: &str, required: bool) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        account.quota_verify_required = required;
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

    /// 使用当前时间为指定模型获取账号。
    pub fn acquire(&mut self, model: &str) -> Option<Account> {
        self.acquire_with(AccountAcquireRequest::new(model, Utc::now()))
            .map(|acquired| acquired.account)
    }

    /// 使用完整调度请求获取账号。
    pub fn acquire_with(&mut self, request: AccountAcquireRequest) -> Option<AcquiredAccount> {
        self.cleanup_stale_slots(request.now);
        self.refresh_account_statuses(request.now);
        let candidates = self.candidates(&request);
        let selected = if let Some(preferred_account_id) = &request.preferred_account_id {
            candidates
                .iter()
                .find(|account| account.id == *preferred_account_id)
                .cloned()
        } else {
            None
        }
        .or_else(|| match self.options.rotation_strategy {
            RotationStrategy::LeastUsed => self.select_least_used(&candidates),
            RotationStrategy::RoundRobin => self.select_round_robin(&candidates),
            RotationStrategy::Sticky => self.select_sticky(&candidates),
        })?;
        let selected_id = selected.id.clone();
        let previous_slot_at = self.previous_slot_at(&selected_id);
        self.push_slot(&selected_id, request.now);
        let selected = self
            .mark_usage(&selected_id, request.now)
            .unwrap_or(selected);
        Some(AcquiredAccount {
            account: selected,
            previous_slot_at,
        })
    }

    /// 释放指定账号的一个在途槽位。
    pub fn release(&mut self, account_id: &str) {
        let Some(slots) = self.slots.get_mut(account_id) else {
            return;
        };
        slots.pop_front();
        if slots.is_empty() {
            self.slots.remove(account_id);
        }
    }

    /// 计算账号池容量摘要。
    pub fn capacity_summary(&mut self, now: DateTime<Utc>) -> AccountCapacitySummary {
        self.cleanup_stale_slots(now);
        self.refresh_account_statuses(now);
        let active_accounts = self
            .accounts
            .values()
            .filter(|account| {
                account.status == AccountStatus::Active
                    && AccountService::quota_available_at(
                        account,
                        now,
                        self.options.skip_quota_limited,
                    )
            })
            .count();
        let total_slots = active_accounts * self.options.max_concurrent_per_account;
        let used_slots = self
            .slots
            .iter()
            .filter(|(account_id, _)| {
                self.accounts
                    .get(*account_id)
                    .is_some_and(|account| account.status == AccountStatus::Active)
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

    /// 计算账号池状态摘要。
    pub fn status_summary(&mut self, now: DateTime<Utc>) -> AccountPoolStatusSummary {
        self.cleanup_stale_slots(now);
        self.refresh_account_statuses(now);
        let mut summary = AccountPoolStatusSummary {
            total: self.accounts.len(),
            ..AccountPoolStatusSummary::default()
        };
        for account in self.accounts.values() {
            match account.status {
                AccountStatus::Active
                    if !AccountService::quota_available_at(
                        account,
                        now,
                        self.options.skip_quota_limited,
                    ) =>
                {
                    summary.rate_limited += 1;
                }
                AccountStatus::Active => summary.active += 1,
                AccountStatus::Expired => summary.expired += 1,
                AccountStatus::QuotaExhausted => summary.quota_exhausted += 1,
                AccountStatus::Refreshing => summary.refreshing += 1,
                AccountStatus::Disabled => summary.disabled += 1,
                AccountStatus::Banned => summary.banned += 1,
            }
        }
        summary
    }

    /// 获取所有处于配额锁定状态的账号 ID。
    pub fn list_quota_locked_accounts(&self) -> Vec<String> {
        self.accounts
            .values()
            .filter(|account| {
                account.status == AccountStatus::Active && account.quota_limit_reached
            })
            .map(|account| account.id.clone())
            .collect()
    }

    fn select_least_used(&mut self, candidates: &[Account]) -> Option<Account> {
        let best_key = candidates
            .iter()
            .map(LeastUsedGroupKey::from_account)
            .min_by(compare_least_used_group)?;
        let mut tied = candidates
            .iter()
            .filter(|account| LeastUsedGroupKey::from_account(account) == best_key)
            .collect::<Vec<_>>();
        tied.sort_by(compare_lru_then_id);
        let index = self.least_used_cursor % tied.len();
        self.least_used_cursor = self.least_used_cursor.wrapping_add(1);
        Some((*tied[index]).clone())
    }

    fn select_sticky(&self, candidates: &[Account]) -> Option<Account> {
        candidates
            .iter()
            .max_by_key(|account| account.last_used_at.clone())
            .cloned()
    }

    fn select_round_robin(&mut self, candidates: &[Account]) -> Option<Account> {
        if candidates.is_empty() {
            return None;
        }
        let index = self.round_robin_cursor % candidates.len();
        self.round_robin_cursor = (index + 1) % candidates.len();
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
            .and_then(|slots| slots.back().cloned())
    }

    fn push_slot(&mut self, account_id: &str, now: DateTime<Utc>) {
        self.slots
            .entry(account_id.to_string())
            .or_default()
            .push_back(now);
    }

    fn mark_usage(&mut self, account_id: &str, now: DateTime<Utc>) -> Option<Account> {
        let account = self.accounts.get_mut(account_id)?;
        account.last_used_at = Some(now.to_rfc3339());
        account.request_count = account.request_count.saturating_add(1);
        account.window_request_count = account.window_request_count.saturating_add(1);
        if account.window_started_at.is_none() {
            if let Some(seconds) = account.limit_window_seconds {
                account.window_started_at = Some(now);
                account.window_reset_at =
                    Some(now + Duration::seconds(seconds.min(i64::MAX as u64) as i64));
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
            // slot 只代表本进程内的在途请求，超过 TTL 后必须释放，避免异常中断永久占满账号。
            slots.retain(|slot_at| now.signed_duration_since(*slot_at) <= ttl);
            !slots.is_empty()
        });
    }

    fn refresh_account_statuses(&mut self, now: DateTime<Utc>) {
        let account_ids = self.accounts.keys().cloned().collect::<Vec<_>>();
        for account_id in account_ids {
            self.refresh_account_status(&account_id, now);
        }
    }

    fn refresh_account_status(&mut self, account_id: &str, now: DateTime<Utc>) {
        let mut should_clear_slots = false;
        if let Some(account) = self.accounts.get_mut(account_id) {
            if account.status == AccountStatus::Active && access_token_expired(account, now) {
                account.status = AccountStatus::Expired;
                should_clear_slots = true;
            } else {
                refresh_quota_window(account, now);
                refresh_cloudflare_cooldown(account, now);
            }
        }
        if should_clear_slots {
            self.slots.remove(account_id);
        }
    }

    fn is_model_allowed(&self, account: &Account, model: &str) -> bool {
        let Some(allowed_plans) = self.options.model_plan_allowlist.get(model) else {
            return true;
        };
        allowed_plans
            .iter()
            .any(|plan| account.plan_type.as_deref() == Some(plan.as_str()))
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

    let elapsed_seconds = now.signed_duration_since(reset_at).num_seconds().max(0) as u64;
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct LeastUsedGroupKey {
    quota_limited: bool,
    window_reset_at: Option<DateTime<Utc>>,
    request_count: u64,
}

impl LeastUsedGroupKey {
    fn from_account(account: &Account) -> Self {
        Self {
            quota_limited: account.quota_limit_reached,
            window_reset_at: account.window_reset_at,
            request_count: account.request_count,
        }
    }
}

fn compare_least_used_group(a: &LeastUsedGroupKey, b: &LeastUsedGroupKey) -> Ordering {
    a.quota_limited
        .cmp(&b.quota_limited)
        .then_with(|| compare_window_reset(a.window_reset_at, b.window_reset_at))
        .then_with(|| a.request_count.cmp(&b.request_count))
}

fn compare_window_reset(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) | (None, Some(_)) | (None, None) => Ordering::Equal,
    }
}

fn compare_lru_then_id(a: &&Account, b: &&Account) -> Ordering {
    a.last_used_at
        .cmp(&b.last_used_at)
        .then_with(|| a.id.cmp(&b.id))
}
