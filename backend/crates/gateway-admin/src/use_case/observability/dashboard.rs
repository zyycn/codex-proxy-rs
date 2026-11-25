//! 仪表盘趋势、健康时间桶与期间指标

use super::metrics::{average, rate_or_zero, ratio};
use crate::model::AdminError;
use crate::model::observability::{
    CostCoverage, CurrencyCost, DashboardPeriodMetrics, DecimalAmount, HealthStatus,
    HealthTimeline, HealthTimelinePoint, RequestHealth, RequestMetricPoint, RequestMetrics, Trend,
    TrendKind, TrendPoint, TrendSummary,
};
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;

const HEALTH_TIMELINE_SLOT_MINUTES: i64 = 15;
const HEALTH_TIMELINE_MIN_SAMPLE_SIZE: u64 = 10;
const HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD: u64 = 3;
const HEALTH_TIMELINE_STABLE_RELIABILITY: f64 = 95.0;
/// 按部署时区的自然日计算 15 分钟健康桶，桶数随夏令时变化
pub(in crate::use_case) fn health_timeline_at(
    records: &[RequestMetricPoint],
    now: DateTime<Utc>,
    timezone: gateway_core::time::DeploymentTimeZone,
) -> Result<HealthTimeline, AdminError> {
    let start = timezone
        .day_start(now)
        .ok_or_else(|| AdminError::internal("时间超出支持范围"))?;
    let end = timezone
        .days_after(now, 1)
        .ok_or_else(|| AdminError::internal("时间超出支持范围"))?;
    let current_slot = start
        + Duration::minutes(
            (now - start)
                .num_minutes()
                .div_euclid(HEALTH_TIMELINE_SLOT_MINUTES)
                * HEALTH_TIMELINE_SLOT_MINUTES,
        );
    let slots = ((end - start).num_minutes() + HEALTH_TIMELINE_SLOT_MINUTES - 1)
        / HEALTH_TIMELINE_SLOT_MINUTES;
    let mut buckets = (0..slots)
        .map(|index| {
            (
                start + Duration::minutes(HEALTH_TIMELINE_SLOT_MINUTES * index),
                RequestHealth::default(),
            )
        })
        .collect::<Vec<_>>();
    for record in records {
        if record.bucket_start < start || record.bucket_start > now {
            continue;
        }
        let record_slot = start
            + Duration::minutes(
                (record.bucket_start - start)
                    .num_minutes()
                    .div_euclid(HEALTH_TIMELINE_SLOT_MINUTES)
                    * HEALTH_TIMELINE_SLOT_MINUTES,
            );
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(bucket_start, _)| *bucket_start == record_slot)
        {
            bucket.add_metrics(&record.metrics);
        }
    }

    let totals = buckets
        .iter()
        .filter(|(bucket_start, _)| *bucket_start <= current_slot)
        .fold(RequestHealth::default(), |mut totals, (_, bucket)| {
            totals.add_window(*bucket);
            totals
        });
    Ok(HealthTimeline {
        reliability_percent: health_reliability(totals),
        status: health_status(totals, false),
        success_requests: totals.success_requests,
        failed_requests: totals.failed_requests,
        cancelled_requests: totals.cancelled_requests,
        incomplete_requests: totals.incomplete_requests,
        caller_error_requests: totals.caller_error_requests,
        points: buckets
            .into_iter()
            .map(|(bucket_start, bucket)| HealthTimelinePoint {
                bucket_start,
                status: health_status(bucket, bucket_start > current_slot),
                reliability_percent: health_reliability(bucket),
                success_requests: bucket.success_requests,
                failed_requests: bucket.failed_requests,
                cancelled_requests: bucket.cancelled_requests,
                incomplete_requests: bucket.incomplete_requests,
                caller_error_requests: bucket.caller_error_requests,
            })
            .collect(),
    })
}

pub(super) fn trend(
    kind: TrendKind,
    points: Vec<RequestMetricPoint>,
    analytics: crate::model::analytics::AnalyticsMetadata,
) -> Result<Trend, AdminError> {
    let mut metrics = RequestMetrics::default();
    let mut coverage = CostCoverage::default();
    let mut costs = BTreeMap::<String, DecimalAmount>::new();
    for point in &points {
        add_request_metrics(&mut metrics, &point.metrics);
        coverage.provider_reported_count = coverage
            .provider_reported_count
            .saturating_add(point.cost_coverage.provider_reported_count);
        coverage.calculated_count = coverage
            .calculated_count
            .saturating_add(point.cost_coverage.calculated_count);
        coverage.partial_count = coverage
            .partial_count
            .saturating_add(point.cost_coverage.partial_count);
        coverage.unavailable_count = coverage
            .unavailable_count
            .saturating_add(point.cost_coverage.unavailable_count);
        coverage.not_billable_count = coverage
            .not_billable_count
            .saturating_add(point.cost_coverage.not_billable_count);
        for cost in &point.costs {
            if let Some(amount) = costs.get_mut(&cost.currency) {
                *amount = amount
                    .checked_add(&cost.amount)
                    .ok_or_else(|| AdminError::internal("成本聚合结果超出数值范围"))?;
            } else {
                costs.insert(cost.currency.clone(), cost.amount.clone());
            }
        }
    }
    let average_latency_ms = average(metrics.latency_sum_ms, metrics.latency_count);
    let health = RequestHealth::from_metrics(&metrics);
    let error_count = health.non_success_requests();
    let success_rate = health.success_ratio();
    let cache_hit_request_rate = ratio(
        metrics.cache_hit_request_count,
        metrics.cache_eligible_request_count,
    );
    let peak_first_token_p95_ms = points
        .iter()
        .filter_map(|point| point.metrics.first_token_latency_percentiles.p95_ms)
        .map(|value| value.as_f64())
        .reduce(f64::max);
    let peak_latency_p95_ms = points
        .iter()
        .filter_map(|point| point.metrics.latency_percentiles.p95_ms)
        .map(|value| value.as_f64())
        .reduce(f64::max);
    let minimum_output_throughput_p50 = points
        .iter()
        .filter_map(|point| point.metrics.output_throughput_p50)
        .min();
    Ok(Trend {
        analytics,
        kind,
        points: points.into_iter().map(trend_point).collect(),
        summary: TrendSummary {
            request_count: metrics.request_count,
            input_tokens: metrics.input_tokens,
            output_tokens: metrics.output_tokens,
            cached_input_tokens: metrics.cached_input_tokens,
            total_tokens: metrics.total_tokens,
            error_count,
            average_latency_ms,
            max_latency_ms: metrics.max_latency_ms,
            min_latency_ms: metrics.min_latency_ms,
            peak_first_token_p95_ms,
            peak_latency_p95_ms,
            minimum_output_throughput_p50,
            success_rate,
            cache_hit_request_rate,
            costs: costs
                .into_iter()
                .map(|(currency, amount)| CurrencyCost { currency, amount })
                .collect(),
            cost_coverage: coverage,
        },
    })
}

fn trend_point(point: RequestMetricPoint) -> TrendPoint {
    let health = RequestHealth::from_metrics(&point.metrics);
    TrendPoint {
        bucket_start: point.bucket_start,
        granularity: point.granularity,
        error_count: health.non_success_requests(),
        average_latency_ms: average(point.metrics.latency_sum_ms, point.metrics.latency_count),
        average_first_token_latency_ms: average(
            point.metrics.first_token_latency_sum_ms,
            point.metrics.first_token_latency_count,
        ),
        cached_token_rate: rate_or_zero(
            point.metrics.cached_input_tokens,
            point.metrics.input_tokens,
        ),
        cache_hit_request_rate: ratio(
            point.metrics.cache_hit_request_count,
            point.metrics.cache_eligible_request_count,
        ),
        success_rate: health.success_ratio(),
        metrics: point.metrics,
        cost_coverage: point.cost_coverage,
        costs: point.costs,
    }
}

pub(super) fn dashboard_period_metrics<'a>(
    metrics: impl IntoIterator<Item = &'a RequestMetrics>,
) -> DashboardPeriodMetrics {
    let mut result = DashboardPeriodMetrics::default();
    for metrics in metrics {
        result.request_count = result.request_count.saturating_add(metrics.request_count);
        result.input_tokens = result.input_tokens.saturating_add(metrics.input_tokens);
        result.output_tokens = result.output_tokens.saturating_add(metrics.output_tokens);
        result.cached_input_tokens = result
            .cached_input_tokens
            .saturating_add(metrics.cached_input_tokens);
        result.total_tokens = result.total_tokens.saturating_add(metrics.total_tokens);
    }
    result.cached_token_rate = rate_or_zero(result.cached_input_tokens, result.input_tokens);
    result.observed_cached_token_rate = ratio(result.cached_input_tokens, result.input_tokens);
    result
}

fn add_request_metrics(total: &mut RequestMetrics, value: &RequestMetrics) {
    total.request_count = total.request_count.saturating_add(value.request_count);
    total.success_count = total.success_count.saturating_add(value.success_count);
    total.failure_count = total.failure_count.saturating_add(value.failure_count);
    total.cancelled_count = total.cancelled_count.saturating_add(value.cancelled_count);
    total.incomplete_count = total
        .incomplete_count
        .saturating_add(value.incomplete_count);
    total.caller_error_count = total
        .caller_error_count
        .saturating_add(value.caller_error_count);
    total.input_tokens = total.input_tokens.saturating_add(value.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(value.output_tokens);
    total.cached_input_tokens = total
        .cached_input_tokens
        .saturating_add(value.cached_input_tokens);
    total.cache_write_input_tokens = total
        .cache_write_input_tokens
        .saturating_add(value.cache_write_input_tokens);
    total.reasoning_output_tokens = total
        .reasoning_output_tokens
        .saturating_add(value.reasoning_output_tokens);
    total.total_tokens = total.total_tokens.saturating_add(value.total_tokens);
    total.first_token_latency_sum_ms = total
        .first_token_latency_sum_ms
        .saturating_add(value.first_token_latency_sum_ms);
    total.first_token_latency_count = total
        .first_token_latency_count
        .saturating_add(value.first_token_latency_count);
    total.latency_sum_ms = total.latency_sum_ms.saturating_add(value.latency_sum_ms);
    total.latency_count = total.latency_count.saturating_add(value.latency_count);
    total.min_latency_ms = match (total.min_latency_ms, value.min_latency_ms) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    };
    total.max_latency_ms = match (total.max_latency_ms, value.max_latency_ms) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    };
    total.cache_eligible_request_count = total
        .cache_eligible_request_count
        .saturating_add(value.cache_eligible_request_count);
    total.cache_hit_request_count = total
        .cache_hit_request_count
        .saturating_add(value.cache_hit_request_count);
}

fn health_status(bucket: RequestHealth, is_future: bool) -> HealthStatus {
    let eligible_requests = bucket.eligible_requests();
    if is_future {
        HealthStatus::Future
    } else if eligible_requests == 0 {
        HealthStatus::NoData
    } else if bucket.success_requests == 0
        && bucket.non_success_requests() >= HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD
    {
        HealthStatus::Unavailable
    } else if eligible_requests < HEALTH_TIMELINE_MIN_SAMPLE_SIZE {
        HealthStatus::LowSample
    } else if health_reliability(bucket)
        .is_some_and(|reliability| reliability < HEALTH_TIMELINE_STABLE_RELIABILITY)
    {
        HealthStatus::Unstable
    } else {
        HealthStatus::Stable
    }
}

fn health_reliability(bucket: RequestHealth) -> Option<f64> {
    bucket.success_rate()
}
