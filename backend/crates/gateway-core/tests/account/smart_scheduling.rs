use gateway_core::account::{
    AccountSelector, PreferredAccountSelection, RotationStrategy, SmartSchedulingConfig,
};
use serde_json::json;
use std::time::{Duration, Instant, SystemTime};

use futures::FutureExt;
use gateway_core::concurrency::{
    CapacityWait, ConcurrencyQueuePolicy, ConcurrencyWaitBudget, ConcurrencyWaitQueue,
};

use super::{candidate_with_concurrency, context, weighted_candidate};

#[test]
fn smart_config_rejects_invalid_weights_and_incomplete_wire_values() {
    for value in [-0.1, 10.1, 0.01, f64::NAN, f64::INFINITY] {
        for dimension in 0..6 {
            let mut weights = [1.0; 6];
            weights[dimension] = value;
            assert!(SmartSchedulingConfig::new(weights, false).is_err());
        }
    }
    assert!(SmartSchedulingConfig::new([0.0; 6], true).is_err());
    for value in [json!({}), json!(null), json!({"loadWeight": 1.0})] {
        assert!(serde_json::from_value::<SmartSchedulingConfig>(value).is_err());
    }
    let config = SmartSchedulingConfig::new([0.0, 0.1, 9.9, 10.0, 1.2, 2.3], true).unwrap();
    let encoded = serde_json::to_value(config).unwrap();
    assert_eq!(
        serde_json::from_value::<SmartSchedulingConfig>(encoded).unwrap(),
        config
    );
}

#[test]
fn reset_weight_rewards_only_future_resets_and_preserves_capacity_checks() {
    let mut context = context(RotationStrategy::Smart);
    context.policy = context.policy.with_smart_scheduling(
        SmartSchedulingConfig::new([0.0, 0.0, 0.0, 0.0, 1.0, 0.0], false).unwrap(),
    );
    let mut candidates = [
        candidate_with_concurrency("acct_unknown", 0, 2),
        candidate_with_concurrency("acct_past", 0, 2),
        candidate_with_concurrency("acct_now", 0, 2),
        candidate_with_concurrency("acct_near", 0, 2),
        candidate_with_concurrency("acct_far", 0, 2),
    ];
    candidates[1].signals.quota_reset_at = Some(context.now - Duration::from_secs(1));
    candidates[2].signals.quota_reset_at = Some(context.now);
    candidates[3].signals.quota_reset_at = Some(context.now + Duration::from_secs(3600));
    candidates[4].signals.quota_reset_at = Some(context.now + Duration::from_secs(86_400));
    assert_eq!(
        AccountSelector
            .select(&candidates, &context)
            .unwrap()
            .candidate()
            .account
            .id()
            .as_str(),
        "acct_near"
    );
    let trace = gateway_core::diagnostics::TraceContext::new("req_reset_scores");
    trace.account_selection(&candidates, &context, None);
    let snapshot = trace.snapshot().unwrap();
    let scores = snapshot["events"][0]["data"]["candidates"]
        .as_array()
        .unwrap();
    for (candidate, expected) in scores.iter().zip([0.0, 0.0, 0.0, 0.5, 0.04]) {
        assert_eq!(candidate["smartScore"].as_f64().unwrap(), expected);
    }
    candidates[3].signals.in_flight = 2;
    assert_eq!(
        AccountSelector
            .select(&candidates, &context)
            .unwrap()
            .candidate()
            .account
            .id()
            .as_str(),
        "acct_far"
    );
    candidates[3].signals.in_flight = 0;
    context.now += Duration::from_secs(3600);
    assert_eq!(
        AccountSelector
            .select(&candidates, &context)
            .unwrap()
            .candidate()
            .account
            .id()
            .as_str(),
        "acct_far"
    );
}

#[test]
fn queue_weight_balances_live_pressure_and_health_only_when_enabled_for_smart() {
    for (strategy, queue_weight, expects_shortest) in [
        (RotationStrategy::Smart, 0.0, true),
        (RotationStrategy::Smart, 0.2, false),
        (RotationStrategy::Smart, 2.0, true),
        (RotationStrategy::RoundRobin, 0.2, true),
        (RotationStrategy::QuotaResetPriority, 0.2, true),
        (RotationStrategy::Sticky, 0.2, true),
    ] {
        let mut candidates = [
            candidate_with_concurrency("acct_healthy", 2, 2),
            candidate_with_concurrency("acct_shortest", 2, 2),
        ];
        candidates[1].signals.failure_rate_basis_points = Some(6000);
        let mut context = context(strategy);
        context.policy = context.policy.with_smart_scheduling(
            SmartSchedulingConfig::new([0.0, 0.0, 1.0, 0.0, 0.0, queue_weight], false).unwrap(),
        );
        let queue = ConcurrencyWaitQueue::default();
        let head = queue
            .enqueue(
                &[candidates[0].account.id().clone()],
                2,
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
        let budget = ConcurrencyWaitBudget::default();
        let policy = ConcurrencyQueuePolicy {
            max_waiting: 2,
            timeout: Duration::from_secs(2),
        };
        let mut waiting = CapacityWait::new(
            &queue,
            policy,
            SystemTime::now() + Duration::from_secs(2),
            &budget,
        );
        let keys = AccountSelector.wait_candidates(&candidates, &context);
        assert!(
            AccountSelector
                .wait_for_capacity(&mut waiting, &keys, &candidates, &context)
                .now_or_never()
                .is_none()
        );
        assert_eq!(
            queue.has_waiters(candidates[1].account.id()),
            expects_shortest,
            "{strategy:?}, {queue_weight}"
        );
        // 新请求不能绕过已经选定的队列，取消请求同步归还位置。
        let newcomer = CapacityWait::new(
            &queue,
            policy,
            SystemTime::now() + Duration::from_secs(2),
            &budget,
        );
        assert!(!newcomer.can_try(candidates[usize::from(expects_shortest)].account.id()));
        drop(waiting);
        assert!(!queue.has_waiters(candidates[1].account.id()));
        drop(head);
        assert!(!queue.has_waiters(candidates[0].account.id()));
    }
}

#[test]
fn queue_weight_does_not_widen_immediate_selection_tolerance() {
    let mut candidates = [
        candidate_with_concurrency("acct_a", 0, 2),
        candidate_with_concurrency("acct_b", 0, 2),
    ];
    candidates[0].signals.quota_remaining_rank = Some(10000);
    candidates[1].signals.quota_remaining_rank = Some(9000);
    let mut context = context(RotationStrategy::Smart);
    context.round_robin_cursor = 1;
    for queue_weight in [0.0, 10.0] {
        context.policy = context.policy.with_smart_scheduling(
            SmartSchedulingConfig::new([0.0, 1.0, 0.0, 0.0, 0.0, queue_weight], false).unwrap(),
        );
        assert_eq!(
            AccountSelector
                .select(&candidates, &context)
                .unwrap()
                .candidate()
                .account
                .id()
                .as_str(),
            "acct_a"
        );
    }
    context.policy = context.policy.with_smart_scheduling(
        SmartSchedulingConfig::new([0.0, 0.0, 0.0, 0.0, 0.0, 1.0], false).unwrap(),
    );
    // 仅开启排队系数时，立即可用的同权重账号按既有游标轮换。
    assert_eq!(
        AccountSelector
            .select(&candidates, &context)
            .unwrap()
            .candidate()
            .account
            .id()
            .as_str(),
        "acct_b"
    );
}

#[test]
fn each_smart_weight_controls_its_signal_without_relaxing_capacity() {
    let mut candidates = [
        candidate_with_concurrency("acct_idle", 0, 10),
        candidate_with_concurrency("acct_healthy", 5, 10),
    ];
    candidates[0].signals.quota_remaining_rank = Some(1_000);
    candidates[1].signals.quota_remaining_rank = Some(9_000);
    candidates[0].signals.failure_rate_basis_points = Some(5_000);
    candidates[1].signals.failure_rate_basis_points = Some(0);
    candidates[0].signals.first_output_latency_ms = Some(30_000);
    candidates[1].signals.first_output_latency_ms = Some(1_000);
    for dimension in 0..4 {
        let mut weights = [0.0; 6];
        weights[dimension] = 1.0;
        let mut context = context(RotationStrategy::Smart);
        context.policy = context
            .policy
            .with_smart_scheduling(SmartSchedulingConfig::new(weights, false).unwrap());
        let selected = AccountSelector.select(&candidates, &context).unwrap();
        assert_eq!(
            selected.candidate().account.id().as_str(),
            if dimension == 0 {
                "acct_idle"
            } else {
                "acct_healthy"
            }
        );
        let mut saturated = candidates.clone();
        saturated[1].signals.in_flight = 10;
        assert_eq!(
            AccountSelector
                .select(&saturated, &context)
                .unwrap()
                .candidate()
                .account
                .id()
                .as_str(),
            "acct_idle"
        );
    }
}

#[test]
fn proportional_coefficients_preserve_near_best_rotation_and_diagnostics() {
    let mut candidates = [
        candidate_with_concurrency("acct_a", 0, 100),
        candidate_with_concurrency("acct_b", 0, 100),
        candidate_with_concurrency("acct_c", 0, 100),
    ];
    // 默认额度权重 0.8 下，9375 正好位于容差边界，9374 刚好超出。
    for (candidate, quota) in candidates.iter_mut().zip([10_000, 9_375, 9_374]) {
        candidate.signals.quota_remaining_rank = Some(quota);
    }
    for scale in [1.0, 2.0, 5.0] {
        let mut context = context(RotationStrategy::Smart);
        context.policy = context.policy.with_smart_scheduling(
            SmartSchedulingConfig::new([scale, 0.8 * scale, scale, 0.5 * scale, 0.0, 0.0], false)
                .unwrap(),
        );
        let mut ids = Vec::new();
        for cursor in 0..4 {
            candidates.reverse();
            context.round_robin_cursor = cursor;
            ids.push(
                AccountSelector
                    .select(&candidates, &context)
                    .unwrap()
                    .candidate()
                    .account
                    .id()
                    .as_str()
                    .to_owned(),
            );
        }
        assert_eq!(ids, ["acct_a", "acct_b", "acct_a", "acct_b"]);
        let trace = gateway_core::diagnostics::TraceContext::new("req_scaled_weights");
        trace.account_selection(&candidates, &context, None);
        let snapshot = trace.snapshot().unwrap();
        let tolerance = snapshot["events"][0]["data"]["smartScoreTolerance"]
            .as_f64()
            .unwrap();
        assert!((tolerance - 0.05 * scale).abs() < 1e-12);
    }
}

#[test]
fn switchback_is_opt_in_and_only_affects_smart_soft_affinity() {
    let mut candidates = [
        weighted_candidate("acct_affinity", 10, 0),
        weighted_candidate("acct_primary", 100, 0),
    ];
    for strategy in [
        RotationStrategy::Smart,
        RotationStrategy::Sticky,
        RotationStrategy::RoundRobin,
        RotationStrategy::QuotaResetPriority,
    ] {
        let mut context = context(strategy);
        context.preferred_account = Some(candidates[0].account.id().clone());
        context.preferred_account_overrides_weight = true;
        for enabled in [false, true] {
            context.policy = context.policy.with_smart_scheduling(
                SmartSchedulingConfig::new([1.0, 0.8, 1.0, 0.5, 0.0, 0.0], enabled).unwrap(),
            );
            let selected = AccountSelector.select(&candidates, &context).unwrap();
            let should_switch = enabled && strategy == RotationStrategy::Smart;
            assert_eq!(
                selected.candidate().account.id(),
                candidates[usize::from(should_switch)].account.id()
            );
        }
    }
    let mut context = context(RotationStrategy::Smart);
    context.policy = context.policy.with_smart_scheduling(
        SmartSchedulingConfig::new([1.0, 0.8, 1.0, 0.5, 0.0, 0.0], true).unwrap(),
    );
    context.preferred_account_overrides_weight = true;
    context.preferred_account = Some(candidates[0].account.id().clone());
    candidates[1].signals.in_flight = 3;
    assert_eq!(
        AccountSelector
            .select(&candidates, &context)
            .unwrap()
            .preferred(),
        PreferredAccountSelection::Hit
    );
    candidates[1].signals.in_flight = 0;
    context
        .excluded_accounts
        .insert(candidates[1].account.id().clone());
    assert_eq!(
        AccountSelector
            .select(&candidates, &context)
            .unwrap()
            .preferred(),
        PreferredAccountSelection::Hit
    );
    context.excluded_accounts.clear();
    candidates[0] = weighted_candidate("acct_affinity", 100, 2);
    assert_eq!(
        AccountSelector
            .select(&candidates, &context)
            .unwrap()
            .preferred(),
        PreferredAccountSelection::Hit
    );
}
