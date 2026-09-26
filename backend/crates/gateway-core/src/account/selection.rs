//! 请求级账号资格投影、反馈统计与选择算法。

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use crate::concurrency::{CapacityWait, ConcurrencyQueuePolicy, QueueRejection};
use crate::identity::ProviderKind;

use super::{
    AccountConcurrency, AccountStatus, ProviderAccount, ProviderAccountId, SmartSchedulingConfig,
};

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
    smart_scheduling: SmartSchedulingConfig,
    max_concurrent_per_account: AccountConcurrency,
    request_interval: Duration,
    queue_policy: ConcurrencyQueuePolicy,
}

impl AccountSelectionPolicy {
    #[must_use]
    pub fn new(
        strategy: RotationStrategy,
        max_concurrent_per_account: impl Into<AccountConcurrency>,
        request_interval: Duration,
    ) -> Self {
        Self {
            strategy,
            smart_scheduling: SmartSchedulingConfig::default(),
            max_concurrent_per_account: max_concurrent_per_account.into(),
            request_interval,
            queue_policy: ConcurrencyQueuePolicy {
                max_waiting: 0,
                timeout: Duration::ZERO,
            },
        }
    }

    #[must_use]
    pub const fn with_queue(mut self, policy: ConcurrencyQueuePolicy) -> Self {
        self.queue_policy = policy;
        self
    }

    #[must_use]
    pub const fn queue_policy(self) -> ConcurrencyQueuePolicy {
        self.queue_policy
    }

    #[must_use]
    pub const fn strategy(self) -> RotationStrategy {
        self.strategy
    }

    #[must_use]
    pub const fn with_smart_scheduling(mut self, config: SmartSchedulingConfig) -> Self {
        self.smart_scheduling = config;
        self
    }

    #[must_use]
    pub const fn smart_scheduling(self) -> SmartSchedulingConfig {
        self.smart_scheduling
    }

    #[must_use]
    pub const fn max_concurrent_per_account(self) -> AccountConcurrency {
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
    /// Provider 归一化的剩余额度基点：0 耗尽，10_000 全部可用。
    pub quota_remaining_rank: Option<u64>,
    pub cooldown: Option<super::AccountCooldown>,
    pub failure_rate_basis_points: Option<u16>,
    pub first_output_latency_ms: Option<u64>,
}

const ACCOUNT_FEEDBACK_EWMA_ALPHA: f64 = 0.2;
const ACCOUNT_CAPACITY_FAILURE_EWMA_ALPHA: f64 = 0.4;
const ACCOUNT_FAILURE_RATE_HALF_LIFE: Duration = Duration::from_secs(15 * 60);
const EMPTY_FEEDBACK_SAMPLE: u64 = f64::NAN.to_bits();

/// 一次真实上游 attempt 对账号级 Smart 调度产生的中立反馈。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAttemptFeedback {
    Succeeded { first_output_ms: Option<u64> },
    Failed { first_output_ms: Option<u64> },
    CapacityRejected { first_output_ms: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AccountFeedbackKey {
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
}

#[derive(Debug)]
struct AccountFeedback {
    failure_rate: Mutex<DecayingAccountFailureRate>,
    first_output_ms: AtomicU64,
}

#[derive(Debug, Default)]
struct DecayingAccountFailureRate {
    value: f64,
    updated_at: Option<Instant>,
}

impl DecayingAccountFailureRate {
    fn value_at(&self, now: Instant) -> f64 {
        let Some(updated_at) = self.updated_at else {
            return self.value;
        };
        let elapsed = now.checked_duration_since(updated_at).unwrap_or_default();
        self.value
            * 0.5_f64.powf(elapsed.as_secs_f64() / ACCOUNT_FAILURE_RATE_HALF_LIFE.as_secs_f64())
    }

    fn report_at(&mut self, sample: f64, alpha: f64, now: Instant) {
        let now = self
            .updated_at
            .map_or(now, |updated_at| updated_at.max(now));
        let decayed = self.value_at(now);
        self.value = alpha * sample + (1.0 - alpha) * decayed;
        self.updated_at = Some(now);
    }
}

impl Default for AccountFeedback {
    fn default() -> Self {
        Self {
            // 新账号从健康基线开始，首个失败样本按 EWMA 平滑，而不是直接判死。
            failure_rate: Mutex::new(DecayingAccountFailureRate::default()),
            first_output_ms: AtomicU64::new(EMPTY_FEEDBACK_SAMPLE),
        }
    }
}

impl AccountFeedback {
    fn report_at(&self, feedback: AccountAttemptFeedback, now: Instant) {
        let (failure, alpha, first_output_ms) = match feedback {
            AccountAttemptFeedback::Succeeded { first_output_ms } => {
                (0.0, ACCOUNT_FEEDBACK_EWMA_ALPHA, first_output_ms)
            }
            AccountAttemptFeedback::Failed { first_output_ms } => {
                (1.0, ACCOUNT_FEEDBACK_EWMA_ALPHA, first_output_ms)
            }
            AccountAttemptFeedback::CapacityRejected { first_output_ms } => {
                (1.0, ACCOUNT_CAPACITY_FAILURE_EWMA_ALPHA, first_output_ms)
            }
        };
        self.failure_rate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .report_at(failure, alpha, now);
        if let Some(first_output_ms) = first_output_ms.filter(|value| *value > 0) {
            update_feedback_ewma(&self.first_output_ms, first_output_ms as f64);
        }
    }

    fn scheduling_signals_at(&self, now: Instant) -> (Option<u16>, Option<u64>) {
        let failure_rate = self
            .failure_rate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .value_at(now);
        let failure_rate = Some((failure_rate.clamp(0.0, 1.0) * 10_000.0).round() as u16);
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
        self.scheduling_signals_at(provider_kind, account_id, Instant::now())
    }

    /// 读取账号在指定单调时刻的错误率与首个有效输出延迟 EWMA。
    #[doc(hidden)]
    #[must_use]
    pub fn scheduling_signals_at(
        &self,
        provider_kind: &ProviderKind,
        account_id: &ProviderAccountId,
        now: Instant,
    ) -> (Option<u16>, Option<u64>) {
        let key = AccountFeedbackKey {
            provider_kind: provider_kind.clone(),
            account_id: account_id.clone(),
        };
        self.accounts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(|account| account.scheduling_signals_at(now))
            .unwrap_or_default()
    }

    /// 回灌一次已经真实发送的上游 attempt。
    pub fn report(
        &self,
        provider_kind: &ProviderKind,
        account_id: &ProviderAccountId,
        feedback: AccountAttemptFeedback,
    ) {
        self.report_at(provider_kind, account_id, feedback, Instant::now());
    }

    /// 回灌一次在指定单调时刻真实发送的上游 attempt。
    #[doc(hidden)]
    pub fn report_at(
        &self,
        provider_kind: &ProviderKind,
        account_id: &ProviderAccountId,
        feedback: AccountAttemptFeedback,
        observed_at: Instant,
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
            account.report_at(feedback, observed_at);
            return;
        }
        self.accounts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key)
            .or_default()
            .report_at(feedback, observed_at);
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
    pub const fn with_rate_limit(mut self, cooldown: Option<super::AccountCooldown>) -> Self {
        self.cooldown = cooldown;
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
    /// `remaining_rank` 使用剩余额度基点，范围为 0..=10_000。
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
    /// 优先账号可调度时，是否优先于其它账号的持久权重。
    pub preferred_account_overrides_weight: bool,
    pub round_robin_cursor: u64,
    pub eligibility: AccountEligibilityPolicy,
    pub account_scope: Option<std::sync::Arc<crate::account::scope::FrozenAccountScope>>,
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
    OverriddenByPolicy,
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
    /// 返回调度策略可见的全部合格候选；权重层授权由调用策略的宿主适配器裁剪。
    #[must_use]
    pub(crate) fn policy_candidates<'a>(
        &self,
        candidates: &'a [AccountCandidate],
        context: &AccountSelectionContext,
    ) -> Vec<&'a AccountCandidate> {
        candidates
            .iter()
            .filter(|candidate| self.scheduling_blocker(candidate, context).is_none())
            .collect()
    }

    /// 对插件返回的 ID 再执行同一资格判断，并保留原有亲和遥测结果。
    #[must_use]
    pub(crate) fn select_policy_candidate<'a>(
        &self,
        candidates: &'a [AccountCandidate],
        context: &AccountSelectionContext,
        account_id: &ProviderAccountId,
    ) -> Option<AccountSelection<'a>> {
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.account.id() == account_id)?;
        if self.scheduling_blocker(candidate, context).is_some() {
            return None;
        }
        let (preferred, _) = self.preferred_decision(candidates, context);
        let preferred = if preferred == PreferredAccountSelection::Hit
            && context.preferred_account.as_ref() != Some(account_id)
        {
            PreferredAccountSelection::OverriddenByPolicy
        } else {
            preferred
        };
        Some(AccountSelection {
            candidate,
            preferred,
        })
    }

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
            .try_fold((0_u64, 0_u64), |(used, total), candidate| {
                let capacity = u64::from(
                    candidate
                        .account
                        .effective_concurrency(context.policy.max_concurrent_per_account())
                        .limit()?
                        .get(),
                );
                Some((
                    used.saturating_add(u64::from(candidate.signals.in_flight)),
                    total.saturating_add(capacity),
                ))
            })?;
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
        let (mut preferred, preferred_candidate) = self.preferred_decision(candidates, context);
        if let Some(candidate) = preferred_candidate {
            // 权重回切只裁决内置策略的软亲和，插件显式选号按实际选择记录结果。
            let prefer_higher_weight = context.policy.strategy() == RotationStrategy::Smart
                && context.policy.smart_scheduling().prefer_higher_weight();
            if (!context.preferred_account_overrides_weight || prefer_higher_weight)
                && candidate.account.weight() < highest_weight
            {
                preferred =
                    PreferredAccountSelection::Blocked(AccountSchedulingBlocker::LowerWeight);
            } else {
                return Some(AccountSelection {
                    candidate,
                    preferred: PreferredAccountSelection::Hit,
                });
            }
        }
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
                context.policy.smart_scheduling(),
                context.now,
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

    fn preferred_decision<'a>(
        &self,
        candidates: &'a [AccountCandidate],
        context: &AccountSelectionContext,
    ) -> (PreferredAccountSelection, Option<&'a AccountCandidate>) {
        let Some(preferred) = context.preferred_account.as_ref() else {
            return (PreferredAccountSelection::NotRequested, None);
        };
        let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.account.id() == preferred)
        else {
            return (PreferredAccountSelection::Missing, None);
        };
        match self.scheduling_blocker(candidate, context) {
            Some(blocker) => (PreferredAccountSelection::Blocked(blocker), None),
            None => (PreferredAccountSelection::Hit, Some(candidate)),
        }
    }

    /// 只有本地并发/调度间隔可等待；账号权限、失效、额度与上游冷却仍立即排除。
    #[must_use]
    pub fn wait_candidates(
        &self,
        candidates: &[AccountCandidate],
        context: &AccountSelectionContext,
    ) -> Vec<ProviderAccountId> {
        let mut candidates = candidates
            .iter()
            .filter(|candidate| {
                matches!(
                    self.scheduling_blocker(candidate, context),
                    None | Some(
                        AccountSchedulingBlocker::ConcurrencyLimit
                            | AccountSchedulingBlocker::RequestInterval
                    )
                )
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| {
            (
                context.preferred_account.as_ref() != Some(candidate.account.id()),
                Reverse(candidate.account.weight()),
                candidate.account.id().clone(),
            )
        });
        candidates
            .into_iter()
            .map(|candidate| candidate.account.id().clone())
            .collect()
    }

    /// Provider 先投影续写范围与资格，再将等待候选交给共用评分规则选择队列。
    pub async fn wait_for_capacity(
        &self,
        waiting: &mut CapacityWait<'_, ProviderAccountId>,
        keys: &[ProviderAccountId],
        candidates: &[AccountCandidate],
        context: &AccountSelectionContext,
    ) -> Result<(), QueueRejection> {
        let config = context.policy.smart_scheduling();
        let queue_weight = config.weights()[5];
        if context.policy.strategy() != RotationStrategy::Smart || queue_weight == 0.0 {
            return waiting.wait(keys).await;
        }
        // 账号信号在队列锁外评分，锁内只读取实时队长并查表，避免扫描候选阻塞其他等待者。
        let scores = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.account.id(),
                    smart_score(
                        candidate,
                        context.policy.max_concurrent_per_account(),
                        config,
                        context.now,
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        waiting
            .wait_with_priority(keys, |key, count| {
                scores[key] + queue_weight / (1.0 + count as f64)
            })
            .await
    }

    pub(crate) fn scheduling_blocker(
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
                .status_projection(context.now, candidate.signals.cooldown)
                .status;
            if status != AccountStatus::Normal {
                return Some(AccountSchedulingBlocker::LocalAvailability);
            }
        }
        if context.excluded_accounts.contains(candidate.account.id()) {
            return Some(AccountSchedulingBlocker::Excluded);
        }
        if candidate
            .account
            .effective_concurrency(context.policy.max_concurrent_per_account())
            .limit()
            .is_some_and(|limit| candidate.signals.in_flight >= limit.get())
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

// 首输出 10 秒时延迟得分减半；固定尺度不随其他候选账号变化。
const SMART_LATENCY_HALF_SCORE_MS: f64 = 10_000.0;
// 距重置一小时时得分减半；未知和已过期时间不提供重置奖励。
const SMART_RESET_HALF_SCORE_SECONDS: f64 = 3_600.0;

fn capacity_utilization(
    candidate: &AccountCandidate,
    default_concurrency: AccountConcurrency,
) -> f64 {
    candidate
        .account
        .effective_concurrency(default_concurrency)
        .limit()
        .map_or(0.0, |limit| {
            f64::from(candidate.signals.in_flight) / f64::from(limit.get())
        })
}

fn select_smart_candidate<'a>(
    candidates: &[&'a AccountCandidate],
    default_concurrency: AccountConcurrency,
    cursor: u64,
    config: SmartSchedulingConfig,
    now: SystemTime,
) -> Option<&'a AccountCandidate> {
    let mut ranked = candidates
        .iter()
        .map(|candidate| {
            (
                *candidate,
                smart_score(candidate, default_concurrency, config, now),
            )
        })
        .collect::<Vec<_>>();
    let best_score = ranked
        .iter()
        .map(|(_, score)| *score)
        .max_by(f64::total_cmp)?;
    ranked.retain(|(_, score)| best_score - score <= config.score_tolerance());
    // 轮换顺序保持稳定，避免分数轻微交错与 cursor 同步后仍反复命中同一账号。
    ranked.sort_unstable_by(|(left, _), (right, _)| left.account.id().cmp(right.account.id()));
    let index = (cursor % ranked.len() as u64) as usize;
    Some(ranked[index].0)
}

pub(crate) fn smart_score(
    candidate: &AccountCandidate,
    default_concurrency: AccountConcurrency,
    config: SmartSchedulingConfig,
    now: SystemTime,
) -> f64 {
    let load = 1.0 - capacity_utilization(candidate, default_concurrency).clamp(0.0, 1.0);
    let quota = candidate
        .signals
        .quota_remaining_rank
        .map_or(0.5, |quota| quota.min(10_000) as f64 / 10_000.0);
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
            SMART_LATENCY_HALF_SCORE_MS / (SMART_LATENCY_HALF_SCORE_MS + latency as f64)
        });

    let reset = candidate
        .signals
        .quota_reset_at
        .and_then(|reset| reset.duration_since(now).ok())
        .filter(|remaining| !remaining.is_zero())
        .map_or(0.0, |remaining| {
            SMART_RESET_HALF_SCORE_SECONDS
                / (SMART_RESET_HALF_SCORE_SECONDS + remaining.as_secs_f64())
        });
    let [
        load_weight,
        quota_weight,
        health_weight,
        latency_weight,
        reset_weight,
        _,
    ] = config.weights();
    load_weight * load
        + quota_weight * quota
        + health_weight * failure
        + latency_weight * latency
        + reset_weight * reset
}
