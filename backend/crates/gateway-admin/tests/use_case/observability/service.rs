//! 观测查询协调、错误传播与费用补充测试

use super::*;

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
    assert_eq!(insights.health.success_rate, Some(0.75));
    assert_eq!(insights.health.completion_rate, 1.0);
    assert_eq!(insights.granularity, Granularity::FifteenMinutes);
    assert_eq!(
        insights
            .performance
            .load
            .admission_decision_p95_ms
            .map(|value| value.as_f64()),
        Some(5.0)
    );
    assert_eq!(
        insights
            .performance
            .load
            .account_selection_wait_p95_ms
            .map(|value| value.as_f64()),
        Some(40.0)
    );
    assert_eq!(
        insights.performance.points[0].load,
        insights.performance.load
    );
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
    assert_eq!(result.items[1].error_rate, Some(1.0));
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
