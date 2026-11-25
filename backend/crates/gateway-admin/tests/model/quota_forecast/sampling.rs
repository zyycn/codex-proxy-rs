//! 验证额度预测采样的去重、回退处理与近期窗口选择

use chrono::{DateTime, Duration, Utc};
use gateway_admin::model::quota_forecast::{
    QuotaForecastMethod, QuotaForecastPoint, QuotaForecastSample, QuotaForecastUsage,
    select_forecast_sample,
};

fn time(minutes: i64) -> DateTime<Utc> {
    "2026-09-12T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::minutes(minutes)
}

fn point(minute: i64, percent: f64, tokens: u64) -> QuotaForecastPoint {
    QuotaForecastPoint {
        started_at: time(minute) - Duration::seconds(1),
        observed_at: time(minute),
        reset_at: time(7 * 24 * 60),
        used_percent: percent,
        usage: QuotaForecastUsage {
            request_count: minute as u64,
            tokens,
            input_tokens: tokens,
            known_cost_count: minute as u64,
            usd: tokens as f64 / 1_000.0,
            ..Default::default()
        },
    }
}

fn select(points: Vec<QuotaForecastPoint>, current: QuotaForecastPoint) -> QuotaForecastSample {
    select_forecast_sample("week".to_owned(), time(0), current, points, 2, false)
}

#[test]
fn repeated_readings_do_not_make_independent_blocks_or_shift_the_baseline() {
    let points = (1..100)
        .map(|minute| point(minute, 20.0, minute as u64 * 100))
        .collect();
    let sample = select(points, point(100, 25.0, 10_000));
    assert_eq!(sample.method, QuotaForecastMethod::Incremental);
    assert_eq!(sample.block_count, 1);
    assert_eq!(sample.start_at, time(1));
    assert_eq!(sample.sampled_percent, 5.0);
    assert_eq!(sample.usage.tokens, 9_900);
    assert_eq!(sample.pending_request_count, 2);
}

#[test]
fn recent_blocks_and_tail_replace_old_workload_without_averaging_individual_ratios() {
    let points = vec![
        point(1, 0.0, 0),
        point(2, 5.0, 100),
        point(3, 10.0, 200),
        point(4, 15.0, 400),
        point(5, 20.0, 600),
        point(6, 25.0, 800),
    ];
    let sample = select(points, point(7, 27.0, 900));
    assert_eq!(sample.baseline_percent, 10.0);
    assert_eq!(sample.sampled_percent, 17.0);
    assert_eq!(sample.usage.tokens, 700);
    assert_eq!(sample.cycle_usage.tokens, 900);
    assert_eq!(sample.block_count, 3);
}

#[test]
fn snapshot_at_a_block_boundary_keeps_only_three_recent_blocks() {
    let points = (0..6)
        .map(|i| point(i + 1, i as f64 * 5.0, i as u64 * 100))
        .collect();
    let sample = select(points, point(7, 30.0, 600));
    assert_eq!(sample.baseline_percent, 15.0);
    assert_eq!(sample.sampled_percent, 15.0);
    assert_eq!(sample.usage.tokens, 300);
}

#[test]
fn large_percentage_reversal_restarts_only_sampling_without_crossing_old_consumption() {
    let sample = select(
        vec![point(1, 40.0, 100), point(2, 50.0, 200), point(3, 1.0, 210)],
        point(4, 8.0, 280),
    );
    assert_eq!(sample.baseline_percent, 1.0);
    assert_eq!(sample.usage.tokens, 70);
    assert_eq!(sample.cycle_usage.tokens, 280);
    assert_eq!(sample.sampled_percent, 7.0);
    assert!(!sample.discontinuous);
    let waiting = select(
        vec![point(1, 40.0, 100), point(2, 50.0, 200)],
        point(3, 2.0, 210),
    );
    assert!(waiting.discontinuous);
}

#[test]
fn same_window_reversal_preserves_all_recorded_cycle_usage() {
    let current = point(3, 35.0, 140);
    let recorded = current.usage.clone();
    let sample = select(vec![point(1, 32.0, 100), point(2, 30.0, 120)], current);
    assert_eq!(sample.cycle_usage, recorded);
    assert_eq!(sample.usage.tokens, 20);
    assert!((sample.usage.usd - 0.02).abs() < 1e-10);
    assert!(!sample.discontinuous);
}

#[test]
fn small_reversals_after_full_usage_preserve_cycle_totals_and_cumulative_forecasts() {
    let current = point(3, 99.5, 10_020);
    let recorded = current.usage.clone();
    let sample = select(
        vec![point(1, 100.0, 10_000), point(2, 99.0, 10_010)],
        current,
    );
    assert_eq!(sample.cycle_usage, recorded);
    assert_eq!(sample.method, QuotaForecastMethod::Cumulative);
    assert!(!sample.discontinuous);
}

#[test]
fn one_point_reversal_is_skipped_without_counting_an_extra_block() {
    let sample = select(
        vec![
            point(1, 10.0, 100),
            point(2, 15.0, 200),
            point(3, 14.0, 210),
        ],
        point(4, 20.0, 300),
    );
    assert_eq!(sample.block_count, 2);
    assert_eq!(sample.usage.tokens, 200);
    assert!(!sample.discontinuous);
}

#[test]
fn missing_token_and_cost_counts_are_local_to_the_selected_interval() {
    let mut baseline = point(1, 30.0, 1_000);
    baseline.usage.missing_token_count = 2;
    baseline.usage.unavailable_cost_count = 3;
    let mut end = point(3, 40.0, 2_000);
    end.usage.missing_token_count = 2;
    end.usage.unavailable_cost_count = 4;
    let sample = select(vec![baseline], end);
    assert_eq!(sample.usage.missing_token_count, 0);
    assert_eq!(sample.usage.unavailable_cost_count, 1);
}

#[test]
fn counter_regressions_fail_closed_and_future_points_are_not_used() {
    let invalid = select(vec![point(1, 10.0, 1_000)], point(2, 20.0, 500));
    assert!(invalid.discontinuous);
    let future = select(vec![point(10, 1.0, 1)], point(2, 20.0, 500));
    assert_eq!(future.method, QuotaForecastMethod::Cumulative);
    assert_eq!(future.observation_count, 0);
}

#[test]
fn same_window_recovery_restarts_sampling_but_preserves_recorded_cycle_usage() {
    let sample = select(
        vec![point(1, 100.0, 10_000), point(2, 0.0, 10_000)],
        point(3, 10.0, 10_100),
    );
    assert_eq!(sample.cycle_usage.tokens, 10_100);
    assert!((sample.cycle_usage.usd - 10.1).abs() < 1e-10);
    assert_eq!(sample.usage.tokens, 100);
    assert_eq!(sample.sampled_percent, 10.0);
    assert!(!sample.discontinuous);
}

#[test]
fn a_single_non_exhausted_observation_does_not_restart_cycle_usage() {
    let sample = select(vec![point(1, 100.0, 10_000)], point(2, 0.0, 10_000));
    assert_eq!(sample.cycle_usage.tokens, 10_000);
    assert!(sample.discontinuous);
}

#[test]
fn repeated_same_window_recoveries_preserve_recorded_cycle_usage() {
    let sample = select(
        vec![
            point(1, 100.0, 10_000),
            point(2, 0.0, 10_000),
            point(3, 100.0, 10_100),
            point(4, 0.0, 10_100),
        ],
        point(5, 10.0, 10_200),
    );
    assert_eq!(sample.cycle_usage.tokens, 10_200);
    assert_eq!(sample.usage.tokens, 100);
    assert!(!sample.discontinuous);
}

#[test]
fn an_expired_deadline_alone_cannot_cut_totals_or_anchor_remaining_usage() {
    let mut older = point(1, 15.0, 100);
    older.reset_at = time(3);
    let mut previous = point(2, 20.0, 200);
    previous.reset_at = time(3) + Duration::seconds(4);
    let first = point(4, 25.0, 700);
    let sample = select(vec![older, previous, first], point(6, 35.0, 900));
    assert_eq!(sample.cycle_usage.tokens, 900);
    assert_eq!(sample.baseline_percent, 25.0);
    assert_eq!(sample.usage.tokens, 200);
}

#[test]
fn copied_concurrent_or_contradictory_readings_cannot_confirm_a_recovery() {
    let mut older = point(1, 75.0, 100);
    older.reset_at = time(3);
    let mut previous = point(2, 85.0, 200);
    previous.reset_at = time(3);
    let first = point(4, 5.0, 300);
    let current = point(6, 40.0, 900);
    let mut overlapping = point(5, 20.0, 400);
    overlapping.started_at = first.started_at;
    for extra in [None, Some(overlapping), Some(current.clone())] {
        let mut points = vec![older.clone(), previous.clone(), first.clone()];
        points.extend(extra);
        let sample = select(points, current.clone());
        assert_eq!(sample.cycle_usage.tokens, 900);
        assert!(sample.discontinuous);
    }
    let sample = select(vec![older, previous, first, point(5, 86.0, 400)], current);
    assert_eq!(sample.cycle_usage.tokens, 900);
}

#[test]
fn changed_plan_restarts_cycle_totals_at_first_matching_observation() {
    let sample = select_forecast_sample(
        "week".to_owned(),
        time(0),
        point(4, 12.0, 10_120),
        vec![point(2, 0.0, 10_000), point(3, 5.0, 10_050)],
        0,
        true,
    );
    assert_eq!(sample.cycle_usage.tokens, 120);
    assert!((sample.cycle_usage.usd - 0.12).abs() < 1e-10);
    assert_eq!(sample.usage.tokens, 120);
    assert!(!sample.discontinuous);
    let waiting = select_forecast_sample(
        "week".to_owned(),
        time(0),
        point(4, 2.0, 10_020),
        vec![],
        0,
        true,
    );
    assert_eq!(waiting.cycle_usage.tokens, 0);
    assert!(waiting.discontinuous);
}
