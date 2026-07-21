//! 观测查询、趋势、费用覆盖与健康阈值规则。

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Timelike as _, Utc};
use gateway_core::routing::ProviderKind;

use crate::{
    model::{
        AdminError,
        observability::{
            CostCoverage, CurrencyCost, DashboardCapacity, DashboardPeriodMetrics, DashboardResult,
            DecimalAmount, DiagnosticDimension, DiagnosticsItem, DiagnosticsResult, HealthStatus,
            HealthTimeline, HealthTimelinePoint, OpsErrorPage, OpsErrorQuery, ProviderBillingInput,
            RequestMetricPoint, RequestMetrics, TimeRange, Trend, TrendKind, TrendPoint,
            TrendSummary, UsageBilling, UsageDetail, UsageFilter, UsageInsights, UsageInsightsCost,
            UsageInsightsCostPoint, UsageInsightsHealth, UsageInsightsHealthPoint,
            UsageInsightsPerformance, UsageInsightsPerformancePoint, UsageOverview, UsagePage,
            UsageQuery, UsageSummary,
        },
    },
    ports::{
        provider::ProviderAdminRegistry,
        store::{ObservabilityStore, SettingsStore},
    },
};

use super::{map_provider_error, map_store_error};

const HEALTH_TIMELINE_SLOT_MINUTES: i64 = 15;
const HEALTH_TIMELINE_SLOTS: i64 = 24 * 4;
const HEALTH_TIMELINE_MIN_SAMPLE_SIZE: u64 = 10;
const HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD: u64 = 3;
const HEALTH_TIMELINE_STABLE_RELIABILITY: f64 = 99.0;
const CHINA_OFFSET_SECONDS: i64 = 8 * 60 * 60;

/// API 消费的观测控制面服务。
#[async_trait]
pub trait ObservabilityService: Send + Sync {
    async fn dashboard_summary(
        &self,
        range: TimeRange,
        kind: TrendKind,
    ) -> Result<DashboardResult, AdminError>;
    async fn dashboard_trend(&self, range: TimeRange, kind: TrendKind)
    -> Result<Trend, AdminError>;
    async fn usage_records(&self, query: UsageQuery) -> Result<UsagePage, AdminError>;
    async fn usage_record_detail(&self, request_id: &str) -> Result<UsageDetail, AdminError>;
    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageSummary, AdminError>;
    async fn usage_insights(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageInsights, AdminError>;
    async fn diagnostics(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        dimension: DiagnosticDimension,
    ) -> Result<DiagnosticsResult, AdminError>;
    async fn ops_errors(&self, query: OpsErrorQuery) -> Result<OpsErrorPage, AdminError>;
}

pub(crate) struct DefaultObservabilityService {
    store: Arc<dyn ObservabilityStore>,
    settings: Arc<dyn SettingsStore>,
    providers: ProviderAdminRegistry,
}

impl DefaultObservabilityService {
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn ObservabilityStore>,
        settings: Arc<dyn SettingsStore>,
        providers: ProviderAdminRegistry,
    ) -> Self {
        Self {
            store,
            settings,
            providers,
        }
    }
}

#[async_trait]
impl ObservabilityService for DefaultObservabilityService {
    async fn dashboard_summary(
        &self,
        range: TimeRange,
        kind: TrendKind,
    ) -> Result<DashboardResult, AdminError> {
        let (mut observation, settings) = futures::try_join!(
            self.store.dashboard_summary(range),
            self.settings.load_runtime_settings(),
        )
        .map_err(|error| map_store_error(error, "dashboard"))?;
        self.enrich_billing(&mut observation.recent_requests)?;
        let today_start = china_day_start(observation.range.end);
        let yesterday_start = today_start - Duration::days(1);
        let today =
            dashboard_period_metrics(&observation.trend, today_start, observation.range.end);
        let yesterday = dashboard_period_metrics(&observation.trend, yesterday_start, today_start);
        let total_billing_usd = observation
            .attempts
            .costs
            .iter()
            .find(|cost| cost.currency == "USD")
            .map(|cost| cost.amount.clone());
        let total_cached_token_rate = rate_or_zero(
            observation.requests.cached_tokens,
            observation.requests.input_tokens,
        );
        let average_first_token_latency_ms = average(
            observation.requests.first_token_latency_sum_ms,
            observation.requests.first_token_latency_count,
        );
        let trend = trend(kind, observation.trend.clone())?;
        let health_timeline = health_timeline_at(&observation.trend, Utc::now());
        let wire_profiles = self.providers.dashboard_wire_profiles();
        let max_concurrent_per_account = u64::from(settings.max_concurrent_per_account);
        Ok(DashboardResult {
            capacity: DashboardCapacity {
                max_concurrent_per_account,
                total_slots: observation
                    .provider_accounts
                    .active
                    .saturating_mul(max_concurrent_per_account),
                used_slots: None,
                available_slots: None,
            },
            rotation_strategy: settings.rotation_strategy,
            observation,
            today,
            yesterday,
            total_billing_usd,
            total_cached_token_rate,
            average_first_token_latency_ms,
            trend,
            health_timeline,
            wire_profiles,
        })
    }

    async fn dashboard_trend(
        &self,
        range: TimeRange,
        kind: TrendKind,
    ) -> Result<Trend, AdminError> {
        let points = self
            .store
            .dashboard_trend(range)
            .await
            .map_err(|error| map_store_error(error, "dashboard trend"))?;
        trend(kind, points)
    }

    async fn usage_records(&self, query: UsageQuery) -> Result<UsagePage, AdminError> {
        let mut page = self
            .store
            .list_usage_records(query)
            .await
            .map_err(|error| map_store_error(error, "usage records"))?;
        self.enrich_billing(&mut page.items)?;
        Ok(page)
    }

    async fn usage_record_detail(&self, request_id: &str) -> Result<UsageDetail, AdminError> {
        if request_id.trim().is_empty() {
            return Err(AdminError::invalid("Usage record ID is required"));
        }
        let mut detail = self
            .store
            .usage_record_detail(request_id)
            .await
            .map_err(|error| map_store_error(error, "usage record"))?;
        self.enrich_billing(std::slice::from_mut(&mut detail.request))?;
        Ok(detail)
    }

    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageSummary, AdminError> {
        let overview = self
            .store
            .usage_summary(range, filter)
            .await
            .map_err(|error| map_store_error(error, "usage summary"))?;
        let average_latency_ms = average(
            overview.requests.latency_sum_ms,
            overview.requests.latency_count,
        );
        Ok(UsageSummary {
            overview,
            average_latency_ms,
        })
    }

    async fn usage_insights(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageInsights, AdminError> {
        let (overview, trend) = futures::try_join!(
            self.store.usage_summary(range, filter.clone()),
            self.store.usage_trend(range, filter),
        )
        .map_err(|error| map_store_error(error, "usage insights"))?;
        build_usage_insights(overview, trend)
    }

    async fn diagnostics(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        dimension: DiagnosticDimension,
    ) -> Result<DiagnosticsResult, AdminError> {
        let items = self
            .store
            .usage_diagnostics(range, filter, dimension)
            .await
            .map_err(|error| map_store_error(error, "usage diagnostics"))?;
        let total_requests = items.iter().fold(0_u64, |total, item| {
            total.saturating_add(item.request_count)
        });
        Ok(DiagnosticsResult {
            dimension,
            items: items
                .into_iter()
                .map(|item| DiagnosticsItem {
                    name: item.name,
                    request_count: item.request_count,
                    success_count: item.request_count.saturating_sub(item.failure_count),
                    error_count: item.failure_count,
                    error_rate: rate_or_zero(item.failure_count, item.request_count),
                    request_share: rate_or_zero(item.request_count, total_requests),
                    average_latency_ms: item.average_latency_ms,
                    estimated_cost: None,
                    attempt_count: item.attempt_count,
                    total_tokens: item.total_tokens,
                })
                .collect(),
        })
    }

    async fn ops_errors(&self, query: OpsErrorQuery) -> Result<OpsErrorPage, AdminError> {
        self.store
            .list_ops_errors(query)
            .await
            .map_err(|error| map_store_error(error, "operations errors"))
    }
}

fn build_usage_insights(
    overview: UsageOverview,
    trend: Vec<RequestMetricPoint>,
) -> Result<UsageInsights, AdminError> {
    let granularity = trend.first().map_or(
        crate::model::observability::Granularity::FifteenMinutes,
        |point| point.granularity,
    );
    if trend.iter().any(|point| point.granularity != granularity) {
        return Err(AdminError::internal(
            "Usage insight trend contains mixed granularities",
        ));
    }

    let requests = &overview.requests;
    let failed_requests = service_failure_count(requests);
    let health_requests = requests.success_count.saturating_add(failed_requests);
    let health = UsageInsightsHealth {
        total_requests: health_requests,
        success_requests: requests.success_count,
        failed_requests,
        cancelled_requests: requests.cancelled_count,
        incomplete_requests: requests.incomplete_count,
        caller_error_requests: requests.caller_error_count,
        success_rate: rate_or_zero(requests.success_count, health_requests),
        points: trend
            .iter()
            .map(|point| {
                let failed_requests = service_failure_count(&point.metrics);
                let health_requests = point.metrics.success_count.saturating_add(failed_requests);
                UsageInsightsHealthPoint {
                    bucket_start: point.bucket_start,
                    success_requests: point.metrics.success_count,
                    failed_requests,
                    cancelled_requests: point.metrics.cancelled_count,
                    incomplete_requests: point.metrics.incomplete_count,
                    caller_error_requests: point.metrics.caller_error_count,
                    error_rate: rate_or_zero(failed_requests, health_requests),
                }
            })
            .collect(),
    };
    let performance = UsageInsightsPerformance {
        latency_percentiles: requests.latency_percentiles,
        first_token_latency_percentiles: requests.first_token_latency_percentiles,
        latency_coverage: rate_or_zero(requests.latency_count, requests.request_count),
        first_token_coverage: rate_or_zero(
            requests.first_token_latency_count,
            requests.request_count,
        ),
        points: trend
            .iter()
            .map(|point| UsageInsightsPerformancePoint {
                bucket_start: point.bucket_start,
                latency_percentiles: point.metrics.latency_percentiles,
                first_token_latency_percentiles: point.metrics.first_token_latency_percentiles,
            })
            .collect(),
    };
    let estimated_cost = overview
        .attempts
        .costs
        .iter()
        .find(|cost| cost.currency.eq_ignore_ascii_case("USD"))
        .map(|cost| cost.amount.clone());
    let cost = UsageInsightsCost {
        estimated_cost,
        standard_cost: None,
        cost_per_request: None,
        tokens_per_request: rate_or_zero(requests.total_tokens, requests.request_count),
        cached_token_rate: rate_or_zero(requests.cached_tokens, requests.input_tokens),
        cache_hit_request_rate: ratio(
            requests.cache_hit_request_count,
            requests.cache_eligible_request_count,
        ),
        input_tokens: requests.input_tokens,
        output_tokens: requests.output_tokens,
        cached_tokens: requests.cached_tokens,
        total_tokens: requests.total_tokens,
        points: trend
            .iter()
            .map(|point| UsageInsightsCostPoint {
                bucket_start: point.bucket_start,
                input_tokens: point.metrics.input_tokens,
                output_tokens: point.metrics.output_tokens,
                cached_tokens: point.metrics.cached_tokens,
                total_tokens: point.metrics.total_tokens,
                estimated_cost: None,
                standard_cost: None,
                cached_token_rate: rate_or_zero(
                    point.metrics.cached_tokens,
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
        granularity,
        health,
        performance,
        cost,
        attempts: overview.attempts,
        providers: overview.providers,
    })
}

impl DefaultObservabilityService {
    fn enrich_billing(
        &self,
        records: &mut [crate::model::observability::UsageRecord],
    ) -> Result<(), AdminError> {
        for record in records {
            let Some(UsageBilling::Total { source, total }) = record.billing.as_ref() else {
                continue;
            };
            if source != "calculated" {
                continue;
            }
            let (Some(provider), Some(upstream_model_id)) = (
                record.provider_kind.as_deref(),
                record.upstream_model_id.as_ref(),
            ) else {
                continue;
            };
            let provider_kind = ProviderKind::new(provider.to_owned())
                .map_err(|_| AdminError::internal("Stored Provider kind is invalid"))?;
            let input = ProviderBillingInput {
                upstream_model_id: upstream_model_id.clone(),
                input_tokens: record.input_tokens,
                output_tokens: record.output_tokens,
                cached_tokens: record.cached_tokens,
                cache_write_tokens: record.cache_write_tokens,
                total: total.clone(),
            };
            if let Some(breakdown) = self
                .providers
                .calculated_billing(&provider_kind, &input)
                .map_err(|error| map_provider_error(error, "usage billing"))?
            {
                record.billing = Some(UsageBilling::Calculated(Box::new(breakdown)));
            }
        }
        Ok(())
    }
}

/// 按指定时刻计算中国自然日的 96 个 15 分钟健康桶。
#[must_use]
fn health_timeline_at(records: &[RequestMetricPoint], now: DateTime<Utc>) -> HealthTimeline {
    let current_slot = quarter_hour_start(now);
    let start = china_day_start(now);
    let mut buckets = (0..HEALTH_TIMELINE_SLOTS)
        .map(|index| {
            (
                start + Duration::minutes(HEALTH_TIMELINE_SLOT_MINUTES * index),
                HealthWindow::default(),
            )
        })
        .collect::<Vec<_>>();
    for record in records {
        if record.bucket_start < start || record.bucket_start > now {
            continue;
        }
        let record_slot = quarter_hour_start(record.bucket_start);
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
        .fold(HealthWindow::default(), |mut totals, (_, bucket)| {
            totals.add_window(*bucket);
            totals
        });
    HealthTimeline {
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
    }
}

fn trend(kind: TrendKind, points: Vec<RequestMetricPoint>) -> Result<Trend, AdminError> {
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
                *amount = amount.checked_add(&cost.amount).ok_or_else(|| {
                    AdminError::internal("Cost aggregation exceeded numeric bounds")
                })?;
            } else {
                costs.insert(cost.currency.clone(), cost.amount.clone());
            }
        }
    }
    let average_latency_ms = average(metrics.latency_sum_ms, metrics.latency_count);
    let service_failure_count = service_failure_count(&metrics);
    let success_rate = ratio(
        metrics.success_count,
        metrics.success_count.saturating_add(service_failure_count),
    );
    let cache_hit_request_rate = ratio(
        metrics.cache_hit_request_count,
        metrics.cache_eligible_request_count,
    );
    Ok(Trend {
        kind,
        points: points.into_iter().map(trend_point).collect(),
        summary: TrendSummary {
            request_count: metrics.request_count,
            input_tokens: metrics.input_tokens,
            output_tokens: metrics.output_tokens,
            cached_tokens: metrics.cached_tokens,
            total_tokens: metrics.total_tokens,
            service_failure_count,
            average_latency_ms,
            max_latency_ms: metrics.max_latency_ms,
            min_latency_ms: metrics.min_latency_ms,
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
    let service_failure_count = service_failure_count(&point.metrics);
    TrendPoint {
        bucket_start: point.bucket_start,
        granularity: point.granularity,
        service_failure_count,
        average_latency_ms: average(point.metrics.latency_sum_ms, point.metrics.latency_count),
        average_first_token_latency_ms: average(
            point.metrics.first_token_latency_sum_ms,
            point.metrics.first_token_latency_count,
        ),
        cached_token_rate: rate_or_zero(point.metrics.cached_tokens, point.metrics.input_tokens),
        cache_hit_request_rate: ratio(
            point.metrics.cache_hit_request_count,
            point.metrics.cache_eligible_request_count,
        ),
        success_rate: ratio(point.metrics.success_count, point.metrics.request_count),
        metrics: point.metrics,
        cost_coverage: point.cost_coverage,
        costs: point.costs,
    }
}

fn dashboard_period_metrics(
    points: &[RequestMetricPoint],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> DashboardPeriodMetrics {
    let mut result = DashboardPeriodMetrics::default();
    for metrics in points
        .iter()
        .filter(|point| point.bucket_start >= start && point.bucket_start < end)
        .map(|point| &point.metrics)
    {
        result.request_count = result.request_count.saturating_add(metrics.request_count);
        result.input_tokens = result.input_tokens.saturating_add(metrics.input_tokens);
        result.output_tokens = result.output_tokens.saturating_add(metrics.output_tokens);
        result.cached_tokens = result.cached_tokens.saturating_add(metrics.cached_tokens);
        result.total_tokens = result.total_tokens.saturating_add(metrics.total_tokens);
    }
    result.cached_token_rate = rate_or_zero(result.cached_tokens, result.input_tokens);
    result.observed_cached_token_rate = ratio(result.cached_tokens, result.input_tokens);
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
    total.cached_tokens = total.cached_tokens.saturating_add(value.cached_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(value.cache_write_tokens);
    total.reasoning_tokens = total
        .reasoning_tokens
        .saturating_add(value.reasoning_tokens);
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

fn average(sum: u64, count: u64) -> Option<u64> {
    (count > 0).then(|| sum / count)
}

fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

fn rate_or_zero(numerator: u64, denominator: u64) -> f64 {
    ratio(numerator, denominator).unwrap_or(0.0)
}

#[derive(Debug, Clone, Copy, Default)]
struct HealthWindow {
    success_requests: u64,
    failed_requests: u64,
    cancelled_requests: u64,
    incomplete_requests: u64,
    caller_error_requests: u64,
}

impl HealthWindow {
    fn add_metrics(&mut self, metrics: &RequestMetrics) {
        self.success_requests = self.success_requests.saturating_add(metrics.success_count);
        self.failed_requests = self
            .failed_requests
            .saturating_add(service_failure_count(metrics));
        self.cancelled_requests = self
            .cancelled_requests
            .saturating_add(metrics.cancelled_count);
        self.incomplete_requests = self
            .incomplete_requests
            .saturating_add(metrics.incomplete_count);
        self.caller_error_requests = self
            .caller_error_requests
            .saturating_add(metrics.caller_error_count);
    }

    fn add_window(&mut self, other: Self) {
        self.success_requests = self.success_requests.saturating_add(other.success_requests);
        self.failed_requests = self.failed_requests.saturating_add(other.failed_requests);
        self.cancelled_requests = self
            .cancelled_requests
            .saturating_add(other.cancelled_requests);
        self.incomplete_requests = self
            .incomplete_requests
            .saturating_add(other.incomplete_requests);
        self.caller_error_requests = self
            .caller_error_requests
            .saturating_add(other.caller_error_requests);
    }
}

fn health_status(bucket: HealthWindow, is_future: bool) -> HealthStatus {
    let eligible_requests = bucket
        .success_requests
        .saturating_add(bucket.failed_requests);
    if is_future {
        HealthStatus::Future
    } else if eligible_requests == 0 {
        HealthStatus::NoData
    } else if bucket.success_requests == 0
        && bucket.failed_requests >= HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD
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

fn health_reliability(bucket: HealthWindow) -> Option<f64> {
    let eligible_requests = bucket
        .success_requests
        .saturating_add(bucket.failed_requests);
    (eligible_requests > 0)
        .then(|| bucket.success_requests as f64 / eligible_requests as f64 * 100.0)
}

fn service_failure_count(metrics: &RequestMetrics) -> u64 {
    metrics
        .failure_count
        .saturating_sub(metrics.caller_error_count)
}

fn china_day_start(value: DateTime<Utc>) -> DateTime<Utc> {
    let elapsed = (value.timestamp() + CHINA_OFFSET_SECONDS).rem_euclid(24 * 60 * 60);
    value - Duration::seconds(elapsed) - Duration::nanoseconds(i64::from(value.nanosecond()))
}

fn quarter_hour_start(value: DateTime<Utc>) -> DateTime<Utc> {
    let elapsed = value
        .timestamp()
        .rem_euclid(HEALTH_TIMELINE_SLOT_MINUTES * 60);
    value - Duration::seconds(elapsed) - Duration::nanoseconds(i64::from(value.nanosecond()))
}
