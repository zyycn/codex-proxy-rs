//! 单项洞察保留统计口径，未选择的分布和并发不进入结果

use super::*;
use admin_observability::{UsageInsightMetric as Metric, UsageInsightsObservation};

#[tokio::test]
async fn usage_metrics_preserve_selected_values_for_aggregate_and_exact_reads() {
    let Some(database) = TestDatabase::create("usage_metrics").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now).await.unwrap();
    seed_calculated_billing_facts(&database.pool, now)
        .await
        .unwrap();
    super::super::drain_analytics(&database.pool).await;
    let store = admin_observability_store(&database.pool);
    let range =
        admin_observability::TimeRange::new(now - TimeDelta::hours(2), now + TimeDelta::hours(1))
            .unwrap();
    let granularity = admin_observability::Granularity::FifteenMinutes;
    for filter in [
        admin_observability::UsageFilter::default(),
        admin_observability::UsageFilter {
            model: Some("public-model".to_owned()),
            ..Default::default()
        },
    ] {
        let full = store
            .usage_insights_observation(range, filter.clone(), granularity, true)
            .await
            .unwrap();
        assert!(full.overview.requests.request_count > 0);
        for metric in [
            Metric::Health,
            Metric::Latency,
            Metric::FirstToken,
            Metric::Throughput,
            Metric::Load,
            Metric::Cost,
            Metric::Savings,
            Metric::Cache,
        ] {
            let selected = store
                .usage_metric_observation(range, filter.clone(), granularity, metric)
                .await
                .unwrap();
            assert_eq!(
                selected.overview.requests.request_count, full.overview.requests.request_count,
                "{metric:?}"
            );
            assert_eq!(selected.trend.len(), full.trend.len());
            assert_selected_metrics(&selected.overview.requests, &full.overview.requests, metric);
            for (actual, expected) in selected.trend.iter().zip(&full.trend) {
                assert_eq!(actual.bucket_start, expected.bucket_start);
                assert_eq!(actual.metrics.request_count, expected.metrics.request_count);
                assert_selected_metrics(&actual.metrics, &expected.metrics, metric);
            }
            assert_costs(&selected, &full, metric);
            if metric == Metric::Load {
                assert_eq!(selected.concurrency, full.concurrency);
            } else {
                assert!(selected.concurrency.is_empty(), "{metric:?}");
            }
        }
    }
    database.close().await;
}

fn assert_selected_metrics(
    actual: &admin_observability::RequestMetrics,
    expected: &admin_observability::RequestMetrics,
    metric: Metric,
) {
    match metric {
        Metric::Health => {
            assert_eq!(actual.success_count, expected.success_count);
            assert_eq!(actual.failure_count, expected.failure_count);
            assert_eq!(actual.cancelled_count, expected.cancelled_count);
            assert_eq!(actual.incomplete_count, expected.incomplete_count);
            assert_eq!(actual.caller_error_count, expected.caller_error_count);
        }
        Metric::Latency => assert_eq!(actual.latency_percentiles, expected.latency_percentiles),
        Metric::FirstToken => assert_eq!(
            actual.first_token_latency_percentiles,
            expected.first_token_latency_percentiles
        ),
        Metric::Throughput => assert_eq!(
            (
                actual.output_throughput_p10,
                actual.output_throughput_p50,
                actual.output_throughput_p90
            ),
            (
                expected.output_throughput_p10,
                expected.output_throughput_p50,
                expected.output_throughput_p90
            )
        ),
        Metric::Load => {
            assert_eq!(
                actual.admission_decision_percentiles,
                expected.admission_decision_percentiles
            );
            assert_eq!(
                actual.account_selection_wait_percentiles,
                expected.account_selection_wait_percentiles
            );
        }
        Metric::Cost | Metric::Savings => {
            assert_eq!(actual.success_count, expected.success_count);
        }
        Metric::Cache => {
            assert_eq!(actual.success_count, expected.success_count);
            assert_eq!(actual.input_tokens, expected.input_tokens);
            assert_eq!(actual.cached_input_tokens, expected.cached_input_tokens);
            assert_eq!(
                actual.cache_hit_request_count,
                expected.cache_hit_request_count
            );
            assert_eq!(
                actual.cache_eligible_request_count,
                expected.cache_eligible_request_count
            );
        }
    }
    if metric != Metric::Latency {
        assert_eq!(actual.latency_percentiles, Default::default());
    }
    if metric != Metric::FirstToken {
        assert_eq!(actual.first_token_latency_percentiles, Default::default());
    }
    if metric != Metric::Throughput {
        assert_eq!(actual.output_throughput_p50, None);
    }
    if metric != Metric::Load {
        assert_eq!(actual.admission_decision_percentiles, Default::default());
    }
}

fn assert_costs(
    actual: &UsageInsightsObservation,
    expected: &UsageInsightsObservation,
    metric: Metric,
) {
    if matches!(metric, Metric::Cost | Metric::Savings | Metric::Cache) {
        assert_eq!(
            actual.overview.attempts.costs,
            expected.overview.attempts.costs
        );
        assert_eq!(
            actual.overview.attempts.cost_coverage,
            expected.overview.attempts.cost_coverage
        );
        assert_eq!(actual.costs, expected.costs);
        for (actual, expected) in actual.trend.iter().zip(&expected.trend) {
            assert_eq!(actual.costs, expected.costs);
            assert_eq!(actual.cost_coverage, expected.cost_coverage);
        }
    } else {
        assert!(actual.overview.attempts.costs.is_empty());
        assert!(actual.costs.is_empty());
    }
}
