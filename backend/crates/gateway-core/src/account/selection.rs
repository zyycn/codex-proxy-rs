//! 请求级账号资格投影、反馈统计与选择算法。

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroU32;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::routing::ProviderKind;

use super::{AccountStatus, ProviderAccount, ProviderAccountId};

/// `runtime_settings.rotation_strategy` 的稳定值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RotationStrategy {
    Smart,
    QuotaResetPriority,
    RoundRobin,
    Sticky,
}

impl RotationStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::QuotaResetPriority => "quota_reset_priority",
            Self::RoundRobin => "round_robin",
            Self::Sticky => "sticky",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "smart" => Some(Self::Smart),
            "quota_reset_priority" => Some(Self::QuotaResetPriority),
            "round_robin" => Some(Self::RoundRobin),
            "sticky" => Some(Self::Sticky),
            _ => None,
        }
    }
}

/// 从 `runtime_settings` 冻结到一次请求计划的账号调度策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSelectionPolicy {
    strategy: RotationStrategy,
    max_concurrent_per_account: NonZeroU32,
    request_interval: Duration,
}

impl AccountSelectionPolicy {
    #[must_use]
    pub const fn new(
        strategy: RotationStrategy,
        max_concurrent_per_account: NonZeroU32,
        request_interval: Duration,
    ) -> Self {
        Self {
            strategy,
            max_concurrent_per_account,
            request_interval,
        }
    }

    #[must_use]
    pub const fn strategy(self) -> RotationStrategy {
        self.strategy
    }

    #[must_use]
    pub const fn max_concurrent_per_account(self) -> NonZeroU32 {
        self.max_concurrent_per_account
    }

    #[must_use]
    pub const fn request_interval(self) -> Duration {
        self.request_interval
    }
}

/// Store 提供并发事实，Provider 叠加自己解释的额度事实；全部信号均可重建。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRuntimeSignals {
    pub in_flight: u32,
    pub last_started_at: Option<SystemTime>,
    pub quota_reset_at: Option<SystemTime>,
    pub quota_remaining_rank: Option<u64>,
    pub rate_limited_until: Option<SystemTime>,
    pub failure_rate_basis_points: Option<u16>,
    pub first_output_latency_ms: Option<u64>,
}

const ACCOUNT_FEEDBACK_EWMA_ALPHA: f64 = 0.2;
const EMPTY_FEEDBACK_SAMPLE: u64 = f64::NAN.to_bits();

/// 一次真实上游 attempt 对账号级 Smart 调度产生的中立反馈。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAttemptFeedback {
    Succeeded { first_output_ms: Option<u64> },
    Failed { first_output_ms: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AccountFeedbackKey {
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
}

#[derive(Debug)]
struct AccountFeedback {
    failure_rate: AtomicU64,
    first_output_ms: AtomicU64,
}

impl Default for AccountFeedback {
    fn default() -> Self {
        Self {
            // 新账号从健康基线开始，首个失败样本按 EWMA 平滑，而不是直接判死。
            failure_rate: AtomicU64::new(0.0_f64.to_bits()),
            first_output_ms: AtomicU64::new(EMPTY_FEEDBACK_SAMPLE),
        }
    }
}

impl AccountFeedback {
    fn report(&self, feedback: AccountAttemptFeedback) {
        let (failure, first_output_ms) = match feedback {
            AccountAttemptFeedback::Succeeded { first_output_ms } => (0.0, first_output_ms),
            AccountAttemptFeedback::Failed { first_output_ms } => (1.0, first_output_ms),
        };
        update_feedback_ewma(&self.failure_rate, failure);
        if let Some(first_output_ms) = first_output_ms.filter(|value| *value > 0) {
            update_feedback_ewma(&self.first_output_ms, first_output_ms as f64);
        }
    }

    fn scheduling_signals(&self) -> (Option<u16>, Option<u64>) {
        let failure_rate = load_feedback_ewma(&self.failure_rate).map(|value| {
            let basis_points = (value.clamp(0.0, 1.0) * 10_000.0).round();
            basis_points as u16
        });
        let first_output_ms = load_feedback_ewma(&self.first_output_ms)
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| value.round() as u64);
        (failure_rate, first_output_ms)
    }
}

/// 所有 Provider 共享、按 Provider 和账号隔离的进程内 Smart 健康反馈。
#[derive(Debug, Default)]
pub struct AccountFeedbackStats {
    accounts: RwLock<HashMap<AccountFeedbackKey, AccountFeedback>>,
}

impl AccountFeedbackStats {
    /// 读取账号当前的错误率与首个有效输出延迟 EWMA。
    #[must_use]
    pub fn scheduling_signals(
        &self,
        provider_kind: &ProviderKind,
        account_id: &ProviderAccountId,
    ) -> (Option<u16>, Option<u64>) {
        let key = AccountFeedbackKey {
            provider_kind: provider_kind.clone(),
            account_id: account_id.clone(),
        };
        self.accounts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(AccountFeedback::scheduling_signals)
            .unwrap_or_default()
    }

    /// 回灌一次已经真实发送的上游 attempt。
    pub fn report(
        &self,
        provider_kind: &ProviderKind,
        account_id: &ProviderAccountId,
        feedback: AccountAttemptFeedback,
    ) {
        let key = AccountFeedbackKey {
            provider_kind: provider_kind.clone(),
            account_id: account_id.clone(),
        };
        if let Some(account) = self
            .accounts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            account.report(feedback);
            return;
        }
        self.accounts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key)
            .or_default()
            .report(feedback);
    }
}

fn load_feedback_ewma(value: &AtomicU64) -> Option<f64> {
    let value = f64::from_bits(value.load(Ordering::Relaxed));
    (!value.is_nan()).then_some(value)
}

fn update_feedback_ewma(target: &AtomicU64, sample: f64) {
    let mut current = target.load(Ordering::Relaxed);
    loop {
        let previous = f64::from_bits(current);
        let next = if previous.is_nan() {
            sample
        } else {
            ACCOUNT_FEEDBACK_EWMA_ALPHA * sample + (1.0 - ACCOUNT_FEEDBACK_EWMA_ALPHA) * previous
        };
        match target.compare_exchange_weak(
            current,
            next.to_bits(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

impl AccountRuntimeSignals {
    #[must_use]
    pub fn with_provider_quota(mut self, quota: Option<AccountQuotaSignals>) -> Self {
        if let Some(quota) = quota {
            self.quota_reset_at = quota.reset_at;
            self.quota_remaining_rank = quota.remaining_rank;
        }
        self
    }

    #[must_use]
    pub const fn with_rate_limit(mut self, rate_limited_until: Option<SystemTime>) -> Self {
        self.rate_limited_until = rate_limited_until;
        self
    }

    #[must_use]
    pub const fn with_runtime_health(
        mut self,
        failure_rate_basis_points: Option<u16>,
        first_output_latency_ms: Option<u64>,
    ) -> Self {
        self.failure_rate_basis_points = failure_rate_basis_points;
        self.first_output_latency_ms = first_output_latency_ms;
        self
    }
}

/// Provider 从私有 quota JSON 投影出的中立调度事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountQuotaSignals {
    reset_at: Option<SystemTime>,
    remaining_rank: Option<u64>,
}

impl AccountQuotaSignals {
    #[must_use]
    pub const fn new(reset_at: Option<SystemTime>, remaining_rank: Option<u64>) -> Self {
        Self {
            reset_at,
            remaining_rank,
        }
    }

    #[must_use]
    pub const fn reset_at(self) -> Option<SystemTime> {
        self.reset_at
    }

    #[must_use]
    pub const fn remaining_rank(self) -> Option<u64> {
        self.remaining_rank
    }
}

/// 账号持久事实与可重建运行信号的请求级组合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCandidate {
    pub account: ProviderAccount,
    pub signals: AccountRuntimeSignals,
}

/// 一次账号选择看到的可调度并发槽快照。
///
/// `used_slots` 包含刚刚获取成功的当前请求；快照只覆盖本次请求范围内、
/// 模型可用且未被显式排除的账号池。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountCapacitySnapshot {
    used_slots: u64,
    total_slots: u64,
}

impl AccountCapacitySnapshot {
    #[must_use]
    pub const fn used_slots(self) -> u64 {
        self.used_slots
    }

    #[must_use]
    pub const fn total_slots(self) -> u64 {
        self.total_slots
    }

    #[must_use]
    pub fn with_acquired_request(self) -> Self {
        Self {
            used_slots: self.used_slots.saturating_add(1).min(self.total_slots),
            total_slots: self.total_slots,
        }
    }
}

/// 一次账号选择使用的全局策略快照。
#[derive(Debug, Clone)]
pub struct AccountSelectionContext {
    pub policy: AccountSelectionPolicy,
    pub now: SystemTime,
    pub excluded_accounts: BTreeSet<ProviderAccountId>,
    pub preferred_account: Option<ProviderAccountId>,
    pub round_robin_cursor: u64,
    pub eligibility: AccountEligibilityPolicy,
    pub account_scope: Option<std::sync::Arc<crate::routing::FrozenAccountScope>>,
}

/// 选择账号时是否执行本地调度资格投影。
///
/// 管理端对指定账号执行诊断时，会直接向上游确认实际状态；该模式跳过
/// `enabled`、可用性、冷却和 token 到期等本地投影，仍保留租约、并发和
/// 请求间隔约束，且只能与固定账号约束组合使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountEligibilityPolicy {
    #[default]
    Enforce,
    BypassForDiagnostic,
}

impl AccountEligibilityPolicy {
    #[must_use]
    pub const fn bypasses_local_eligibility(self) -> bool {
        matches!(self, Self::BypassForDiagnostic)
    }
}

/// 候选账号未进入调度池的约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSchedulingBlocker {
    OutsideClientScope,
    LocalAvailability,
    Excluded,
    ConcurrencyLimit,
    RequestInterval,
    LowerWeight,
}

/// 优先账号在本次选择中的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredAccountSelection {
    NotRequested,
    Hit,
    Missing,
    Blocked(AccountSchedulingBlocker),
}

/// 一次完整账号选择；同时保留优先账号决策，供 Provider 记录调度遥测。
#[derive(Debug, Clone, Copy)]
pub struct AccountSelection<'a> {
    candidate: &'a AccountCandidate,
    preferred: PreferredAccountSelection,
}

impl<'a> AccountSelection<'a> {
    #[must_use]
    pub const fn candidate(self) -> &'a AccountCandidate {
        self.candidate
    }

    #[must_use]
    pub const fn preferred(self) -> PreferredAccountSelection {
        self.preferred
    }
}

/// 同一 target 内唯一的账号排序器。
#[derive(Debug, Default, Clone, Copy)]
pub struct AccountSelector;

impl AccountSelector {
    /// 汇总与本次调度约束一致的并发容量，供请求级观测使用。
    #[must_use]
    pub fn capacity_snapshot(
        &self,
        candidates: &[AccountCandidate],
        context: &AccountSelectionContext,
    ) -> Option<AccountCapacitySnapshot> {
        let (used_slots, total_slots) = candidates
            .iter()
            .filter(|candidate| {
                !matches!(
                    self.scheduling_blocker(candidate, context),
                    Some(
                        AccountSchedulingBlocker::OutsideClientScope
                            | AccountSchedulingBlocker::LocalAvailability
                            | AccountSchedulingBlocker::Excluded
                    )
                )
            })
            .fold((0_u64, 0_u64), |(used, total), candidate| {
                let capacity = u64::from(
                    candidate
                        .account
                        .effective_concurrency(context.policy.max_concurrent_per_account())
                        .get(),
                );
                (
                    used.saturating_add(u64::from(candidate.signals.in_flight)),
                    total.saturating_add(capacity),
                )
            });
        (total_slots > 0).then_some(AccountCapacitySnapshot {
            used_slots: used_slots.min(total_slots),
            total_slots,
        })
    }

    /// 从可调度账号中确定一个候选；这里只消费 Provider 已解析的额度投影。
    #[must_use]
    pub fn select<'a>(
        &self,
        candidates: &'a [AccountCandidate],
        context: &AccountSelectionContext,
    ) -> Option<AccountSelection<'a>> {
        let mut eligible = candidates
            .iter()
            .filter(|candidate| self.scheduling_blocker(candidate, context).is_none())
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            return None;
        }
        let highest_weight = eligible
            .iter()
            .map(|candidate| candidate.account.weight())
            .max()?;
        let preferred = if let Some(preferred) = context.preferred_account.as_ref() {
            match candidates
                .iter()
                .find(|candidate| candidate.account.id() == preferred)
            {
                Some(candidate) => match self.scheduling_blocker(candidate, context) {
                    Some(blocker) => PreferredAccountSelection::Blocked(blocker),
                    None if candidate.account.weight() < highest_weight => {
                        PreferredAccountSelection::Blocked(AccountSchedulingBlocker::LowerWeight)
                    }
                    None => {
                        return Some(AccountSelection {
                            candidate,
                            preferred: PreferredAccountSelection::Hit,
                        });
                    }
                },
                None => PreferredAccountSelection::Missing,
            }
        } else {
            PreferredAccountSelection::NotRequested
        };
        eligible.retain(|candidate| candidate.account.weight() == highest_weight);

        let candidate = match context.policy.strategy() {
            RotationStrategy::QuotaResetPriority => {
                let default_concurrency = context.policy.max_concurrent_per_account();
                eligible.sort_by(|left, right| {
                    (
                        left.signals.quota_reset_at.is_none(),
                        left.signals.quota_reset_at,
                    )
                        .cmp(&(
                            right.signals.quota_reset_at.is_none(),
                            right.signals.quota_reset_at,
                        ))
                        .then_with(|| {
                            capacity_utilization(left, default_concurrency)
                                .total_cmp(&capacity_utilization(right, default_concurrency))
                        })
                        .then_with(|| {
                            left.signals
                                .last_started_at
                                .cmp(&right.signals.last_started_at)
                        })
                        .then_with(|| left.account.id().cmp(right.account.id()))
                });
                eligible.first().copied()?
            }
            RotationStrategy::RoundRobin => {
                eligible.sort_by_key(|candidate| candidate.account.id().clone());
                let index = context.round_robin_cursor as usize % eligible.len();
                eligible.get(index).copied()?
            }
            RotationStrategy::Smart => select_smart_candidate(
                &eligible,
                context.policy.max_concurrent_per_account(),
                context.round_robin_cursor,
            )?,
            RotationStrategy::Sticky => {
                eligible.sort_by_key(|candidate| {
                    (
                        Reverse(candidate.signals.last_started_at),
                        candidate.account.id().clone(),
                    )
                });
                eligible.first().copied()?
            }
        };
        Some(AccountSelection {
            candidate,
            preferred,
        })
    }

    fn scheduling_blocker(
        &self,
        candidate: &AccountCandidate,
        context: &AccountSelectionContext,
    ) -> Option<AccountSchedulingBlocker> {
        if context
            .account_scope
            .as_ref()
            .is_some_and(|scope| !scope.allows(candidate.account.id()))
        {
            return Some(AccountSchedulingBlocker::OutsideClientScope);
        }
        if !context.eligibility.bypasses_local_eligibility() {
            let status = candidate
                .account
                .status_projection(context.now, candidate.signals.rate_limited_until)
                .status;
            if status != AccountStatus::Normal {
                return Some(AccountSchedulingBlocker::LocalAvailability);
            }
        }
        if context.excluded_accounts.contains(candidate.account.id()) {
            return Some(AccountSchedulingBlocker::Excluded);
        }
        if candidate.signals.in_flight
            >= candidate
                .account
                .effective_concurrency(context.policy.max_concurrent_per_account())
                .get()
        {
            return Some(AccountSchedulingBlocker::ConcurrencyLimit);
        }
        if candidate
            .signals
            .last_started_at
            .is_some_and(|last_started| {
                !context
                    .now
                    .duration_since(last_started)
                    .is_ok_and(|elapsed| elapsed >= context.policy.request_interval())
            })
        {
            return Some(AccountSchedulingBlocker::RequestInterval);
        }
        None
    }
}

const SMART_LOAD_WEIGHT: f64 = 1.0;
const SMART_QUOTA_WEIGHT: f64 = 0.8;
const SMART_FAILURE_WEIGHT: f64 = 1.0;
const SMART_LATENCY_WEIGHT: f64 = 0.5;

fn capacity_utilization(candidate: &AccountCandidate, default_concurrency: NonZeroU32) -> f64 {
    f64::from(candidate.signals.in_flight)
        / f64::from(
            candidate
                .account
                .effective_concurrency(default_concurrency)
                .get(),
        )
}

struct SmartNormalization {
    max_capacity_utilization: f64,
    min_quota: Option<u64>,
    max_quota: Option<u64>,
    min_latency_ms: Option<u64>,
    max_latency_ms: Option<u64>,
}

impl SmartNormalization {
    fn from_candidates(candidates: &[&AccountCandidate], default_concurrency: NonZeroU32) -> Self {
        let max_capacity_utilization = candidates
            .iter()
            .map(|candidate| capacity_utilization(candidate, default_concurrency))
            .reduce(f64::max)
            .unwrap_or_default();
        let min_quota = candidates
            .iter()
            .filter_map(|candidate| candidate.signals.quota_remaining_rank)
            .min();
        let max_quota = candidates
            .iter()
            .filter_map(|candidate| candidate.signals.quota_remaining_rank)
            .max();
        let min_latency_ms = candidates
            .iter()
            .filter_map(|candidate| candidate.signals.first_output_latency_ms)
            .filter(|latency| *latency > 0)
            .min();
        let max_latency_ms = candidates
            .iter()
            .filter_map(|candidate| candidate.signals.first_output_latency_ms)
            .filter(|latency| *latency > 0)
            .max();
        Self {
            max_capacity_utilization,
            min_quota,
            max_quota,
            min_latency_ms,
            max_latency_ms,
        }
    }
}

fn select_smart_candidate<'a>(
    candidates: &[&'a AccountCandidate],
    default_concurrency: NonZeroU32,
    cursor: u64,
) -> Option<&'a AccountCandidate> {
    let normalization = SmartNormalization::from_candidates(candidates, default_concurrency);
    let mut ranked = candidates
        .iter()
        .map(|candidate| {
            (
                *candidate,
                smart_score(candidate, default_concurrency, &normalization),
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left, left_score), (right, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left.account.id().cmp(right.account.id()))
    });
    let best_score = ranked.first()?.1.to_bits();
    let tied = ranked
        .iter()
        .take_while(|(_, score)| score.to_bits() == best_score)
        .count();
    let index = cursor as usize % tied;
    Some(ranked[index].0)
}

fn smart_score(
    candidate: &AccountCandidate,
    default_concurrency: NonZeroU32,
    normalization: &SmartNormalization,
) -> f64 {
    let load = lower_ratio_is_better(
        capacity_utilization(candidate, default_concurrency),
        normalization.max_capacity_utilization,
    );
    let quota = candidate.signals.quota_remaining_rank.map_or(0.5, |quota| {
        higher_is_better(
            quota,
            normalization.min_quota.unwrap_or(quota),
            normalization.max_quota.unwrap_or(quota),
        )
    });
    let failure = 1.0
        - f64::from(
            candidate
                .signals
                .failure_rate_basis_points
                .unwrap_or_default()
                .min(10_000),
        ) / 10_000.0;
    let latency = candidate
        .signals
        .first_output_latency_ms
        .filter(|latency| *latency > 0)
        .map_or(1.0, |latency| {
            lower_is_better(
                latency,
                normalization.min_latency_ms.unwrap_or(latency),
                normalization.max_latency_ms.unwrap_or(latency),
            )
        });

    SMART_LOAD_WEIGHT * load
        + SMART_QUOTA_WEIGHT * quota
        + SMART_FAILURE_WEIGHT * failure
        + SMART_LATENCY_WEIGHT * latency
}

fn lower_ratio_is_better(value: f64, maximum: f64) -> f64 {
    if maximum <= 0.0 {
        return 1.0;
    }
    1.0 - (value / maximum).clamp(0.0, 1.0)
}

fn lower_is_better(value: u64, minimum: u64, maximum: u64) -> f64 {
    if maximum <= minimum {
        return 1.0;
    }
    1.0 - (value.saturating_sub(minimum) as f64 / (maximum - minimum) as f64).clamp(0.0, 1.0)
}

fn higher_is_better(value: u64, minimum: u64, maximum: u64) -> f64 {
    if maximum <= minimum {
        return 1.0;
    }
    (value.saturating_sub(minimum) as f64 / (maximum - minimum) as f64).clamp(0.0, 1.0)
}
