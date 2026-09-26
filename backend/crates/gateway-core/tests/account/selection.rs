use std::time::{Duration, Instant};

use gateway_core::account::{
    AccountAttemptFeedback, AccountCandidate, AccountFeedbackStats, AccountSchedulingBlocker,
    AccountSelectionPolicy, AccountSelector, PreferredAccountSelection, ProviderAccountId,
    RotationStrategy,
};
use gateway_core::routing::ProviderKind;

use super::{candidate, candidate_with_concurrency, context, weighted_candidate};

const FAILURE_RATE_HALF_LIFE: Duration = Duration::from_secs(15 * 60);

#[test]
fn weight_priority_overrides_lower_affinity_but_preserves_equal_weight_affinity() {
    let mut candidates = [
        weighted_candidate("acct_preferred", 1, 0),
        weighted_candidate("acct_high", 100, 2),
    ];
    let mut context = context(RotationStrategy::WeightPriority);
    context.preferred_account = Some(candidates[0].account.id().clone());
    context.preferred_account_overrides_weight = true;
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("candidate");
    assert_eq!(selected.candidate().account.id().as_str(), "acct_high");
    assert_eq!(
        selected.preferred(),
        PreferredAccountSelection::Blocked(AccountSchedulingBlocker::LowerWeight)
    );

    candidates[0] = weighted_candidate("acct_preferred", 100, 2);
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("candidate");
    assert_eq!(selected.candidate().account.id().as_str(), "acct_preferred");
    assert_eq!(selected.preferred(), PreferredAccountSelection::Hit);
}

#[test]
fn weight_priority_spills_over_when_busy_and_returns_when_capacity_recovers() {
    let mut candidates = [
        weighted_candidate("acct_high", 100, 3),
        weighted_candidate("acct_low", 1, 0),
    ];
    let mut context = context(RotationStrategy::WeightPriority);
    context.preferred_account_overrides_weight = true;
    context.preferred_account = Some(candidates[0].account.id().clone());
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("fallback");
    assert_eq!(selected.candidate().account.id().as_str(), "acct_low");

    context.preferred_account = Some(candidates[1].account.id().clone());
    candidates[0].signals.in_flight = 2;
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("recovered");
    assert_eq!(selected.candidate().account.id().as_str(), "acct_high");

    context.policy = AccountSelectionPolicy::new(
        RotationStrategy::WeightPriority,
        std::num::NonZeroU32::new(3).expect("non-zero concurrency"),
        Duration::from_secs(1),
    );
    candidates[0].signals.last_started_at = Some(context.now);
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("interval fallback");
    assert_eq!(selected.candidate().account.id().as_str(), "acct_low");
    context.now += Duration::from_secs(1);
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("interval elapsed");
    assert_eq!(selected.candidate().account.id().as_str(), "acct_high");
}

#[test]
fn weight_priority_rotates_only_the_highest_tier_without_affinity() {
    let candidates = [
        weighted_candidate("acct_high_b", 100, 2),
        weighted_candidate("acct_low", 1, 0),
        weighted_candidate("acct_high_a", 100, 0),
    ];
    let mut context = context(RotationStrategy::WeightPriority);
    for (cursor, expected) in [(0, "acct_high_a"), (1, "acct_high_b"), (2, "acct_high_a")] {
        context.round_robin_cursor = cursor;
        let selected = AccountSelector
            .select(&candidates, &context)
            .expect("candidate");
        assert_eq!(selected.candidate().account.id().as_str(), expected);
    }
}

#[test]
fn unlimited_account_concurrency_preserves_overrides_intervals_and_finite_scores() {
    use gateway_core::account::{AccountConcurrency, AccountSelectionPolicy};

    for strategy in [
        RotationStrategy::Smart,
        RotationStrategy::WeightPriority,
        RotationStrategy::RoundRobin,
        RotationStrategy::Sticky,
        RotationStrategy::QuotaResetPriority,
    ] {
        let mut context = context(strategy);
        context.policy =
            AccountSelectionPolicy::new(strategy, AccountConcurrency::Unlimited, Duration::ZERO);
        let candidates = [
            candidate("acct_unlimited", u32::MAX, None),
            candidate_with_concurrency("acct_limited", 2, 2),
        ];
        assert_eq!(
            AccountSelector
                .select(&candidates, &context)
                .expect("unlimited remains eligible")
                .candidate()
                .account
                .id()
                .as_str(),
            "acct_unlimited"
        );
        assert!(AccountSelector.select(&candidates[1..], &context).is_none());
        assert!(
            AccountSelector
                .capacity_snapshot(&candidates, &context)
                .is_none()
        );
        let trace = gateway_core::diagnostics::TraceContext::new("req_unlimited_concurrency");
        trace.account_selection(
            &candidates,
            &context,
            AccountSelector.select(&candidates, &context).as_ref(),
        );
        if strategy == RotationStrategy::Smart {
            let snapshot = trace.snapshot().expect("trace");
            assert!(
                snapshot["events"][0]["data"]["candidates"][0]["smartScore"]
                    .as_f64()
                    .expect("finite score")
                    .is_finite()
            );
        }
        context.policy = AccountSelectionPolicy::new(
            strategy,
            AccountConcurrency::Unlimited,
            Duration::from_secs(1),
        );
        let mut recent = candidate("acct_unlimited", u32::MAX, None);
        recent.signals.last_started_at = Some(context.now);
        assert!(AccountSelector.select(&[recent], &context).is_none());
    }
}

#[test]
fn unavailable_unlimited_account_does_not_hide_the_finite_pool_capacity() {
    use gateway_core::account::{
        AccountConcurrency, AccountSelectionPolicy, CredentialState, QuotaState,
    };
    let mut context = context(RotationStrategy::Smart);
    context.policy = AccountSelectionPolicy::new(
        RotationStrategy::Smart,
        AccountConcurrency::Unlimited,
        Duration::ZERO,
    );
    let mut unavailable = candidate("acct_disabled", 0, None);
    unavailable.account = unavailable.account.with_account_facts(
        false,
        CredentialState::Ready,
        QuotaState::unknown(),
        None,
        None,
    );
    let finite = candidate_with_concurrency("acct_limited", 1, 2);
    let capacity = AccountSelector
        .capacity_snapshot(&[unavailable, finite], &context)
        .expect("finite available pool");
    assert_eq!((capacity.used_slots(), capacity.total_slots()), (1, 2));
}

fn feedback_subject() -> (AccountFeedbackStats, ProviderKind, ProviderAccountId) {
    (
        AccountFeedbackStats::default(),
        ProviderKind::new("openai").expect("valid provider"),
        ProviderAccountId::new("acct_decay").expect("valid account"),
    )
}

fn report_failure(
    feedback: &AccountFeedbackStats,
    provider: &ProviderKind,
    account: &ProviderAccountId,
    observed_at: Instant,
) {
    feedback.report_at(
        provider,
        account,
        AccountAttemptFeedback::Failed {
            first_output_ms: None,
        },
        observed_at,
    );
}

#[test]
fn capacity_rejections_should_raise_failure_rate_faster_than_regular_failures() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    let mut failure_rates = Vec::new();
    for _ in 0..2 {
        feedback.report_at(
            &provider,
            &account,
            AccountAttemptFeedback::CapacityRejected {
                first_output_ms: None,
            },
            observed_at,
        );
        failure_rates.push(
            feedback
                .scheduling_signals_at(&provider, &account, observed_at)
                .0,
        );
    }

    assert_eq!(failure_rates, [Some(4_000), Some(6_400)]);
}

#[test]
fn capacity_failure_rate_should_keep_time_decay_and_success_recovery() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    feedback.report_at(
        &provider,
        &account,
        AccountAttemptFeedback::CapacityRejected {
            first_output_ms: Some(100),
        },
        observed_at,
    );
    let recovered_at = observed_at + FAILURE_RATE_HALF_LIFE;
    feedback.report_at(
        &provider,
        &account,
        AccountAttemptFeedback::Succeeded {
            first_output_ms: Some(200),
        },
        recovered_at,
    );

    assert_eq!(
        feedback.scheduling_signals_at(&provider, &account, recovered_at),
        (Some(1_600), Some(120)),
    );
}

#[test]
fn account_failure_rate_should_halve_after_one_half_life() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    report_failure(&feedback, &provider, &account, observed_at);

    let failure_rate = feedback
        .scheduling_signals_at(&provider, &account, observed_at + FAILURE_RATE_HALF_LIFE)
        .0;

    assert_eq!(failure_rate, Some(1_000));
}

#[test]
fn account_failure_rate_should_quarter_after_two_half_lives() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    report_failure(&feedback, &provider, &account, observed_at);

    let failure_rate = feedback
        .scheduling_signals_at(
            &provider,
            &account,
            observed_at + FAILURE_RATE_HALF_LIFE * 2,
        )
        .0;

    assert_eq!(failure_rate, Some(500));
}

#[test]
fn account_failure_rate_should_decay_before_applying_a_new_sample() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    report_failure(&feedback, &provider, &account, observed_at);
    feedback.report_at(
        &provider,
        &account,
        AccountAttemptFeedback::Succeeded {
            first_output_ms: None,
        },
        observed_at + FAILURE_RATE_HALF_LIFE,
    );

    let failure_rate = feedback
        .scheduling_signals_at(&provider, &account, observed_at + FAILURE_RATE_HALF_LIFE)
        .0;

    assert_eq!(failure_rate, Some(800));
}

#[test]
fn concurrent_account_failures_should_not_lose_samples() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| report_failure(&feedback, &provider, &account, observed_at));
        }
    });

    let failure_rate = feedback
        .scheduling_signals_at(&provider, &account, observed_at)
        .0;

    assert_eq!(failure_rate, Some(8_322));
}
fn smart_selection_ids(candidates: &[AccountCandidate]) -> Vec<&str> {
    let mut selection = context(RotationStrategy::Smart);
    (0..20)
        .map(|cursor| {
            selection.round_robin_cursor = cursor;
            AccountSelector
                .select(candidates, &selection)
                .expect("candidate available")
                .candidate()
                .account
                .id()
                .as_str()
        })
        .collect()
}

#[test]
fn smart_selector_should_rotate_despite_small_signal_differences() {
    for signal in ["quota", "latency", "load", "failure"] {
        let mut candidates = [
            candidate_with_concurrency("acct_a", 0, 100),
            candidate_with_concurrency("acct_b", 0, 100),
        ];
        match signal {
            "quota" => {
                candidates[0].signals.quota_remaining_rank = Some(600);
                candidates[1].signals.quota_remaining_rank = Some(601);
            }
            "latency" => {
                candidates[0].signals.first_output_latency_ms = Some(2_500);
                candidates[1].signals.first_output_latency_ms = Some(2_501);
            }
            "load" => candidates[0].signals.in_flight = 1,
            "failure" => candidates[0].signals.failure_rate_basis_points = Some(1),
            _ => unreachable!(),
        }
        let selected = smart_selection_ids(&candidates);

        assert_eq!(selected, ["acct_a", "acct_b"].repeat(10), "{signal}");
    }
}

#[test]
fn smart_selector_should_keep_rotation_order_when_nearby_scores_cross() {
    let mut candidates = [
        candidate("acct_a", 0, Some(8_000)),
        candidate("acct_b", 0, Some(8_001)),
        candidate("acct_worse", 0, Some(2_000)),
    ];
    let mut selection = context(RotationStrategy::Smart);
    let mut selected = Vec::new();
    for cursor in 0..20 {
        selection.round_robin_cursor = cursor;
        for candidate in &mut candidates {
            match candidate.account.id().as_str() {
                "acct_a" => candidate.signals.quota_remaining_rank = Some(8_000 + cursor % 2),
                "acct_b" => candidate.signals.quota_remaining_rank = Some(8_001 - cursor % 2),
                _ => {}
            }
        }
        candidates.reverse();
        selected.push(
            AccountSelector
                .select(&candidates, &selection)
                .expect("candidate available")
                .candidate()
                .account
                .id()
                .as_str()
                .to_owned(),
        );
    }

    assert_eq!(selected, ["acct_a", "acct_b"].repeat(10));
}

#[test]
fn smart_selector_should_only_rotate_among_candidates_close_to_the_best() {
    let candidates = [
        candidate("acct_a", 0, Some(10_000)),
        candidate("acct_b", 0, Some(9_500)),
        candidate("acct_c", 0, Some(9_000)),
    ];

    assert_eq!(
        smart_selection_ids(&candidates),
        ["acct_a", "acct_b"].repeat(10)
    );
}

#[test]
fn smart_selector_should_preserve_material_signal_advantages() {
    for signal in ["quota", "latency", "load", "failure"] {
        let mut candidates = [
            candidate_with_concurrency("acct_a", 0, 10),
            candidate_with_concurrency("acct_b", 0, 10),
        ];
        match signal {
            "quota" => {
                candidates[0].signals.quota_remaining_rank = Some(600);
                candidates[1].signals.quota_remaining_rank = Some(2_600);
            }
            "latency" => {
                candidates[0].signals.first_output_latency_ms = Some(20_000);
                candidates[1].signals.first_output_latency_ms = Some(2_500);
            }
            "load" => candidates[0].signals.in_flight = 1,
            "failure" => candidates[0].signals.failure_rate_basis_points = Some(2_000),
            _ => unreachable!(),
        }

        assert_eq!(smart_selection_ids(&candidates), ["acct_b"; 20], "{signal}");
    }
}

#[test]
fn smart_selector_should_balance_actual_load_against_remaining_quota() {
    let mut candidates = [
        candidate_with_concurrency("acct_a", 0, 10),
        candidate_with_concurrency("acct_b", 1, 10),
    ];
    candidates[0].signals.quota_remaining_rank = Some(600);
    candidates[1].signals.quota_remaining_rank = Some(2_600);

    assert_eq!(smart_selection_ids(&candidates), ["acct_b"; 20]);
}

#[test]
fn smart_selector_should_treat_unknown_quota_as_neutral() {
    let candidates = [
        candidate("acct_known_low", 0, Some(600)),
        candidate("acct_unknown", 0, None),
    ];

    assert_eq!(smart_selection_ids(&candidates), ["acct_unknown"; 20]);
}
