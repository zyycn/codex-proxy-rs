//! 验证重置窗口独立排序、相对系数及亲和与可用性边界

use std::time::Duration;

use gateway_core::account::{
    AccountQuotaResetWindow, AccountSelector, QuotaResetPeriod, ResetWindowWeights,
    RotationStrategy,
};

use super::{candidate, context};

#[test]
fn selected_windows_rank_strictly_without_mixing_runtime_scores_or_rotating_near_ties() {
    let mut context = context(RotationStrategy::QuotaResetPriority);
    let mut early_weekly = candidate("acct_early_weekly", 2, Some(100));
    early_weekly.signals.failure_rate_basis_points = Some(9_000);
    early_weekly.signals.first_output_latency_ms = Some(20_000);
    let mut late_weekly = candidate("acct_late_weekly", 0, Some(9_900));
    late_weekly.signals.failure_rate_basis_points = Some(0);
    late_weekly.signals.first_output_latency_ms = Some(10);
    for (candidate, short, weekly) in [
        (&mut early_weekly, 14_400, 6 * 86_400),
        (&mut late_weekly, 3_600, 6 * 86_400 + 43_200),
    ] {
        candidate.signals.quota_reset_at = Some(context.now + Duration::from_secs(short));
        candidate.signals.quota_reset_windows = Some(
            [
                (QuotaResetPeriod::FiveHour, short),
                (QuotaResetPeriod::Weekly, weekly),
            ]
            .map(|(period, seconds)| AccountQuotaResetWindow {
                period,
                reset_at: context.now + Duration::from_secs(seconds),
            })
            .into(),
        );
    }
    let mut candidates = [early_weekly, late_weekly];
    for (weights, expected) in [
        ([0.0, 0.0, 0.0], "acct_late_weekly"),
        ([1.0, 0.0, 0.0], "acct_late_weekly"),
        ([0.0, 0.1, 0.0], "acct_early_weekly"),
        ([0.0, 1.0, 0.0], "acct_early_weekly"),
        ([0.0, 10.0, 0.0], "acct_early_weekly"),
        ([1.0, 0.1, 0.0], "acct_late_weekly"),
        ([10.0, 1.0, 0.0], "acct_late_weekly"),
        ([0.1, 10.0, 0.0], "acct_early_weekly"),
    ] {
        context.policy = context
            .policy
            .with_quota_reset_window_weights(ResetWindowWeights::new(weights).unwrap());
        for cursor in 0..6 {
            context.round_robin_cursor = cursor;
            assert_eq!(
                AccountSelector
                    .select(&candidates, &context)
                    .unwrap()
                    .candidate()
                    .account
                    .id()
                    .as_str(),
                expected,
                "weights {weights:?}, cursor {cursor}"
            );
        }
    }
    context.preferred_account = Some(candidates[1].account.id().clone());
    let selected = AccountSelector.select(&candidates, &context).unwrap();
    assert_eq!(
        selected.candidate().account.id().as_str(),
        "acct_late_weekly"
    );
    assert_eq!(
        selected.preferred(),
        gateway_core::account::PreferredAccountSelection::Hit
    );

    context.preferred_account = None;
    candidates[0].signals.in_flight = 3;
    assert_eq!(
        AccountSelector
            .select(&candidates, &context)
            .unwrap()
            .candidate()
            .account
            .id()
            .as_str(),
        "acct_late_weekly"
    );
}

#[test]
fn missing_or_elapsed_selected_windows_do_not_fall_back_to_disabled_periods() {
    let mut context = context(RotationStrategy::QuotaResetPriority);
    context.policy = context
        .policy
        .with_quota_reset_window_weights(ResetWindowWeights::new([0.0, 0.0, 1.0]).unwrap());
    let mut known = candidate("acct_known_monthly", 1, None);
    known.signals.quota_reset_windows = Some(
        vec![AccountQuotaResetWindow {
            period: QuotaResetPeriod::Monthly,
            reset_at: context.now + Duration::from_secs(30 * 86_400),
        }]
        .into(),
    );
    for reset_at in [
        None,
        Some(context.now),
        Some(context.now - Duration::from_secs(1)),
    ] {
        let mut missing = candidate("acct_earlier_other_period", 0, None);
        missing.signals.quota_reset_at = Some(context.now + Duration::from_secs(1));
        missing.signals.quota_reset_windows = Some(
            reset_at
                .into_iter()
                .map(|reset_at| AccountQuotaResetWindow {
                    period: QuotaResetPeriod::Monthly,
                    reset_at,
                })
                .collect::<Vec<_>>()
                .into(),
        );
        let candidates = [missing, known.clone()];
        assert_eq!(
            AccountSelector
                .select(&candidates, &context)
                .unwrap()
                .candidate()
                .account
                .id()
                .as_str(),
            "acct_known_monthly"
        );
    }
}
