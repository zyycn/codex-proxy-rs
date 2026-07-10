//! 候选账号过滤。
//!
//! 所有调度策略共享的前置：从账号池中筛出「本次请求可用」的候选集合。这是选择流程
//! 的第一步，与具体策略无关——策略只在过滤后的候选之间做选择。
//!
//! 过滤规则（全部满足才入选）：
//!
//! 1. 账号状态为 [`AccountStatus::Active`]；
//! 2. 在途槽位数未达单账号并发上限；
//! 3. 配额可用（[`AccountService::quota_available_at`]，受 `skip_quota_limited` 控制）；
//! 4. 模型在账号订阅计划的允许列表内；
//! 5. 不在本次请求的排除列表内；
//! 6. 不处于 Cloudflare 冷却期。
//!
//! 之后再按订阅层级优先级（`tier_priority`）收窄到最高可用层级；若请求指定了
//! `required_account_id`，则只保留该账号（跳过层级收窄）。

use chrono::{DateTime, Utc};

use crate::accounts::account::{Account, AccountStatus};
use crate::accounts::service::AccountService;

/// 候选过滤所需的账号池策略参数（只读视图）。
pub struct CandidateFilter<'a> {
    /// 单账号最大并发请求数。
    pub max_concurrent_per_account: usize,
    /// 是否跳过已标记配额耗尽的账号。
    pub skip_quota_limited: bool,
    /// 订阅层级优先级，越靠前优先级越高。
    pub tier_priority: &'a [String],
    /// 模型到允许订阅计划的映射。
    pub model_plan_allowlist: &'a std::collections::BTreeMap<String, Vec<String>>,
    /// 已成功拉取过模型列表的订阅计划。
    pub fetched_model_plan_types: &'a std::collections::BTreeSet<String>,
    /// 按账号 ID 读取当前在途槽位数。
    pub slot_count: &'a dyn Fn(&str) -> usize,
}

/// 本次账号获取请求的过滤条件。
pub struct CandidateRequest<'a> {
    /// 本次请求使用的模型。
    pub model: &'a str,
    /// 本次请求需要排除的账号 ID。
    pub exclude_account_ids: &'a [String],
    /// 本次请求必须使用的账号 ID（存在时跳过层级收窄，只保留该账号）。
    pub required_account_id: Option<&'a str>,
    /// 本次调度使用的当前时间。
    pub now: DateTime<Utc>,
}

/// 从账号池筛出本次请求的可用候选集合。
pub fn filter<'a>(
    accounts: impl Iterator<Item = &'a Account>,
    filter: &CandidateFilter<'_>,
    request: &CandidateRequest<'_>,
) -> Vec<Account> {
    let mut candidates = accounts
        .filter(|account| is_base_available(account, filter, request))
        .cloned()
        .collect::<Vec<_>>();

    if let Some(required_account_id) = request.required_account_id {
        candidates.retain(|account| account.id == required_account_id);
        return candidates;
    }

    if let Some(best_tier) = best_available_tier(&candidates, filter.tier_priority) {
        candidates.retain(|account| account.plan_type.as_deref() == Some(best_tier.as_str()));
    }
    candidates
}

/// 判断账号是否满足全部基础可用条件。
fn is_base_available(
    account: &Account,
    filter: &CandidateFilter<'_>,
    request: &CandidateRequest<'_>,
) -> bool {
    account.status == AccountStatus::Active
        && (filter.slot_count)(&account.id) < filter.max_concurrent_per_account
        && AccountService::quota_available_at(account, request.now, filter.skip_quota_limited)
        && is_model_allowed(
            account,
            request.model,
            filter.model_plan_allowlist,
            filter.fetched_model_plan_types,
        )
        && !request
            .exclude_account_ids
            .iter()
            .any(|account_id| account_id == &account.id)
        && AccountService::cloudflare_available_at(account, request.now)
}

/// 判断模型是否允许该账号的订阅计划使用。
fn is_model_allowed(
    account: &Account,
    model: &str,
    model_plan_allowlist: &std::collections::BTreeMap<String, Vec<String>>,
    fetched_model_plan_types: &std::collections::BTreeSet<String>,
) -> bool {
    let Some(allowed_plans) = model_plan_allowlist.get(model) else {
        return true;
    };
    let Some(plan_type) = account.plan_type.as_deref() else {
        return false;
    };
    allowed_plans.iter().any(|plan| plan == plan_type)
        || !fetched_model_plan_types.contains(plan_type)
}

/// 在候选集合中找出订阅层级优先级最高的可用层级。
fn best_available_tier(candidates: &[Account], tier_priority: &[String]) -> Option<String> {
    for tier in tier_priority {
        let matched = candidates
            .iter()
            .any(|account| account.plan_type.as_deref() == Some(tier.as_str()));
        if matched {
            return Some(tier.clone());
        }
    }
    None
}

/// 判断账号是否可用于模型列表刷新（不涉及排除列表 / 模型允许列表 / 层级收窄）。
pub fn is_model_refresh_available(
    account: &Account,
    max_concurrent_per_account: usize,
    skip_quota_limited: bool,
    slot_count: &dyn Fn(&str) -> usize,
    now: DateTime<Utc>,
) -> bool {
    account.status == AccountStatus::Active
        && slot_count(&account.id) < max_concurrent_per_account
        && AccountService::quota_available_at(account, now, skip_quota_limited)
        && AccountService::cloudflare_available_at(account, now)
}
