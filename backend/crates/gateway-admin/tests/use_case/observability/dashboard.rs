//! 仪表盘指标、时间桶与账号聚合测试

use super::*;

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

#[test]
fn dashboard_comparison_should_follow_deployment_calendar_across_dst() {
    let timezone = "America/New_York".parse().unwrap();
    for (end, previous_start, previous_end, hours) in [
        (
            "2026-03-09T16:00:00Z",
            "2026-03-08T05:00:00Z",
            "2026-03-09T04:00:00Z",
            23,
        ),
        (
            "2026-11-02T17:00:00Z",
            "2026-11-01T04:00:00Z",
            "2026-11-02T05:00:00Z",
            25,
        ),
        (
            "2026-11-02T05:00:00Z",
            "2026-11-01T04:00:00Z",
            "2026-11-02T05:00:00Z",
            25,
        ),
    ] {
        let end = end.parse().unwrap();
        let range = TimeRange::calendar_at(
            gateway_admin::model::observability::CalendarPeriod::Today,
            end,
            timezone,
        )
        .unwrap();
        let query = DashboardQuery::new(range, timezone).unwrap();
        assert_eq!(query.range, range);
        assert_eq!(
            query.yesterday_range.start,
            previous_start.parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            query.yesterday_range.end,
            previous_end.parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            (query.yesterday_range.end - query.yesterday_range.start).num_hours(),
            hours
        );
    }
}

#[tokio::test]
async fn dashboard_summary_should_compare_separate_yesterday_metrics_with_today_only_trend() {
    let end: DateTime<Utc> = "2026-10-18T04:00:00Z".parse().unwrap();
    let range = TimeRange::calendar_at(
        gateway_admin::model::observability::CalendarPeriod::Today,
        end,
        Default::default(),
    )
    .unwrap();
    let store = Arc::new(FixtureObservabilityStore::new(range));
    store.replace_trend(vec![health_metric_point(
        range.start,
        RequestMetrics {
            request_count: 2,
            input_tokens: 100,
            cached_input_tokens: 20,
            total_tokens: 120,
            ..Default::default()
        },
    )]);
    *store.yesterday.lock().unwrap() = RequestMetrics {
        request_count: 4,
        input_tokens: 400,
        cached_input_tokens: 200,
        total_tokens: 500,
        ..Default::default()
    };
    let result = observability_services(store.clone())
        .await
        .observability()
        .dashboard_summary(range, TrendKind::Usage)
        .await
        .unwrap();
    assert_eq!(result.today.request_count, 2);
    assert_eq!(result.today.total_tokens, 120);
    assert_eq!(result.today.observed_cached_token_rate, Some(0.2));
    assert_eq!(result.yesterday.request_count, 4);
    assert_eq!(result.yesterday.total_tokens, 500);
    assert_eq!(result.yesterday.observed_cached_token_rate, Some(0.5));
    assert_eq!(result.observation.range, range);
    assert_eq!(result.observation.trend.len(), 1);
    assert_eq!(result.observation.trend[0].bucket_start, range.start);
    assert_eq!(
        store
            .summary_query
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .yesterday_range
            .end,
        range.start
    );
}

#[tokio::test]
async fn dashboard_summary_should_keep_missing_yesterday_cache_rate_unknown() {
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let result = observability_services(store)
        .await
        .observability()
        .dashboard_summary(range, TrendKind::Usage)
        .await
        .unwrap();
    assert_eq!(result.yesterday.request_count, 0);
    assert_eq!(result.yesterday.total_tokens, 0);
    assert_eq!(result.yesterday.observed_cached_token_rate, None);
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
        (162, 0, 2, 0, 0, 0, Some(98.780_487_804_878_05)),
        (297, 0, 2, 5, 0, 0, Some(99.331_103_678_929_76)),
        (467, 1, 5, 0, 0, 0, Some(98.731_501_057_082_45)),
        (95, 2, 3, 4, 1, 7, Some(95.0)),
        (100, 0, 0, 0, 0, 0, Some(100.0)),
        (0, 0, 0, 0, 0, 0, None),
        (0, 0, 0, 4, 3, 2, None),
        (0, 1, 2, 0, 0, 0, Some(0.0)),
    ];

    for (
        index,
        (success, failed, incomplete, cancelled, caller_error, running, expected_percent),
    ) in fixtures.into_iter().enumerate()
    {
        let range = observation_range(
            now + Duration::seconds(i64::try_from(index).expect("fixture index fits i64") * 2),
        );
        let metrics = RequestMetrics {
            request_count: success + failed + incomplete + cancelled + caller_error + running,
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

        let mut item = diagnostic(
            "model",
            success + failed + incomplete + cancelled + caller_error + running,
        );
        item.success_count = success;
        item.failure_count = failed + caller_error;
        item.incomplete_count = incomplete;
        item.cancelled_count = cancelled;
        item.caller_error_count = caller_error;
        store.replace_diagnostics(DiagnosticsObservation {
            analytics: Default::default(),
            total_request_count: item.request_count,
            items: vec![item],
        });
        let diagnostics = services
            .observability()
            .diagnostics(range, UsageFilter::default(), DiagnosticDimension::Model)
            .await
            .unwrap();
        assert_eq!(diagnostics.items[0].error_count, failed + incomplete);

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
        assert_eq!(dashboard.trend.summary.error_count, failed + incomplete);
        assert_eq!(dashboard.trend.points[0].error_count, failed + incomplete);
        assert_eq!(point.failed_requests, failed);
        assert_eq!(point.incomplete_requests, incomplete);
        assert_eq!(point.cancelled_requests, cancelled);
        assert_eq!(point.caller_error_requests, caller_error);

        let insights = services
            .observability()
            .usage_insights(range, UsageFilter::default())
            .await
            .expect("usage insights");
        assert_eq!(insights.health.failed_requests, failed);
        assert_eq!(insights.health.incomplete_requests, incomplete);
        for actual in [
            timeline.reliability_percent,
            point.reliability_percent,
            dashboard
                .trend
                .summary
                .success_rate
                .map(|rate| rate * 100.0),
            dashboard.trend.points[0]
                .success_rate
                .map(|rate| rate * 100.0),
            insights.health.success_rate.map(|rate| rate * 100.0),
            insights.health.points[0]
                .error_rate
                .map(|rate| (1.0 - rate) * 100.0),
            diagnostics.items[0]
                .error_rate
                .map(|rate| (1.0 - rate) * 100.0),
        ] {
            match (actual, expected_percent) {
                (Some(actual), Some(expected)) => assert!((actual - expected).abs() < 1e-10),
                (None, None) => {}
                _ => panic!("unexpected rate: {actual:?}, expected {expected_percent:?}"),
            }
        }
    }
}

#[tokio::test]
async fn dashboard_summary_should_project_rebuildable_runtime_slots() {
    let now = Utc::now();
    let store = Arc::new(FixtureObservabilityStore::new(observation_range(now)));
    store.replace_runtime_slots(Some(DashboardRuntimeSlots {
        inherited_versions: Vec::new(),
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
            inherited_versions: Vec::new(),
            inherited_accounts,
            overridden_slots,
            used_slots: Some(2),
        }));
        let services = crate::use_case::AdminHarness::new()
            .observability(store)
            .settings(Arc::new(FixtureSettingsStore {
                max_concurrent_per_account: 0,
            }))
            .provider(crate::use_case::dashboard_profile_provider())
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
async fn dashboard_should_preserve_provider_plan_names() {
    let now = Utc::now();
    let range = observation_range(now);
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let plans = [("prolite", "ProLite"), ("pro", "Pro"), ("promax", "ProMax")];
    *store.account_usage.lock().unwrap() = plans
        .iter()
        .map(|(plan, _)| DashboardAccountUsage {
            upstream: None,
            account_id: format!("account-{plan}"),
            provider_kind: "openai".to_owned(),
            authentication_kind: "oauth".to_owned(),
            name: (*plan).to_owned(),
            email: None,
            notes: None,
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
    let provider = crate::use_case::accounts::FakeProviderAdmin::new(
        "openai",
        Arc::new(Mutex::new(Vec::new())),
    );
    let services = crate::use_case::AdminHarness::new()
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
