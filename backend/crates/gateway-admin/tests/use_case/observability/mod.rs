//! 验证管理观测查询的时间范围、健康投影与运行态聚合

use std::{
    str::FromStr as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Timelike as _, Utc};

use gateway_admin::{
    AdminServices,
    model::{
        MutationContext, Revision,
        observability::{
            AccountPoolMetrics, AnalyticsState, AttemptMetrics, CostCoverage, CurrencyCost,
            DashboardAccountUsage, DashboardObservation, DashboardQuery, DashboardRuntimeSlots,
            DiagnosticDimension, DiagnosticObservation, DiagnosticsObservation, Granularity,
            HealthStatus, LatencyPercentiles, ObservabilityPageSize, OpsErrorPage, OpsErrorQuery,
            PercentileMilliseconds, RequestMetricPoint, RequestMetrics, TimeRange, TrendKind,
            UsageBilling, UsageDetail, UsageFilter, UsageInsightsObservation, UsageListRecord,
            UsageOverview, UsagePage, UsageProjectedCostPoint, UsageQuery,
        },
        settings::{
            AdminApiKey, AdminApiKeyMutation, ReplaceRuntimeSettings, RotationStrategy,
            RuntimeSettings,
        },
    },
    ports::store::{AdminStoreResult, ObservabilityStore, SettingsStore},
};

#[test]
fn external_observability_range_accepts_exactly_366_days() {
    let end = Utc::now();
    let range = TimeRange::new(end - Duration::days(366), end)
        .expect("366-day external range should be accepted");
    assert_eq!(range.end, end);
}

#[test]
fn external_observability_range_rejects_over_366_days_and_reversed_range() {
    let end = Utc::now();
    assert!(TimeRange::new(end - Duration::days(367), end).is_err());
    assert!(TimeRange::new(end, end).is_err());
    assert!(TimeRange::new(end + Duration::seconds(1), end).is_err());
}

#[tokio::test]
async fn health_timeline_should_keep_exactly_china_day_quarter_hour_slots() {
    let now = Utc::now();
    let day_start = gateway_core::time::DeploymentTimeZone::default()
        .day_start(now)
        .unwrap();
    let current_slot = quarter_hour_start(now);
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    store.replace_trend(vec![
        health_metric_point(
            day_start - Duration::minutes(15),
            RequestMetrics {
                success_count: 100,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            current_slot,
            RequestMetrics {
                success_count: 2,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            current_slot + Duration::minutes(15),
            RequestMetrics {
                success_count: 100,
                ..RequestMetrics::default()
            },
        ),
    ]);
    let services = observability_services(store).await;

    let timeline = services
        .observability()
        .dashboard_summary(observation_range(now), TrendKind::Usage)
        .await
        .expect("dashboard summary")
        .health_timeline;

    assert_eq!(timeline.points.len(), 96);
    assert_eq!(
        timeline.points.first().map(|point| point.bucket_start),
        Some(day_start)
    );
    assert_eq!(
        timeline.points.last().map(|point| point.bucket_start),
        Some(day_start + Duration::minutes(95 * 15))
    );
    assert_eq!(timeline.success_requests, 2);
    assert_eq!(
        timeline
            .points
            .iter()
            .find(|point| point.bucket_start == current_slot)
            .map(|point| point.status),
        Some(HealthStatus::LowSample)
    );
    assert!(timeline.points.iter().all(|point| {
        point.bucket_start <= quarter_hour_start(Utc::now())
            || (point.status == HealthStatus::Future && point.success_requests == 0)
    }));
}

#[tokio::test]
async fn dashboard_summary_should_derive_first_token_average_from_trend() {
    let now = Utc::now();
    let current_slot = quarter_hour_start(now);
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    store.replace_trend(vec![
        health_metric_point(
            current_slot - Duration::minutes(15),
            RequestMetrics {
                first_token_latency_sum_ms: 120,
                first_token_latency_count: 2,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            current_slot,
            RequestMetrics {
                first_token_latency_sum_ms: 180,
                first_token_latency_count: 1,
                ..RequestMetrics::default()
            },
        ),
    ]);
    let services = observability_services(store).await;

    let summary = services
        .observability()
        .dashboard_summary(observation_range(now), TrendKind::Usage)
        .await
        .expect("dashboard summary");

    assert_eq!(summary.average_first_token_latency_ms, Some(100));
}

#[tokio::test]
async fn health_timeline_should_apply_status_precedence_and_thresholds() {
    let now = Utc::now();
    let current_slot = quarter_hour_start(now);
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    let services = observability_services(store.clone()).await;
    let fixtures = [
        (
            RequestMetrics {
                failure_count: 1,
                cancelled_count: 1,
                caller_error_count: 1,
                ..RequestMetrics::default()
            },
            HealthStatus::NoData,
            None,
        ),
        (
            RequestMetrics {
                incomplete_count: 1,
                ..RequestMetrics::default()
            },
            HealthStatus::LowSample,
            Some(0.0),
        ),
        (
            RequestMetrics {
                incomplete_count: 3,
                ..RequestMetrics::default()
            },
            HealthStatus::Unavailable,
            Some(0.0),
        ),
        (
            RequestMetrics {
                failure_count: 3,
                ..RequestMetrics::default()
            },
            HealthStatus::Unavailable,
            Some(0.0),
        ),
        (
            RequestMetrics {
                success_count: 1,
                failure_count: 1,
                ..RequestMetrics::default()
            },
            HealthStatus::LowSample,
            Some(50.0),
        ),
        (
            RequestMetrics {
                success_count: 94,
                failure_count: 6,
                ..RequestMetrics::default()
            },
            HealthStatus::Unstable,
            Some(94.0),
        ),
        (
            RequestMetrics {
                success_count: 95,
                incomplete_count: 5,
                ..RequestMetrics::default()
            },
            HealthStatus::Stable,
            Some(95.0),
        ),
    ];

    for (index, (metrics, expected_status, expected_reliability)) in
        fixtures.into_iter().enumerate()
    {
        store.replace_trend(vec![health_metric_point(current_slot, metrics)]);
        let range_end =
            now + Duration::seconds(i64::try_from(index).expect("fixture index fits i64") * 2);
        let timeline = services
            .observability()
            .dashboard_summary(observation_range(range_end), TrendKind::Usage)
            .await
            .expect("dashboard summary")
            .health_timeline;
        let point = timeline
            .points
            .iter()
            .find(|point| point.bucket_start == current_slot)
            .expect("current health slot");
        assert_eq!(point.status, expected_status);
        assert_eq!(point.reliability_percent, expected_reliability);
    }
}

#[tokio::test]
async fn health_views_should_include_incomplete_and_keep_final_outcomes_separate() {
    let now = Utc::now();
    let current_slot = quarter_hour_start(now);
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    let services = observability_services(store.clone()).await;
    let fixtures = [
        (162, 0, 2, 0, 0, 98.780_487_804_878_05),
        (297, 0, 2, 5, 0, 99.331_103_678_929_76),
        (467, 1, 5, 0, 0, 98.731_501_057_082_45),
        (95, 2, 3, 4, 1, 95.0),
        (100, 0, 0, 0, 0, 100.0),
    ];

    for (index, (success, failed, incomplete, cancelled, caller_error, expected_percent)) in
        fixtures.into_iter().enumerate()
    {
        let range = observation_range(
            now + Duration::seconds(i64::try_from(index).expect("fixture index fits i64") * 2),
        );
        let metrics = RequestMetrics {
            request_count: success + failed + incomplete + cancelled + caller_error,
            success_count: success,
            failure_count: failed + caller_error,
            incomplete_count: incomplete,
            cancelled_count: cancelled,
            caller_error_count: caller_error,
            ..RequestMetrics::default()
        };
        store.replace_trend(vec![health_metric_point(current_slot, metrics.clone())]);
        store.replace_overview(UsageOverview {
            analytics: Default::default(),
            range,
            requests: metrics,
            // 中间尝试失败不会增加最终失败请求数
            attempts: AttemptMetrics {
                failure_count: 2,
                ..AttemptMetrics::default()
            },
            providers: Vec::new(),
        });

        let dashboard = services
            .observability()
            .dashboard_summary(range, TrendKind::Usage)
            .await
            .expect("dashboard summary");
        let timeline = dashboard.health_timeline;
        let point = timeline
            .points
            .iter()
            .find(|point| point.bucket_start == current_slot)
            .expect("current health slot");
        assert_eq!(point.status, HealthStatus::Stable);
        assert_eq!(point.failed_requests, failed);
        assert_eq!(point.incomplete_requests, incomplete);
        assert_eq!(point.cancelled_requests, cancelled);
        assert_eq!(point.caller_error_requests, caller_error);
        for reliability in [timeline.reliability_percent, point.reliability_percent] {
            assert!((reliability.expect("health reliability") - expected_percent).abs() < 1e-10);
        }
        for success_rate in [
            dashboard.trend.summary.success_rate,
            dashboard.trend.points[0].success_rate,
        ] {
            assert!(
                (success_rate.expect("trend success rate") * 100.0 - expected_percent).abs()
                    < 1e-10
            );
        }

        let insights = services
            .observability()
            .usage_insights(range, UsageFilter::default())
            .await
            .expect("usage insights");
        assert_eq!(insights.health.failed_requests, failed);
        assert_eq!(insights.health.incomplete_requests, incomplete);
        assert!((insights.health.success_rate * 100.0 - expected_percent).abs() < 1e-10);
        assert!(
            (insights.health.points[0].error_rate * 100.0 - (100.0 - expected_percent)).abs()
                < 1e-10
        );
    }
}

#[tokio::test]
async fn dashboard_summary_should_project_rebuildable_runtime_slots() {
    let now = Utc::now();
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    store.replace_runtime_slots(Some(DashboardRuntimeSlots {
        inherited_accounts: 2,
        overridden_slots: 5,
        used_slots: Some(2),
    }));
    let services = observability_services(store).await;

    let capacity = services
        .observability()
        .dashboard_summary(observation_range(now), TrendKind::Usage)
        .await
        .expect("dashboard summary")
        .capacity;

    assert_eq!(capacity.total_slots, Some(7));
    assert_eq!(capacity.used_slots, Some(2));
    assert_eq!(capacity.available_slots, Some(5));
}

#[tokio::test]
async fn dashboard_capacity_distinguishes_unlimited_inheritance_finite_overrides_and_empty_pool() {
    for (inherited_accounts, overridden_slots, expected_total, expected_available) in [
        (2, 5, None, None),
        (0, 5, Some(5), Some(3)),
        (0, 0, Some(0), Some(0)),
    ] {
        let now = Utc::now();
        let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
        store.replace_runtime_slots(Some(DashboardRuntimeSlots {
            inherited_accounts,
            overridden_slots,
            used_slots: Some(2),
        }));
        let services = super::AdminHarness::new()
            .observability(store)
            .settings(Arc::new(FixtureSettingsStore {
                max_concurrent_per_account: 0,
            }))
            .provider(super::dashboard_profile_provider())
            .build()
            .await;
        let capacity = services
            .observability()
            .dashboard_summary(observation_range(now), TrendKind::Usage)
            .await
            .expect("dashboard")
            .capacity;
        assert_eq!(capacity.total_slots, expected_total);
        assert_eq!(capacity.available_slots, expected_available);
        assert_eq!(capacity.used_slots, Some(2));
    }
}

#[tokio::test]
async fn dashboard_summary_should_observe_current_accounts_for_a_historical_range() {
    let historical_end = Utc::now() - Duration::hours(1);
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(
        historical_end,
    )));
    let services = observability_services(store.clone()).await;

    services
        .observability()
        .dashboard_summary(observation_range(historical_end), TrendKind::Usage)
        .await
        .expect("dashboard summary");

    let summary_observed_at = *store.summary_observed_at.lock().unwrap();
    assert!(summary_observed_at.is_some_and(|value| value > historical_end));
}

#[tokio::test]
async fn dashboard_summary_should_keep_distinct_exact_ranges_in_one_short_window() {
    let end_bucket = Utc::now().timestamp().div_euclid(2) * 2;
    let first_end = DateTime::from_timestamp(end_bucket, 100_000_000).expect("first end");
    let second_end = DateTime::from_timestamp(end_bucket, 900_000_000).expect("second end");
    let first_range = TimeRange::new(
        gateway_core::time::DeploymentTimeZone::default()
            .day_start(first_end)
            .unwrap(),
        first_end,
    )
    .expect("first range");
    let second_range = TimeRange::new(
        gateway_core::time::DeploymentTimeZone::default()
            .day_start(second_end)
            .unwrap(),
        second_end,
    )
    .expect("second range");
    let store = Arc::new(FixtureObservabilityStore::new(first_range));
    store.set_dashboard_delay(StdDuration::from_millis(20));
    let services = observability_services(store.clone()).await;
    let observability = services.observability();

    let (first, second) = tokio::join!(
        observability.dashboard_summary(first_range, TrendKind::Usage),
        observability.dashboard_summary(second_range, TrendKind::Errors),
    );

    first.expect("first dashboard summary");
    second.expect("second dashboard summary");
    assert_eq!(store.dashboard_summary_calls(), 2);
}

#[tokio::test]
async fn dashboard_summary_should_reload_after_the_short_window_changes() {
    let end_bucket = Utc::now().timestamp().div_euclid(2) * 2;
    let first_end = DateTime::from_timestamp(end_bucket, 100_000_000).expect("first end");
    let second_end = first_end + Duration::seconds(2);
    let first_range = TimeRange::new(
        gateway_core::time::DeploymentTimeZone::default()
            .day_start(first_end)
            .unwrap(),
        first_end,
    )
    .expect("first range");
    let second_range = TimeRange::new(
        gateway_core::time::DeploymentTimeZone::default()
            .day_start(second_end)
            .unwrap(),
        second_end,
    )
    .expect("second range");
    let store = Arc::new(FixtureObservabilityStore::new(first_range));
    let services = observability_services(store.clone()).await;
    let observability = services.observability();

    observability
        .dashboard_summary(first_range, TrendKind::Usage)
        .await
        .expect("first dashboard summary");
    observability
        .dashboard_summary(second_range, TrendKind::Usage)
        .await
        .expect("second dashboard summary");

    assert_eq!(store.dashboard_summary_calls(), 2);
}

#[tokio::test]
async fn observability_services_should_calculate_usage_insights_and_diagnostic_shares() {
    let now = Utc::now();
    let range = observation_range(now);
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let metrics = RequestMetrics {
        request_count: 10,
        success_count: 6,
        failure_count: 4,
        caller_error_count: 2,
        input_tokens: 800,
        output_tokens: 200,
        total_tokens: 1_000,
        latency_count: 5,
        admission_decision_count: 10,
        admission_decision_percentiles: LatencyPercentiles {
            p50_ms: Some(PercentileMilliseconds::new(2.0).expect("admission P50")),
            p95_ms: Some(PercentileMilliseconds::new(5.0).expect("admission P95")),
            p99_ms: Some(PercentileMilliseconds::new(8.0).expect("admission P99")),
        },
        account_selection_wait_count: 8,
        account_selection_wait_percentiles: LatencyPercentiles {
            p50_ms: Some(PercentileMilliseconds::new(10.0).expect("selection P50")),
            p95_ms: Some(PercentileMilliseconds::new(40.0).expect("selection P95")),
            p99_ms: Some(PercentileMilliseconds::new(70.0).expect("selection P99")),
        },
        output_throughput_p10: Some(40),
        output_throughput_p50: Some(50),
        output_throughput_p90: Some(60),
        capacity_sample_count: 8,
        capacity_utilization_avg_basis_points: Some(6_500),
        capacity_utilization_p95_basis_points: Some(8_000),
        ..RequestMetrics::default()
    };
    store.replace_overview(UsageOverview {
        analytics: Default::default(),
        range,
        requests: metrics.clone(),
        attempts: AttemptMetrics {
            attempt_count: 12,
            cost_coverage: CostCoverage {
                calculated_count: 10,
                ..CostCoverage::default()
            },
            costs: vec![CurrencyCost {
                currency: "USD".to_owned(),
                amount: gateway_admin::model::observability::DecimalAmount::from_str("1.25")
                    .expect("USD cost"),
            }],
            ..AttemptMetrics::default()
        },
        providers: Vec::new(),
    });
    store.replace_trend(vec![RequestMetricPoint {
        bucket_start: quarter_hour_start(now),
        granularity: Granularity::FifteenMinutes,
        metrics,
        cost_coverage: CostCoverage::default(),
        costs: vec![CurrencyCost {
            currency: "USD".to_owned(),
            amount: gateway_admin::model::observability::DecimalAmount::from_str("0.25")
                .expect("bucket USD cost"),
        }],
    }]);
    store.replace_projected_costs(vec![UsageProjectedCostPoint {
        bucket_start: quarter_hour_start(now),
        standard_amount: Some("1".parse().expect("standard cost")),
        no_cache_amount: Some("1.25".parse().expect("no-cache cost")),
    }]);
    store.replace_diagnostics(DiagnosticsObservation {
        analytics: Default::default(),
        total_request_count: 4,
        items: vec![diagnostic("openai", 3), diagnostic("xai", 1)],
    });
    let services = observability_services_with_calculated_billing(store).await;

    let insights = services
        .observability()
        .usage_insights(range, UsageFilter::default())
        .await
        .expect("usage insights");
    let diagnostics = services
        .observability()
        .diagnostics(range, UsageFilter::default(), DiagnosticDimension::Provider)
        .await
        .expect("usage diagnostics");

    assert_eq!(insights.health.total_requests, 10);
    assert_eq!(insights.health.failed_requests, 2);
    assert_eq!(insights.health.success_rate, 0.75);
    assert_eq!(insights.health.completion_rate, 1.0);
    assert_eq!(insights.granularity, Granularity::FifteenMinutes);
    assert_eq!(insights.performance.latency_coverage, 0.5);
    assert_eq!(insights.performance.admission_decision_coverage, 1.0);
    assert_eq!(insights.performance.account_selection_wait_coverage, 0.8);
    assert_eq!(insights.performance.capacity_coverage, 0.8);
    assert_eq!(insights.performance.output_throughput_p50, Some(50));
    assert_eq!(insights.cost.tokens_per_request, 100.0);
    assert_eq!(
        insights
            .cost
            .estimated_cost
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("1.25")
    );
    assert_eq!(
        insights
            .cost
            .cost_per_request
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("0.125")
    );
    assert_eq!(
        insights.cost.points[0]
            .estimated_cost
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("0.25")
    );
    assert_eq!(
        insights
            .cost
            .standard_cost
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("1")
    );
    assert_eq!(
        insights.cost.points[0]
            .standard_cost
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("1")
    );
    assert_eq!(
        insights
            .cost
            .no_cache_cost
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("1.25")
    );
    assert_eq!(
        insights
            .cost
            .tier_premium
            .as_ref()
            .map(gateway_admin::model::observability::DecimalAmount::as_str),
        Some("0.25")
    );
    assert_eq!(diagnostics.items[0].request_share, 0.75);
    assert_eq!(diagnostics.items[1].request_share, 0.25);
}

#[tokio::test]
async fn diagnostics_should_preserve_request_order_when_a_small_sample_fails() {
    let now = Utc::now();
    let range = observation_range(now);
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let success = diagnostic("popular", 100);
    let mut failure = diagnostic("single_failure", 1);
    failure.success_count = 0;
    failure.failure_count = 1;
    store.replace_diagnostics(DiagnosticsObservation {
        analytics: Default::default(),
        total_request_count: 101,
        items: vec![success, failure],
    });
    let services = observability_services_with_calculated_billing(store).await;

    let result = services
        .observability()
        .diagnostics(range, UsageFilter::default(), DiagnosticDimension::Model)
        .await
        .expect("diagnostics");

    assert_eq!(result.items[0].key, "popular");
    assert_eq!(result.items[1].key, "single_failure");
    assert_eq!(result.items[0].error_count, 0);
    assert_eq!(result.items[1].error_count, 1);
    assert_eq!(result.items[1].error_rate, 1.0);
}

#[tokio::test]
async fn diagnostics_should_use_the_total_before_truncating_groups() {
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    store.replace_diagnostics(DiagnosticsObservation {
        analytics: Default::default(),
        total_request_count: 200,
        items: (0..100)
            .map(|index| diagnostic(&format!("model-{index}"), 1))
            .collect(),
    });
    let services = observability_services_with_calculated_billing(store).await;

    let result = services
        .observability()
        .diagnostics(range, UsageFilter::default(), DiagnosticDimension::Model)
        .await
        .expect("limited diagnostics");

    assert!(result.items.iter().all(|item| item.request_share == 0.005));
}

#[tokio::test]
async fn diagnostics_should_count_each_retried_request_once() {
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let mut item = diagnostic("model", 8);
    item.attempt_count = 14;
    item.retry_count = 6;
    item.retried_request_count = 2;
    store.replace_diagnostics(DiagnosticsObservation {
        analytics: Default::default(),
        total_request_count: 8,
        items: vec![item],
    });
    let services = observability_services_with_calculated_billing(store).await;

    let result = services
        .observability()
        .diagnostics(range, UsageFilter::default(), DiagnosticDimension::Model)
        .await
        .expect("retry diagnostics");

    assert_eq!(
        (result.items[0].retry_count, result.items[0].retry_rate),
        (6, 0.25)
    );
}

#[tokio::test]
async fn diagnostics_should_preserve_empty_results() {
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let services = observability_services_with_calculated_billing(store).await;

    let result = services
        .observability()
        .diagnostics(range, UsageFilter::default(), DiagnosticDimension::Account)
        .await
        .expect("empty diagnostics");

    assert!(result.items.is_empty());
}

#[tokio::test]
async fn usage_records_should_tolerate_records_that_fail_billing_enrichment() {
    let now = Utc::now();
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    let invalid_kind = "x".repeat(65);
    store.replace_usage_records(vec![
        total_record(
            "request_invalid_kind",
            Some(&invalid_kind),
            "calculated",
            now,
        ),
        total_record(
            "request_unregistered_kind",
            Some("anthropic"),
            "calculated",
            now,
        ),
        total_record("request_enriched", Some("openai"), "calculated", now),
    ]);
    let services = observability_services_with_calculated_billing(store).await;

    let page = services
        .observability()
        .usage_records(usage_query(now))
        .await
        .expect("one bad record must not fail the whole usage list");

    assert_eq!(page.items.len(), 3);
    assert!(
        matches!(page.items[0].billing, Some(UsageBilling::Total { .. })),
        "invalid Provider kind keeps the stored total"
    );
    assert!(
        matches!(page.items[1].billing, Some(UsageBilling::Total { .. })),
        "unregistered Provider kind keeps the stored total"
    );
    assert!(
        matches!(page.items[2].billing, Some(UsageBilling::Calculated(_))),
        "healthy record is still enriched"
    );
}

#[tokio::test]
async fn usage_records_should_enrich_provider_reported_totals_when_pricing_matches() {
    let now = Utc::now();
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    store.replace_usage_records(vec![total_record(
        "request_provider_reported",
        Some("openai"),
        "provider_reported",
        now,
    )]);
    let services = observability_services_with_calculated_billing(store).await;

    let page = services
        .observability()
        .usage_records(usage_query(now))
        .await
        .expect("usage records");

    assert!(matches!(
        page.items[0].billing,
        Some(UsageBilling::Calculated(_))
    ));
}

#[tokio::test]
async fn usage_insights_should_reject_partial_costs_when_insights_fail() {
    let now = Utc::now();
    let range = observation_range(now);
    let store = Arc::new(FixtureObservabilityStore::new(range));
    store.replace_projected_costs(vec![UsageProjectedCostPoint {
        bucket_start: quarter_hour_start(now),
        standard_amount: Some("1".parse().expect("standard cost")),
        no_cache_amount: Some("1.25".parse().expect("no-cache cost")),
    }]);
    store.insights_fail.store(true, Ordering::SeqCst);
    let services = observability_services_with_calculated_billing(store).await;
    assert!(
        services
            .observability()
            .usage_insights(range, UsageFilter::default())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn dashboard_should_preserve_provider_plan_names() {
    let now = Utc::now();
    let range = observation_range(now);
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let plans = [("prolite", "ProLite"), ("pro", "Pro"), ("promax", "ProMax")];
    *store.account_usage.lock().unwrap() = plans
        .iter()
        .map(|(plan, _)| DashboardAccountUsage {
            account_id: format!("account-{plan}"),
            provider_kind: "openai".to_owned(),
            authentication_kind: "oauth".to_owned(),
            name: (*plan).to_owned(),
            email: None,
            plan_type: Some((*plan).to_owned()),
            plan_type_display: None,
            request_count: 0,
            success_count: 0,
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
            reasoning_output_tokens: None,
            image_input_tokens: None,
            image_output_tokens: None,
            image_request_count: 0,
            image_request_failed_count: 0,
            total_tokens: None,
            cost_coverage: CostCoverage::default(),
            costs: Vec::new(),
            last_used_at: None,
            request_buckets: Vec::new(),
            quota_used_percent: None,
            quota_window: None,
            models: Vec::new(),
        })
        .collect();
    let provider =
        super::accounts::FakeProviderAdmin::new("openai", Arc::new(Mutex::new(Vec::new())));
    let services = super::AdminHarness::new()
        .observability(store)
        .settings(Arc::new(FixtureSettingsStore {
            max_concurrent_per_account: 1,
        }))
        .provider(provider)
        .build()
        .await;
    let result = services
        .observability()
        .dashboard_summary(range, TrendKind::Usage)
        .await
        .unwrap();
    assert_eq!(result.observation.account_usage.len(), plans.len());
    for (account, (raw, display)) in result.observation.account_usage.iter().zip(plans) {
        assert_eq!(account.plan_type.as_deref(), Some(raw));
        assert_eq!(account.plan_type_display.as_deref(), Some(display));
    }
}

struct FixtureObservabilityStore {
    trend: Mutex<Vec<RequestMetricPoint>>,
    overview: Mutex<UsageOverview>,
    projected_costs: Mutex<Vec<UsageProjectedCostPoint>>,
    insights_fail: AtomicBool,
    summary_fail: AtomicBool,
    diagnostics: Mutex<DiagnosticsObservation>,
    runtime_slots: Mutex<Option<DashboardRuntimeSlots>>,
    summary_observed_at: Mutex<Option<DateTime<Utc>>>,
    summary_query: Mutex<Option<DashboardQuery>>,
    usage_granularities: Mutex<Vec<Granularity>>,
    usage_records: Mutex<Vec<UsageListRecord>>,
    account_usage: Mutex<Vec<DashboardAccountUsage>>,
    dashboard_delay: Mutex<StdDuration>,
    dashboard_summary_calls: AtomicUsize,
    usage_summary_filters: Mutex<Vec<UsageFilter>>,
}

impl FixtureObservabilityStore {
    fn new(range: TimeRange) -> Self {
        Self {
            trend: Mutex::new(Vec::new()),
            overview: Mutex::new(UsageOverview {
                analytics: Default::default(),
                range,
                requests: RequestMetrics::default(),
                attempts: AttemptMetrics::default(),
                providers: Vec::new(),
            }),
            projected_costs: Mutex::new(Vec::new()),
            insights_fail: AtomicBool::new(false),
            summary_fail: AtomicBool::new(false),
            diagnostics: Mutex::new(DiagnosticsObservation::default()),
            runtime_slots: Mutex::new(None),
            summary_observed_at: Mutex::new(None),
            summary_query: Mutex::new(None),
            usage_granularities: Mutex::new(Vec::new()),
            usage_records: Mutex::new(Vec::new()),
            account_usage: Mutex::new(Vec::new()),
            dashboard_delay: Mutex::new(StdDuration::ZERO),
            dashboard_summary_calls: AtomicUsize::new(0),
            usage_summary_filters: Mutex::new(Vec::new()),
        }
    }

    fn set_dashboard_delay(&self, delay: StdDuration) {
        *self.dashboard_delay.lock().expect("dashboard delay") = delay;
    }

    fn dashboard_summary_calls(&self) -> usize {
        self.dashboard_summary_calls.load(Ordering::Relaxed)
    }

    fn replace_trend(&self, trend: Vec<RequestMetricPoint>) {
        *self.trend.lock().expect("trend") = trend;
    }

    fn replace_overview(&self, overview: UsageOverview) {
        *self.overview.lock().expect("overview") = overview;
    }

    fn replace_projected_costs(&self, facts: Vec<UsageProjectedCostPoint>) {
        *self.projected_costs.lock().expect("projected costs") = facts;
    }

    fn replace_diagnostics(&self, diagnostics: DiagnosticsObservation) {
        *self.diagnostics.lock().expect("diagnostics") = diagnostics;
    }

    fn replace_runtime_slots(&self, runtime_slots: Option<DashboardRuntimeSlots>) {
        *self.runtime_slots.lock().expect("runtime slots") = runtime_slots;
    }

    fn replace_usage_records(&self, records: Vec<UsageListRecord>) {
        *self.usage_records.lock().expect("usage records") = records;
    }
}

#[async_trait]
impl ObservabilityStore for FixtureObservabilityStore {
    fn query_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(10)
    }

    async fn dashboard_summary(
        &self,
        query: DashboardQuery,
        observed_at: DateTime<Utc>,
    ) -> AdminStoreResult<DashboardObservation> {
        self.dashboard_summary_calls.fetch_add(1, Ordering::Relaxed);
        *self.summary_query.lock().unwrap() = Some(query.clone());
        *self
            .summary_observed_at
            .lock()
            .expect("summary observed at") = Some(observed_at);
        let dashboard_delay = *self.dashboard_delay.lock().expect("dashboard delay");
        if !dashboard_delay.is_zero() {
            tokio::time::sleep(dashboard_delay).await;
        }
        Ok(DashboardObservation {
            analytics: self.overview.lock().expect("overview").analytics.clone(),
            range: query.range,
            totals: Default::default(),
            provider_accounts: AccountPoolMetrics::default(),
            runtime_slots: *self.runtime_slots.lock().expect("runtime slots"),
            trend: self.trend.lock().expect("trend").clone(),
            account_usage: self.account_usage.lock().expect("account usage").clone(),
            recent_requests: Vec::new(),
        })
    }

    async fn dashboard_trend(
        &self,
        _: TimeRange,
        _: Granularity,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        Ok(self.trend.lock().expect("trend").clone())
    }

    async fn usage_trend(
        &self,
        _: TimeRange,
        _: UsageFilter,
        granularity: Granularity,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        self.usage_granularities.lock().unwrap().push(granularity);
        Ok(self.trend.lock().expect("trend").clone())
    }

    async fn analytics_cache_generation(&self) -> AdminStoreResult<u64> {
        Ok(self.overview.lock().expect("overview").analytics.generation)
    }

    async fn usage_insights_observation(
        &self,
        _: TimeRange,
        _: UsageFilter,
        granularity: Granularity,
    ) -> AdminStoreResult<UsageInsightsObservation> {
        self.usage_granularities.lock().unwrap().push(granularity);
        if self.insights_fail.load(Ordering::SeqCst) {
            return Err(super::unavailable("projection read failed"));
        }
        Ok(UsageInsightsObservation {
            overview: self.overview.lock().expect("overview").clone(),
            trend: self.trend.lock().expect("trend").clone(),
            costs: self
                .projected_costs
                .lock()
                .expect("projected costs")
                .clone(),
        })
    }

    async fn list_usage_records(&self, query: UsageQuery) -> AdminStoreResult<UsagePage> {
        let items = self.usage_records.lock().expect("usage records").clone();
        let total = u64::try_from(items.len()).unwrap_or(u64::MAX);
        Ok(UsagePage {
            items,
            current_page: query.current_page,
            page_size: query.page_size.get(),
            total,
        })
    }

    async fn usage_record_detail(&self, _: &str) -> AdminStoreResult<UsageDetail> {
        Err(super::unavailable("usage detail"))
    }

    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> AdminStoreResult<UsageOverview> {
        self.usage_summary_filters
            .lock()
            .expect("summary filters")
            .push(filter);
        let delay = *self.dashboard_delay.lock().expect("query delay");
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if self.summary_fail.load(Ordering::SeqCst) {
            return Err(super::unavailable("projection summary failed"));
        }
        Ok(UsageOverview {
            range,
            ..self.overview.lock().expect("overview").clone()
        })
    }

    async fn usage_diagnostics(
        &self,
        _: TimeRange,
        _: UsageFilter,
        _: DiagnosticDimension,
        _: u16,
    ) -> AdminStoreResult<DiagnosticsObservation> {
        Ok(self.diagnostics.lock().expect("diagnostics").clone())
    }

    async fn list_ops_errors(&self, _: OpsErrorQuery) -> AdminStoreResult<OpsErrorPage> {
        Err(super::unavailable("ops errors"))
    }
}

struct FixtureSettingsStore {
    max_concurrent_per_account: u32,
}

#[async_trait]
impl SettingsStore for FixtureSettingsStore {
    async fn load_pricing(&self) -> AdminStoreResult<gateway_admin::model::pricing::StoredPricing> {
        Ok(Default::default())
    }
    async fn sync_pricing(
        &self,
        _: gateway_admin::model::pricing::PricingSyncChanges,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::Revision> {
        panic!("unexpected pricing sync")
    }
    async fn update_pricing(
        &self,
        _: gateway_admin::model::pricing::UpdatePricing,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::Revision> {
        panic!("unexpected pricing update")
    }
    async fn load_runtime_settings(&self) -> AdminStoreResult<RuntimeSettings> {
        Ok(RuntimeSettings {
            request_profiles: Default::default(),
            config_revision: Revision::new(1).expect("revision"),
            model_mappings: Default::default(),
            rotation_strategy: RotationStrategy::Smart,
            updated_at: Utc::now(),
            values: gateway_admin::model::settings::RuntimeSettingsValues {
                codex_privacy_policy: Default::default(),
                request_location_enabled: false,
                request_location: Default::default(),
                refresh_margin_seconds: 300,
                refresh_concurrency: 2,
                max_concurrent_per_account: self.max_concurrent_per_account,
                request_interval_ms: 0,
                max_waiting_per_key: 0,
                max_waiting_per_account: 0,
                concurrency_wait_timeout_seconds: 30,
                openai_guardian_reserved_concurrency: 0,
                openai_account_affinity: gateway_core::account::AccountAffinity::Relaxed,
                max_account_rotations: 3,
                openai_session_affinity_ttl_hours: 24,
                responses_max_decompressed_body_bytes: 64 * 1024 * 1024,
                smart_scheduling: gateway_core::account::SmartSchedulingConfig::default(),
                min_codex_desktop_version: None,
                min_codex_cli_version: None,
                usage_retention_days: 31,
                ops_event_retention_days: 30,
                audit_retention_days: 30,
                account_auto_freeze_enabled: true,
                account_auto_freeze_threshold: 12,
                account_auto_freeze_window_seconds: 600,
                account_auto_freeze_duration_seconds: 7_200,
                account_auto_freeze_probe_enabled: true,
                account_auto_freeze_probe_model: None,
                account_auto_freeze_adaptive_concurrency: true,
                account_warmup_enabled: false,
                account_warmup_schedule_time: "08:00".to_owned(),
                account_warmup_model: None,
            },
        })
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        Err(super::unavailable("admin API key"))
    }

    async fn replace_runtime_settings(
        &self,
        _: ReplaceRuntimeSettings,
        _: &MutationContext,
    ) -> AdminStoreResult<RuntimeSettings> {
        Err(super::unavailable("settings"))
    }

    async fn replace_admin_api_key(
        &self,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(super::unavailable("admin API key"))
    }

    async fn delete_admin_api_key(
        &self,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(super::unavailable("admin API key"))
    }
}

async fn observability_services(store: Arc<FixtureObservabilityStore>) -> AdminServices {
    super::AdminHarness::new()
        .observability(store)
        .settings(Arc::new(FixtureSettingsStore {
            max_concurrent_per_account: 1,
        }))
        .provider(super::dashboard_profile_provider())
        .build()
        .await
}

async fn observability_services_with_calculated_billing(
    store: Arc<FixtureObservabilityStore>,
) -> AdminServices {
    super::AdminHarness::new()
        .observability(store)
        .settings(Arc::new(FixtureSettingsStore {
            max_concurrent_per_account: 1,
        }))
        .provider(super::calculated_billing_provider())
        .build()
        .await
}

fn observation_range(end: DateTime<Utc>) -> TimeRange {
    TimeRange::new(end - Duration::hours(24), end).expect("observation range")
}

fn usage_query(now: DateTime<Utc>) -> UsageQuery {
    UsageQuery {
        range: observation_range(now),
        filter: UsageFilter::default(),
        current_page: 1,
        page_size: ObservabilityPageSize::new(50).expect("page size"),
    }
}

fn total_record(
    id: &str,
    provider_kind: Option<&str>,
    source: &str,
    now: DateTime<Utc>,
) -> UsageListRecord {
    UsageListRecord {
        client_api_key_name: Some("Production".to_owned()),
        id: id.to_owned(),
        endpoint: "/v1/responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model_id: Some("gpt-5.5".to_owned()),
        provider_kind: provider_kind.map(str::to_owned),
        provider_account_ref: None,
        provider_account_name: None,
        provider_account_email: None,
        provider_account_notes: None,
        provider_account_plan_type: None,
        provider_account_plan_type_display: None,
        provider_account_authentication_kind: None,
        upstream_model_id: Some("gpt-5.5".to_owned()),
        upstream_transport: None,
        upstream_response_model: None,
        service_tier: None,
        input_tokens: Some(800),
        output_tokens: Some(200),
        cached_input_tokens: Some(0),
        cache_write_input_tokens: Some(0),
        reasoning_output_tokens: Some(0),
        image_input_tokens: None,
        image_output_tokens: None,
        total_tokens: Some(1_000),
        cost_source: source.to_owned(),
        cost_amount: None,
        cost_currency: None,
        billing: Some(UsageBilling::Total {
            source: source.to_owned(),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: gateway_admin::model::observability::DecimalAmount::from_str("1.25")
                    .expect("billing total"),
            },
        }),
        transport_decision_wait_ms: None,
        connect_ms: None,
        headers_ms: None,
        first_event_ms: None,
        first_reasoning_ms: None,
        first_text_ms: None,
        first_token_ms: None,
        provider_processing_ms: None,
        upstream_response_ms: None,
        upstream_api_overhead_ms: None,
        upstream_engine_ms: None,
        upstream_engine_iapi_ttft_ms: None,
        upstream_engine_service_ttft_ms: None,
        upstream_engine_iapi_tbt_ms: None,
        upstream_engine_service_tbt_ms: None,
        latency_ms: Some(100),
        admission_decision_ms: None,
        account_selection_wait_ms: None,
        capacity_used_slots: None,
        capacity_total_slots: None,
        client_ip: None,
        user_agent: None,
        reasoning_effort: None,
        reasoning_preset: None,
        subagent_kind: None,
        compact: false,
        started_at: now,
    }
}

fn health_metric_point(bucket_start: DateTime<Utc>, metrics: RequestMetrics) -> RequestMetricPoint {
    RequestMetricPoint {
        bucket_start,
        granularity: Granularity::FifteenMinutes,
        metrics,
        cost_coverage: CostCoverage::default(),
        costs: Vec::new(),
    }
}

fn diagnostic(name: &str, request_count: u64) -> DiagnosticObservation {
    DiagnosticObservation {
        first_token_p95_ms: None,
        non_completion_count: 0,
        retry_count: 0,
        retried_request_count: 0,
        account_provider_kind: None,
        account_plan_type: None,
        key: name.to_owned(),
        name: name.to_owned(),
        request_count,
        success_count: request_count,
        failure_count: 0,
        attempt_count: request_count,
        total_tokens: request_count.saturating_mul(100),
        average_latency_ms: Some(100),
        latency_p95_ms: Some(200),
        cost_coverage: CostCoverage::default(),
        costs: Vec::new(),
    }
}

fn quarter_hour_start(value: DateTime<Utc>) -> DateTime<Utc> {
    let elapsed = value.timestamp().rem_euclid(15 * 60);
    value - Duration::seconds(elapsed) - Duration::nanoseconds(i64::from(value.nanosecond()))
}

#[tokio::test]
async fn admin_selects_dashboard_defaults_and_matching_usage_buckets_at_range_boundaries() {
    let now = Utc::now();
    for (duration, expected) in [
        (Duration::days(2), Granularity::FifteenMinutes),
        (Duration::days(2) + Duration::seconds(1), Granularity::Hour),
        (Duration::days(31), Granularity::Hour),
        (Duration::days(31) + Duration::seconds(1), Granularity::Day),
    ] {
        let range = TimeRange::new(now - duration, now).unwrap();
        let store = Arc::new(FixtureObservabilityStore::new(range));
        let services = observability_services(store.clone()).await;
        services
            .observability()
            .dashboard_summary(range, TrendKind::Usage)
            .await
            .unwrap();
        let query = store.summary_query.lock().unwrap().clone().unwrap();
        assert_eq!(query.range, range);
        assert_eq!(query.granularity, expected);
        assert_eq!(query.account_limit, 4);
        assert_eq!(query.recent_request_limit, 10);
        assert_eq!(
            query.recent_request_filter.outcome,
            Some(gateway_admin::model::observability::RequestOutcome::Succeeded)
        );
        services
            .observability()
            .usage_insights(range, UsageFilter::default())
            .await
            .unwrap();
        assert_eq!(*store.usage_granularities.lock().unwrap(), vec![expected]);
    }
}

#[tokio::test]
async fn usage_summary_cache_should_coalesce_identical_queries_and_invalidate_deleted_generation() {
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    store.set_dashboard_delay(StdDuration::from_millis(20));
    let services = observability_services(store.clone()).await;
    let service = services.observability();
    let filter = UsageFilter {
        client_api_key_ref: Some("deleted-key".to_owned()),
        ..Default::default()
    };
    let (first, second) = tokio::join!(
        service.usage_summary(range, filter.clone()),
        service.usage_summary(range, filter.clone()),
    );
    first.expect("first summary");
    second.expect("coalesced summary");
    assert_eq!(store.usage_summary_filters.lock().unwrap().len(), 1);
    {
        let mut overview = store.overview.lock().unwrap();
        overview.analytics.generation = 1;
        overview.requests.request_count = 7;
    }
    let refreshed = service
        .usage_summary(range, filter)
        .await
        .expect("generation refresh");
    assert_eq!(refreshed.overview.analytics.generation, 1);
    assert_eq!(refreshed.overview.requests.request_count, 7);
    assert_eq!(store.usage_summary_filters.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn usage_summary_cache_should_coalesce_when_generation_advances_during_the_first_load() {
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    store.set_dashboard_delay(StdDuration::from_millis(20));
    let services = observability_services(store.clone()).await;
    let service = services.observability();
    let (first, second, ()) = tokio::join!(
        service.usage_summary(range, UsageFilter::default()),
        service.usage_summary(range, UsageFilter::default()),
        async {
            tokio::time::sleep(StdDuration::from_millis(5)).await;
            store.overview.lock().unwrap().analytics.generation = 1;
        },
    );
    assert_eq!(
        first.expect("first summary").overview.analytics.generation,
        1
    );
    assert_eq!(
        second
            .expect("coalesced summary")
            .overview
            .analytics
            .generation,
        1
    );
    assert_eq!(store.usage_summary_filters.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn usage_summary_cache_should_isolate_key_identity_and_filters() {
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let services = observability_services(store.clone()).await;
    let service = services.observability();
    for (key, model) in [
        ("key-a", None),
        ("key-a", None),
        ("key-b", None),
        ("key-a", Some("model-a")),
    ] {
        service
            .usage_summary(
                range,
                UsageFilter {
                    client_api_key_ref: Some(key.to_owned()),
                    model: model.map(str::to_owned),
                    ..Default::default()
                },
            )
            .await
            .expect("isolated summary");
    }
    let calls = store.usage_summary_filters.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].client_api_key_ref.as_deref(), Some("key-a"));
    assert_eq!(calls[1].client_api_key_ref.as_deref(), Some("key-b"));
    assert_eq!(calls[2].model.as_deref(), Some("model-a"));
}

#[tokio::test(start_paused = true)]
async fn usage_summary_cache_should_bound_waiting_after_failed_refresh_and_allow_recovery() {
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    store.set_dashboard_delay(store.query_timeout().mul_f64(0.7));
    store.summary_fail.store(true, Ordering::SeqCst);
    let services = observability_services(store.clone()).await;
    let service = services.observability();
    let started = tokio::time::Instant::now();
    let (first, second, third, fourth) = tokio::join!(
        service.usage_summary(range, UsageFilter::default()),
        service.usage_summary(range, UsageFilter::default()),
        service.usage_summary(range, UsageFilter::default()),
        service.usage_summary(range, UsageFilter::default()),
    );
    assert!(first.is_err() && second.is_err() && third.is_err() && fourth.is_err());
    assert!(
        started.elapsed() <= store.query_timeout(),
        "同键失败不能使后续请求逐个等待完整数据库超时: {:?}",
        started.elapsed()
    );

    store.summary_fail.store(false, Ordering::SeqCst);
    store.set_dashboard_delay(StdDuration::ZERO);
    service
        .usage_summary(range, UsageFilter::default())
        .await
        .expect("cancelled cache refresh releases its slot for recovery");
}

#[tokio::test(start_paused = true)]
async fn insights_cache_should_bound_stale_lifetime_without_renewing_failed_refreshes() {
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    {
        let mut overview = store.overview.lock().unwrap();
        overview.analytics.state = AnalyticsState::Ready;
        overview.analytics.generated_at = Utc::now();
        overview.analytics.lag_seconds = 7;
        overview.requests.request_count = 19;
    }
    let services = observability_services(store.clone()).await;
    let service = services.observability();
    service
        .usage_insights(range, UsageFilter::default())
        .await
        .unwrap();
    store.insights_fail.store(true, Ordering::SeqCst);
    tokio::time::advance(StdDuration::from_secs(16)).await;
    let stale = service
        .usage_insights(range, UsageFilter::default())
        .await
        .unwrap();
    assert_eq!(stale.analytics.state, AnalyticsState::Stale);
    assert!(stale.analytics.lag_seconds >= 7);
    assert_eq!(stale.health.total_requests, 19);
    tokio::time::advance(StdDuration::from_secs(45)).await;
    assert!(
        service
            .usage_insights(range, UsageFilter::default())
            .await
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn insights_cache_should_reject_deleted_building_and_old_generation_times() {
    for reason in ["deleted", "building", "generated-too-long-ago"] {
        let range = observation_range(Utc::now());
        let store = Arc::new(FixtureObservabilityStore::new(range));
        {
            let mut overview = store.overview.lock().unwrap();
            overview.analytics.state = if reason == "building" {
                AnalyticsState::Building
            } else {
                AnalyticsState::Ready
            };
            overview.analytics.generated_at = Utc::now()
                - Duration::seconds(if reason == "generated-too-long-ago" {
                    61
                } else {
                    0
                });
        }
        let services = observability_services(store.clone()).await;
        let service = services.observability();
        service
            .usage_insights(range, UsageFilter::default())
            .await
            .unwrap();
        store.insights_fail.store(true, Ordering::SeqCst);
        if reason == "deleted" {
            store.overview.lock().unwrap().analytics.generation += 1;
        }
        tokio::time::advance(StdDuration::from_secs(16)).await;
        assert!(
            service
                .usage_insights(range, UsageFilter::default())
                .await
                .is_err(),
            "{reason}"
        );
    }
}
