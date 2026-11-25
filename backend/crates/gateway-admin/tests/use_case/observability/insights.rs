//! 用量洞察的指标聚合测试

use super::*;

#[tokio::test]
async fn usage_insights_should_weight_concurrency_by_effective_bucket_duration() {
    let bucket_start = DateTime::parse_from_rfc3339("2026-09-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let range = TimeRange::new(
        bucket_start + Duration::minutes(5),
        bucket_start + Duration::minutes(20),
    )
    .unwrap();
    let store = Arc::new(FixtureObservabilityStore::new(range));
    store.replace_trend(vec![
        health_metric_point(bucket_start, RequestMetrics::default()),
        health_metric_point(
            bucket_start + Duration::minutes(15),
            RequestMetrics::default(),
        ),
    ]);
    *store.concurrency.lock().unwrap() = vec![
        UsageConcurrencyPoint {
            bucket_start,
            average_concurrency: 1.0,
            peak_concurrency: 1,
        },
        UsageConcurrencyPoint {
            bucket_start: bucket_start + Duration::minutes(15),
            average_concurrency: 3.0,
            peak_concurrency: 4,
        },
    ];

    let insights = observability_services(store)
        .await
        .observability()
        .usage_insights(range, UsageFilter::default())
        .await
        .unwrap();

    assert!((insights.performance.load.average_concurrency - 5.0 / 3.0).abs() < 1e-10);
    assert_eq!(insights.performance.load.peak_concurrency, 4);
    assert_eq!(insights.performance.points[0].load.average_concurrency, 1.0);
    assert_eq!(insights.performance.points[0].load.peak_concurrency, 1);
    assert_eq!(insights.performance.points[1].load.average_concurrency, 3.0);
    assert_eq!(insights.performance.points[1].load.peak_concurrency, 4);
}
