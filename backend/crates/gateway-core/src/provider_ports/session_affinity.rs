//! 会话绑定的通用选号与认领规则，不解释协议身份或 Provider 账号资格

use super::{ProviderSessionAffinityKey, ProviderSessionBinding, ProviderSessionClaim};
use crate::account::{
    AccountAffinity, AccountCandidate, AccountSelectionContext, AccountSelectionPolicy,
    AccountSelector, ProviderAccountId,
};

/// 一次选号观察到的绑定；CAS 冲突后须释放租约并重新读取
pub struct ProviderSessionAffinityState<'a> {
    key: &'a ProviderSessionAffinityKey,
    binding: Option<ProviderSessionBinding>,
    policy: AccountSelectionPolicy,
}

impl<'a> ProviderSessionAffinityState<'a> {
    #[must_use]
    pub const fn new(
        key: &'a ProviderSessionAffinityKey,
        binding: Option<ProviderSessionBinding>,
        policy: AccountSelectionPolicy,
    ) -> Self {
        Self {
            key,
            binding,
            policy,
        }
    }

    #[must_use]
    pub fn bound_account(&self) -> Option<&ProviderAccountId> {
        self.binding
            .as_ref()
            .map(ProviderSessionBinding::account_id)
    }

    /// 原生续接的固定账号由调用方优先设置，不以软亲和覆盖
    pub fn apply_preference(&self, context: &mut AccountSelectionContext) {
        if self.policy.account_affinity() != AccountAffinity::Relaxed
            && context.preferred_account.is_none()
        {
            context.preferred_account = self.bound_account().cloned();
            context.preferred_account_overrides_weight =
                self.policy.account_affinity() == AccountAffinity::Strict;
        }
    }

    /// Provider 先投影凭据、额度和模型资格，通用层只区分容量不足与资格失效
    #[must_use]
    pub fn binding_eligible(
        &self,
        candidates: &[AccountCandidate],
        context: &AccountSelectionContext,
    ) -> bool {
        self.bound_account().is_some_and(|bound| {
            AccountSelector
                .policy_candidates(candidates, context)
                .iter()
                .any(|candidate| candidate.account.id() == bound)
        })
    }

    /// 严格模式允许有界排队；队列关闭或绑定已失去资格时按普通策略重选
    #[must_use]
    pub fn wait_account(
        &self,
        candidates: &[AccountCandidate],
        context: &AccountSelectionContext,
        queue_enabled: bool,
    ) -> Option<&ProviderAccountId> {
        (queue_enabled
            && self.policy.account_affinity() == AccountAffinity::Strict
            && self.binding_eligible(candidates, context))
        .then(|| self.bound_account())
        .flatten()
    }

    /// 优先模式的临时分流及原生固定账号请求不改写仍有效的主绑定
    #[must_use]
    pub fn claim<'s>(
        &'s self,
        selected: &'s ProviderAccountId,
        binding_eligible: bool,
        pinned: bool,
    ) -> ProviderSessionClaim<'s> {
        let retained = (pinned
            || (self.policy.account_affinity() == AccountAffinity::Preferred && binding_eligible))
            .then(|| self.bound_account())
            .flatten();
        ProviderSessionClaim {
            key: self.key,
            expected: self.binding.as_ref(),
            account: retained.unwrap_or(selected),
            ttl: self.policy.session_affinity_ttl(),
        }
    }
}
