use chrono::{DateTime, Duration, Utc};
use gateway_admin::model::quota_forecast_sampling::{
    QuotaForecastMethod, QuotaForecastPoint, QuotaForecastSample, QuotaForecastUsage,
    select_forecast_sample,
};

fn time(minutes: i64) -> DateTime<Utc> {
    "2026-09-12T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::minutes(minutes)
}

fn point(minute: i64, percent: f64, tokens: u64) -> QuotaForecastPoint {
    QuotaForecastPoint {
        observed_at: time(minute),
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
    select_forecast_sample("week".to_owned(), time(0), current, points, 2)
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
fn large_percentage_reversal_restarts_sampling_without_crossing_old_consumption() {
    let sample = select(
        vec![point(1, 40.0, 100), point(2, 50.0, 200), point(3, 1.0, 210)],
        point(4, 8.0, 280),
    );
    assert_eq!(sample.baseline_percent, 1.0);
    assert_eq!(sample.usage.tokens, 70);
    assert_eq!(sample.sampled_percent, 7.0);
    assert!(!sample.discontinuous);
    let waiting = select(
        vec![point(1, 40.0, 100), point(2, 50.0, 200)],
        point(3, 2.0, 210),
    );
    assert!(waiting.discontinuous);
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
