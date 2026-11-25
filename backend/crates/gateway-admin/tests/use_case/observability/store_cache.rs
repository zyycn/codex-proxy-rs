//! 观测缓存的合并、隔离与失效测试

use super::*;

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
async fn dashboard_and_usage_insights_keep_concurrency_cache_entries_separate() {
    for dashboard_first in [true, false] {
        let now = Utc::now();
        let range = observation_range(now);
        let store = Arc::new(FixtureObservabilityStore::new(range));
        store
            .concurrency
            .lock()
            .unwrap()
            .push(UsageConcurrencyPoint {
                bucket_start: range.start,
                average_concurrency: 2.0,
                peak_concurrency: 3,
            });
        let services = observability_services(store.clone()).await;
        for dashboard in [
            dashboard_first,
            !dashboard_first,
            dashboard_first,
            !dashboard_first,
        ] {
            if dashboard {
                services
                    .observability()
                    .dashboard_trend(range, TrendKind::Usage)
                    .await
                    .unwrap();
            } else {
                let result = services
                    .observability()
                    .usage_insights(range, UsageFilter::default())
                    .await
                    .unwrap();
                assert_eq!(result.performance.load.peak_concurrency, 3);
            }
        }
        assert_eq!(
            *store.concurrency_selections.lock().unwrap(),
            vec![!dashboard_first, dashboard_first]
        );
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
    assert_eq!(refreshed.observation.analytics.generation, 1);
    assert_eq!(refreshed.observation.requests.request_count, 7);
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
        first
            .expect("first summary")
            .observation
            .analytics
            .generation,
        1
    );
    assert_eq!(
        second
            .expect("coalesced summary")
            .observation
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

#[tokio::test]
async fn usage_metric_cache_isolates_selections_and_filters() {
    use gateway_admin::model::observability::UsageInsightMetric as Metric;
    let range = observation_range(Utc::now());
    let store = Arc::new(FixtureObservabilityStore::new(range));
    let services = observability_services(store.clone()).await;
    let metrics = [
        Metric::Health,
        Metric::Latency,
        Metric::FirstToken,
        Metric::Throughput,
        Metric::Load,
        Metric::Cost,
        Metric::Savings,
        Metric::Cache,
    ];
    for _ in 0..2 {
        for metric in metrics {
            services
                .observability()
                .usage_metric(range, UsageFilter::default(), metric)
                .await
                .unwrap();
        }
    }
    assert_eq!(*store.metric_selections.lock().unwrap(), metrics);
    assert_eq!(
        *store.concurrency_selections.lock().unwrap(),
        vec![false, false, false, false, true, false, false, false]
    );
    services
        .observability()
        .usage_metric(
            range,
            UsageFilter {
                provider_kind: Some("openai".into()),
                ..Default::default()
            },
            Metric::Latency,
        )
        .await
        .unwrap();
    assert_eq!(store.metric_selections.lock().unwrap().len(), 9);
}
