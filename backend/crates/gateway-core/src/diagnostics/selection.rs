//! 复用选号时的输入，保留有界的候选评分与阻断原因。

use serde_json::json;

use super::TraceContext;
use crate::account::{
    AccountCandidate, AccountSelection, AccountSelectionContext, AccountSelector, RotationStrategy,
    smart_score,
};

// 单个 trace 事件最多 4 KiB；优先保留实际选中的账号。
const MAX_CANDIDATES: usize = 6;

impl TraceContext {
    /// 记录实际选号输入与结果；只复用评分规则，不参与调度决策。
    pub fn account_selection(
        &self,
        candidates: &[AccountCandidate],
        context: &AccountSelectionContext,
        selection: Option<&AccountSelection<'_>>,
    ) {
        if !self.is_enabled() {
            return;
        }
        let selected = selection.as_ref().map(|s| s.candidate());
        let selected_id = selected.map(|c| c.account.id());
        let smart = context.policy.strategy() == RotationStrategy::Smart;
        let observations = selected
            .into_iter()
            .chain(
                candidates
                    .iter()
                    .filter(|c| Some(c.account.id()) != selected_id),
            )
            .take(MAX_CANDIDATES)
            .map(|candidate| {
                let signals = &candidate.signals;
                json!({
                    "accountId": candidate.account.id().as_str(),
                    "weight": candidate.account.weight().get(),
                    "blocker": AccountSelector.scheduling_blocker(candidate, context)
                        .map(|blocker| format!("{blocker:?}")),
                    "inFlight": signals.in_flight,
                    "concurrencyLimit": candidate.account
                        .effective_concurrency(context.policy.max_concurrent_per_account()).get(),
                    "quotaRemainingBasisPoints": signals.quota_remaining_rank,
                    "quotaResetAtUnixMs": signals.quota_reset_at.and_then(|reset|
                        reset.duration_since(std::time::UNIX_EPOCH).ok()
                    ).map(|duration| duration.as_millis()),
                    "failureRateBasisPoints": signals.failure_rate_basis_points,
                    "firstOutputLatencyMs": signals.first_output_latency_ms,
                    "smartScore": smart.then(|| smart_score(
                        candidate, context.policy.max_concurrent_per_account(), context.policy.smart_scheduling(), context.now
                    )),
                })
            })
            .collect::<Vec<_>>();
        self.record(
            "account.selection",
            json!({
                "strategy": context.policy.strategy().as_str(),
                "selectedAccountId": selected_id.map(|id| id.as_str()),
                "preferredAccountId": context.preferred_account.as_ref().map(|id| id.as_str()),
                "preferredResult": selection.as_ref().map(|s| format!("{:?}", s.preferred())),
                "roundRobinCursor": context.round_robin_cursor,
                "smartScoreTolerance": smart.then(|| context.policy.smart_scheduling().score_tolerance()),
                "candidateCount": candidates.len(),
                "omittedCandidates": candidates.len().saturating_sub(observations.len()),
                "candidates": observations,
            }),
        );
    }
}
