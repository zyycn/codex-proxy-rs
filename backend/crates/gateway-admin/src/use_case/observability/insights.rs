//! 用量洞察与费用场景的纯聚合

use super::billing::{amount_difference, usd_cost};
use super::metrics::{rate_or_zero, ratio};
use crate::model::AdminError;
use crate::model::observability::{
    DecimalAmount, RequestHealth, RequestMetricPoint, UsageCacheEfficiency, UsageConcurrencyPoint,
    UsageCostComparison, UsageCostSummary, UsageInsight, UsageInsightData, UsageInsightMetric,
    UsageInsights, UsageInsightsCost, UsageInsightsCostPoint, UsageInsightsHealth,
    UsageInsightsHealthPoint, UsageInsightsLoad, UsageInsightsObservation,
    UsageInsightsPerformance, UsageInsightsPerformancePoint, UsageMetricPoint, UsageMetricSeries,
    UsageOverview, UsageThroughputPercentiles,
};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

pub(super) fn insights_from_observation(
    observation: UsageInsightsObservation,
) -> Result<UsageInsights, AdminError> {
    let mut standard_costs = UsageCostScenarios::default();
    for point in observation.costs {
        if let Some(amount) = point.standard_amount {
            standard_costs.standard.add(point.bucket_start, amount)?;
        }
        if let Some(amount) = point.no_cache_amount {
            standard_costs.no_cache.add(point.bucket_start, amount)?;
        }
    }
    let overview = observation.overview;
    let trend = observation.trend;
    build_usage_insights(overview, trend, standard_costs, observation.concurrency)
}

pub(super) fn select_usage_metric(
    insights: UsageInsights,
    metric: UsageInsightMetric,
) -> UsageInsight {
    let activity = insights
        .health
        .points
        .iter()
        .map(|point| (point.bucket_start, point.total_requests))
        .collect::<BTreeMap<_, _>>();
    let data = match metric {
        UsageInsightMetric::Health => UsageInsightData::Health(insights.health),
        UsageInsightMetric::Latency => UsageInsightData::Latency(metric_series(
            insights.performance.latency_percentiles,
            insights
                .performance
                .points
                .into_iter()
                .map(|point| (point.bucket_start, point.latency_percentiles)),
            &activity,
        )),
        UsageInsightMetric::FirstToken => UsageInsightData::FirstToken(metric_series(
            insights.performance.first_token_latency_percentiles,
            insights
                .performance
                .points
                .into_iter()
                .map(|point| (point.bucket_start, point.first_token_latency_percentiles)),
            &activity,
        )),
        UsageInsightMetric::Throughput => UsageInsightData::Throughput(metric_series(
            UsageThroughputPercentiles {
                p10: insights.performance.output_throughput_p10,
                p50: insights.performance.output_throughput_p50,
                p90: insights.performance.output_throughput_p90,
            },
            insights.performance.points.into_iter().map(|point| {
                (
                    point.bucket_start,
                    UsageThroughputPercentiles {
                        p10: point.output_throughput_p10,
                        p50: point.output_throughput_p50,
                        p90: point.output_throughput_p90,
                    },
                )
            }),
            &activity,
        )),
        UsageInsightMetric::Load => UsageInsightData::Load(metric_series(
            insights.performance.load,
            insights
                .performance
                .points
                .into_iter()
                .map(|point| (point.bucket_start, point.load)),
            &activity,
        )),
        UsageInsightMetric::Cost => UsageInsightData::Cost(metric_series(
            cost_summary(&insights.cost),
            insights.cost.points.into_iter().map(|point| {
                (
                    point.bucket_start,
                    UsageCostComparison {
                        estimated_cost: point.estimated_cost,
                        no_cache_cost: point.no_cache_cost,
                    },
                )
            }),
            &activity,
        )),
        UsageInsightMetric::Savings => UsageInsightData::Savings(metric_series(
            cost_summary(&insights.cost),
            insights
                .cost
                .points
                .into_iter()
                .map(|point| (point.bucket_start, point.cache_savings)),
            &activity,
        )),
        UsageInsightMetric::Cache => UsageInsightData::Cache(metric_series(
            cost_summary(&insights.cost),
            insights.cost.points.into_iter().map(|point| {
                (
                    point.bucket_start,
                    UsageCacheEfficiency {
                        input_tokens: point.input_tokens,
                        cached_token_rate: point.cached_token_rate,
                        cache_hit_request_rate: point.cache_hit_request_rate,
                    },
                )
            }),
            &activity,
        )),
    };
    UsageInsight {
        analytics: insights.analytics,
        granularity: insights.granularity,
        data,
    }
}

fn cost_summary(cost: &UsageInsightsCost) -> UsageCostSummary {
    UsageCostSummary {
        estimated_cost: cost.estimated_cost.clone(),
        cache_savings: cost.cache_savings.clone(),
        tier_premium: cost.tier_premium.clone(),
        cost_per_successful_request: cost.cost_per_successful_request.clone(),
        coverage: cost.coverage.clone(),
    }
}

fn metric_series<S, P>(
    summary: S,
    points: impl Iterator<Item = (DateTime<Utc>, P)>,
    activity: &BTreeMap<DateTime<Utc>, u64>,
) -> UsageMetricSeries<S, P> {
    UsageMetricSeries {
        summary,
        points: points
            .map(|(bucket_start, value)| UsageMetricPoint {
                bucket_start,
                request_count: activity.get(&bucket_start).copied().unwrap_or_default(),
                value,
            })
            .collect(),
    }
}

fn build_usage_insights(
    overview: UsageOverview,
    trend: Vec<RequestMetricPoint>,
    standard_costs: UsageCostScenarios,
    concurrency: Vec<UsageConcurrencyPoint>,
) -> Result<UsageInsights, AdminError> {
    let granularity = trend.first().map_or(
        crate::model::observability::Granularity::FifteenMinutes,
        |point| point.granularity,
    );
    if trend.iter().any(|point| point.granularity != granularity) {
        return Err(AdminError::internal("用量洞察趋势包含不一致的时间粒度"));
    }

    let requests = &overview.requests;
    let health_window = RequestHealth::from_metrics(requests);
    let failed_requests = health_window.failed_requests;
    let health = UsageInsightsHealth {
        total_requests: requests.request_count,
        success_requests: requests.success_count,
        failed_requests,
        cancelled_requests: requests.cancelled_count,
        incomplete_requests: requests.incomplete_count,
        caller_error_requests: requests.caller_error_count,
        success_rate: health_window.success_ratio(),
        completion_rate: rate_or_zero(
            requests
                .success_count
                .saturating_add(requests.failure_count),
            requests.request_count,
        ),
        points: trend
            .iter()
            .map(|point| {
                let health_window = RequestHealth::from_metrics(&point.metrics);
                UsageInsightsHealthPoint {
                    total_requests: point.metrics.request_count,
                    bucket_start: point.bucket_start,
                    success_requests: point.metrics.success_count,
                    failed_requests: health_window.failed_requests,
                    cancelled_requests: point.metrics.cancelled_count,
                    incomplete_requests: point.metrics.incomplete_count,
                    caller_error_requests: point.metrics.caller_error_count,
                    error_rate: health_window.error_ratio(),
                }
            })
            .collect(),
    };
    let concurrency_by_bucket = concurrency
        .iter()
        .map(|point| (point.bucket_start, point))
        .collect::<BTreeMap<_, _>>();
    let mut weighted_concurrency_micros = 0.0;
    let mut effective_micros = 0_i64;
    for (index, point) in concurrency.iter().enumerate() {
        let start = point.bucket_start.max(overview.range.start);
        let end = concurrency
            .get(index + 1)
            .map_or(overview.range.end, |next| next.bucket_start)
            .min(overview.range.end);
        let micros = (end - start).num_microseconds().unwrap_or(0).max(0);
        effective_micros = effective_micros.saturating_add(micros);
        weighted_concurrency_micros += point.average_concurrency * micros as f64;
    }
    let average_concurrency = if effective_micros > 0 {
        weighted_concurrency_micros / effective_micros as f64
    } else {
        0.0
    };
    let peak_concurrency = concurrency
        .iter()
        .map(|point| point.peak_concurrency)
        .max()
        .unwrap_or(0);
    let performance = UsageInsightsPerformance {
        latency_percentiles: requests.latency_percentiles,
        first_token_latency_percentiles: requests.first_token_latency_percentiles,
        output_throughput_p10: requests.output_throughput_p10,
        output_throughput_p50: requests.output_throughput_p50,
        output_throughput_p90: requests.output_throughput_p90,
        load: UsageInsightsLoad {
            average_concurrency,
            peak_concurrency,
            admission_decision_p95_ms: requests.admission_decision_percentiles.p95_ms,
            account_selection_wait_p95_ms: requests.account_selection_wait_percentiles.p95_ms,
        },
        points: trend
            .iter()
            .map(|point| {
                let concurrency = concurrency_by_bucket.get(&point.bucket_start);
                UsageInsightsPerformancePoint {
                    bucket_start: point.bucket_start,
                    latency_percentiles: point.metrics.latency_percentiles,
                    first_token_latency_percentiles: point.metrics.first_token_latency_percentiles,
                    output_throughput_p10: point.metrics.output_throughput_p10,
                    output_throughput_p50: point.metrics.output_throughput_p50,
                    output_throughput_p90: point.metrics.output_throughput_p90,
                    load: UsageInsightsLoad {
                        average_concurrency: concurrency
                            .map_or(0.0, |point| point.average_concurrency),
                        peak_concurrency: concurrency.map_or(0, |point| point.peak_concurrency),
                        admission_decision_p95_ms: point
                            .metrics
                            .admission_decision_percentiles
                            .p95_ms,
                        account_selection_wait_p95_ms: point
                            .metrics
                            .account_selection_wait_percentiles
                            .p95_ms,
                    },
                }
            })
            .collect(),
    };
    let estimated_cost = usd_cost(&overview.attempts.costs);
    let UsageCostScenarios {
        standard:
            ScenarioCosts {
                total: standard_cost,
                by_bucket: standard_costs_by_bucket,
            },
        no_cache:
            ScenarioCosts {
                total: no_cache_cost,
                by_bucket: no_cache_costs_by_bucket,
            },
    } = standard_costs;
    let cache_savings = amount_difference(no_cache_cost.as_ref(), estimated_cost.as_ref());
    let tier_premium = amount_difference(estimated_cost.as_ref(), standard_cost.as_ref());
    let cost_per_request = estimated_cost
        .as_ref()
        .and_then(|cost| cost.checked_div_u64(requests.request_count));
    let cost_per_successful_request = estimated_cost
        .as_ref()
        .and_then(|cost| cost.checked_div_u64(requests.success_count));
    let cost = UsageInsightsCost {
        cost_per_request,
        estimated_cost,
        standard_cost,
        no_cache_cost,
        cache_savings,
        tier_premium,
        tokens_per_request: rate_or_zero(requests.total_tokens, requests.request_count),
        cost_per_successful_request,
        cached_token_rate: rate_or_zero(requests.cached_input_tokens, requests.input_tokens),
        cache_hit_request_rate: ratio(
            requests.cache_hit_request_count,
            requests.cache_eligible_request_count,
        ),
        input_tokens: requests.input_tokens,
        output_tokens: requests.output_tokens,
        cached_input_tokens: requests.cached_input_tokens,
        total_tokens: requests.total_tokens,
        points: trend
            .iter()
            .map(|point| UsageInsightsCostPoint {
                bucket_start: point.bucket_start,
                input_tokens: point.metrics.input_tokens,
                output_tokens: point.metrics.output_tokens,
                cached_input_tokens: point.metrics.cached_input_tokens,
                total_tokens: point.metrics.total_tokens,
                estimated_cost: usd_cost(&point.costs),
                standard_cost: standard_costs_by_bucket.get(&point.bucket_start).cloned(),
                no_cache_cost: no_cache_costs_by_bucket.get(&point.bucket_start).cloned(),
                cache_savings: amount_difference(
                    no_cache_costs_by_bucket.get(&point.bucket_start),
                    usd_cost(&point.costs).as_ref(),
                ),
                cached_token_rate: rate_or_zero(
                    point.metrics.cached_input_tokens,
                    point.metrics.input_tokens,
                ),
                cache_hit_request_rate: ratio(
                    point.metrics.cache_hit_request_count,
                    point.metrics.cache_eligible_request_count,
                ),
            })
            .collect(),
        costs: overview.attempts.costs.clone(),
        coverage: overview.attempts.cost_coverage.clone(),
    };
    Ok(UsageInsights {
        analytics: overview.analytics,
        granularity,
        health,
        performance,
        cost,
        attempts: overview.attempts,
        providers: overview.providers,
    })
}

#[derive(Debug, Default)]
struct ScenarioCosts {
    total: Option<DecimalAmount>,
    by_bucket: BTreeMap<DateTime<Utc>, DecimalAmount>,
}

impl ScenarioCosts {
    fn add(
        &mut self,
        bucket_start: DateTime<Utc>,
        amount: DecimalAmount,
    ) -> Result<(), AdminError> {
        self.total = Some(match self.total.take() {
            Some(current) => current
                .checked_add(&amount)
                .ok_or_else(|| AdminError::internal("用量标准成本超出数值范围"))?,
            None => amount.clone(),
        });
        match self.by_bucket.entry(bucket_start) {
            Entry::Occupied(mut entry) => {
                let sum = entry
                    .get()
                    .checked_add(&amount)
                    .ok_or_else(|| AdminError::internal("用量标准成本超出数值范围"))?;
                entry.insert(sum);
            }
            Entry::Vacant(entry) => {
                entry.insert(amount);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct UsageCostScenarios {
    standard: ScenarioCosts,
    no_cache: ScenarioCosts,
}
