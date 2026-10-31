//! 观测查询、趋势、费用覆盖与健康阈值规则

use std::{
    collections::{BTreeMap, HashMap, btree_map::Entry},
    str::FromStr,
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use gateway_core::{account::ProviderAccountId, routing::ProviderKind};

use crate::{
    model::{
        AdminError,
        observability::{
            CostCoverage, CurrencyCost, DashboardAccountUsage, DashboardCapacity,
            DashboardPeriodMetrics, DashboardQuery, DashboardResult, DecimalAmount,
            DiagnosticDimension, DiagnosticsItem, DiagnosticsResult, Granularity, HealthStatus,
            HealthTimeline, HealthTimelinePoint, OpsErrorPage, OpsErrorQuery, ProviderBillingInput,
            RequestMetricPoint, RequestMetrics, TimeRange, Trend, TrendKind, TrendPoint,
            TrendSummary, UsageBilling, UsageBillingProjection, UsageCacheEfficiency,
            UsageCalculatedBillingFact, UsageConcurrencyPoint, UsageCostComparison,
            UsageCostSummary, UsageDetail, UsageFilter, UsageInsight, UsageInsightData,
            UsageInsightMetric, UsageInsights, UsageInsightsCost, UsageInsightsCostPoint,
            UsageInsightsHealth, UsageInsightsHealthPoint, UsageInsightsLoad,
            UsageInsightsObservation, UsageInsightsPerformance, UsageInsightsPerformancePoint,
            UsageMetricPoint, UsageMetricSeries, UsageOverview, UsagePage, UsageQuery,
            UsageSummary, UsageThroughputPercentiles,
        },
        provider_credentials::ProviderQuotaRequest,
    },
    ports::{
        provider::{ProviderAdminErrorKind, ProviderAdminRegistry},
        store::{AccountStore, ObservabilityStore, SettingsStore, UsageBillingProjector},
    },
};

use super::{map_provider_error, map_store_error};

const HEALTH_TIMELINE_SLOT_MINUTES: i64 = 15;
const HEALTH_TIMELINE_MIN_SAMPLE_SIZE: u64 = 10;
const HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD: u64 = 3;
const HEALTH_TIMELINE_STABLE_RELIABILITY: f64 = 95.0;
mod export;
mod store_cache;
pub(crate) use store_cache::CachedObservabilityStore;

/// API 消费的观测控制面服务
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
    async fn usage_metric(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        metric: UsageInsightMetric,
    ) -> Result<UsageInsight, AdminError>;
    async fn diagnostics(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        dimension: DiagnosticDimension,
    ) -> Result<DiagnosticsResult, AdminError>;
    async fn ops_errors(&self, query: OpsErrorQuery) -> Result<OpsErrorPage, AdminError>;
    async fn export_diagnostics(
        &self,
        target: crate::model::diagnostics::DiagnosticTarget,
    ) -> Result<crate::model::diagnostics::DiagnosticBundle, AdminError>;
}

pub(crate) struct DefaultObservabilityService {
    timezone: gateway_core::time::DeploymentTimeZone,
    store: Arc<dyn ObservabilityStore>,
    accounts: Arc<dyn AccountStore>,
    settings: Arc<dyn SettingsStore>,
    providers: ProviderAdminRegistry,
    system: Arc<dyn super::system::SystemService>,
    upstream_info: Option<Arc<dyn crate::ports::peer::PeerUpstreamService>>,
}

impl DefaultObservabilityService {
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn ObservabilityStore>,
        accounts: Arc<dyn AccountStore>,
        settings: Arc<dyn SettingsStore>,
        providers: ProviderAdminRegistry,
        timezone: gateway_core::time::DeploymentTimeZone,
        system: Arc<dyn super::system::SystemService>,
        upstream_info: Option<Arc<dyn crate::ports::peer::PeerUpstreamService>>,
    ) -> Self {
        Self {
            timezone,
            store,
            accounts,
            settings,
            providers,
            system,
            upstream_info,
        }
    }

    async fn load_dashboard_summary(
        &self,
        range: TimeRange,
    ) -> Result<DashboardResult, AdminError> {
        let observed_at = Utc::now();
        let query = DashboardQuery::new(range, self.timezone)
            .map_err(|_| AdminError::internal("时间超出支持范围"))?;
        let today_start = query.yesterday_range.end;
        let (mut observation, settings) = futures::try_join!(
            self.store.dashboard_summary(query, observed_at),
            self.settings.load_runtime_settings(),
        )
        .map_err(|error| map_store_error(error, "dashboard"))?;
        let runtime_slots = observation.runtime_slots;
        self.enrich_list_records(&mut observation.recent_requests);
        self.enrich_dashboard_quotas(&mut observation.account_usage)
            .await;
        for account in &mut observation.account_usage {
            account.plan_type_display = self
                .providers
                .plan_type_display(&account.provider_kind, account.plan_type.as_deref());
        }
        let today = dashboard_period_metrics(
            observation
                .trend
                .iter()
                .filter(|point| {
                    point.bucket_start >= today_start && point.bucket_start < observation.range.end
                })
                .map(|point| &point.metrics),
        );
        let yesterday = dashboard_period_metrics(std::iter::once(&observation.yesterday));
        let total_billing_usd = observation.totals.billing_usd.clone();
        let total_cached_token_rate = rate_or_zero(
            observation.totals.cached_input_tokens,
            observation.totals.input_tokens,
        );
        let (first_token_latency_sum_ms, first_token_latency_count) = observation
            .trend
            .iter()
            .fold((0_u64, 0_u64), |(sum, count), point| {
                (
                    sum.saturating_add(point.metrics.first_token_latency_sum_ms),
                    count.saturating_add(point.metrics.first_token_latency_count),
                )
            });
        let average_first_token_latency_ms =
            average(first_token_latency_sum_ms, first_token_latency_count);
        let trend = trend(
            TrendKind::Usage,
            observation.trend.clone(),
            observation.analytics.clone(),
        )?;
        let health_timeline =
            health_timeline_at(&observation.trend, observation.range.end, self.timezone)?;
        let wire_profiles = self
            .providers
            .dashboard_wire_profiles(&settings.request_profiles);
        let max_concurrent_per_account = u64::from(settings.values.max_concurrent_per_account);
        let (inherited_accounts, overridden_slots) = runtime_slots
            .as_ref()
            .map_or((observation.provider_accounts.normal, 0), |slots| {
                (slots.inherited_accounts, slots.overridden_slots)
            });
        let total_slots = (max_concurrent_per_account > 0 || inherited_accounts == 0).then(|| {
            inherited_accounts
                .saturating_mul(max_concurrent_per_account)
                .saturating_add(overridden_slots)
        });
        let used_slots = runtime_slots.and_then(|slots| slots.used_slots);
        Ok(DashboardResult {
            capacity: DashboardCapacity {
                max_concurrent_per_account,
                total_slots,
                used_slots,
                available_slots: used_slots
                    .zip(total_slots)
                    .map(|(used, total)| total.saturating_sub(used)),
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
}

#[async_trait]
impl ObservabilityService for DefaultObservabilityService {
    async fn export_diagnostics(
        &self,
        target: crate::model::diagnostics::DiagnosticTarget,
    ) -> Result<crate::model::diagnostics::DiagnosticBundle, AdminError> {
        self.collect_diagnostics(target).await
    }

    async fn dashboard_summary(
        &self,
        range: TimeRange,
        kind: TrendKind,
    ) -> Result<DashboardResult, AdminError> {
        let mut result = self.load_dashboard_summary(range).await?;
        result.trend = trend(
            kind,
            result.observation.trend.clone(),
            result.observation.analytics.clone(),
        )?;
        Ok(result)
    }

    async fn dashboard_trend(
        &self,
        range: TimeRange,
        kind: TrendKind,
    ) -> Result<Trend, AdminError> {
        let observation = self
            .store
            .usage_insights_observation(
                range,
                UsageFilter::default(),
                Granularity::for_range(range),
                false,
            )
            .await
            .map_err(|error| map_store_error(error, "dashboard trend"))?;
        trend(kind, observation.trend, observation.overview.analytics)
    }

    async fn usage_records(&self, query: UsageQuery) -> Result<UsagePage, AdminError> {
        let mut page = self
            .store
            .list_usage_records(query)
            .await
            .map_err(|error| map_store_error(error, "usage records"))?;
        self.enrich_list_records(&mut page.items);
        Ok(page)
    }

    async fn usage_record_detail(&self, request_id: &str) -> Result<UsageDetail, AdminError> {
        if request_id.trim().is_empty() {
            return Err(AdminError::invalid("用量记录 ID 不能为空"));
        }
        let mut detail = self
            .store
            .usage_record_detail(request_id)
            .await
            .map_err(|error| map_store_error(error, "usage record"))?;
        self.enrich_detail_billing(std::slice::from_mut(&mut detail.request));
        Ok(detail)
    }

    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageSummary, AdminError> {
        let observation = self
            .store
            .usage_summary(range, filter)
            .await
            .map_err(|error| map_store_error(error, "usage summary"))?;
        let average_latency_ms = average(
            observation.requests.latency_sum_ms,
            observation.requests.latency_count,
        );
        Ok(UsageSummary {
            observation,
            average_latency_ms,
        })
    }

    async fn usage_insights(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> Result<UsageInsights, AdminError> {
        let granularity = Granularity::for_range(range);
        let observation = self
            .store
            .usage_insights_observation(range, filter, granularity, true)
            .await
            .map_err(|error| map_store_error(error, "usage insights"))?;
        insights_from_observation(observation)
    }

    async fn usage_metric(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        metric: UsageInsightMetric,
    ) -> Result<UsageInsight, AdminError> {
        let observation = self
            .store
            .usage_metric_observation(range, filter, Granularity::for_range(range), metric)
            .await
            .map_err(|error| map_store_error(error, "usage metric"))?;
        let insights = insights_from_observation(observation)?;
        Ok(select_usage_metric(insights, metric))
    }

    async fn diagnostics(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        dimension: DiagnosticDimension,
    ) -> Result<DiagnosticsResult, AdminError> {
        let observation = self
            .store
            .usage_diagnostics(range, filter, dimension, 100)
            .await
            .map_err(|error| map_store_error(error, "usage diagnostics"))?;
        let items = observation
            .items
            .into_iter()
            .map(|item| {
                let error_rate = rate_or_zero(item.failure_count, item.request_count);
                let non_completion_rate =
                    rate_or_zero(item.non_completion_count, item.request_count);
                let retry_rate = rate_or_zero(item.retried_request_count, item.request_count);
                DiagnosticsItem {
                    key: item.key,
                    name: item.name,
                    account_authentication_kind: item.account_authentication_kind,
                    account_plan_type_display: item.account_provider_kind.as_deref().and_then(
                        |provider| {
                            self.providers
                                .plan_type_display(provider, item.account_plan_type.as_deref())
                        },
                    ),
                    account_plan_type: item.account_plan_type,
                    request_count: item.request_count,
                    success_count: item.success_count,
                    error_count: item.failure_count,
                    error_rate,
                    request_share: rate_or_zero(
                        item.request_count,
                        observation.total_request_count,
                    ),
                    average_latency_ms: item.average_latency_ms,
                    latency_p95_ms: item.latency_p95_ms,
                    first_token_p95_ms: item.first_token_p95_ms,
                    non_completion_count: item.non_completion_count,
                    non_completion_rate,
                    retry_count: item.retry_count,
                    retry_rate,
                    estimated_cost: usd_cost(&item.costs),
                    attempt_count: item.attempt_count,
                    total_tokens: item.total_tokens,
                }
            })
            .collect::<Vec<_>>();
        Ok(DiagnosticsResult {
            analytics: observation.analytics,
            dimension,
            items,
        })
    }

    async fn ops_errors(&self, query: OpsErrorQuery) -> Result<OpsErrorPage, AdminError> {
        let mut page = self
            .store
            .list_ops_errors(query)
            .await
            .map_err(|error| map_store_error(error, "operations errors"))?;
        for error in &mut page.items {
            error.provider_account_plan_type_display =
                error.provider_kind.as_deref().and_then(|provider| {
                    self.providers
                        .plan_type_display(provider, error.provider_account_plan_type.as_deref())
                });
        }
        Ok(page)
    }
}

fn insights_from_observation(
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

fn select_usage_metric(insights: UsageInsights, metric: UsageInsightMetric) -> UsageInsight {
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
    let health_window = HealthWindow::from_metrics(requests);
    let failed_requests = health_window.failed_requests;
    let health_requests = health_window.eligible_requests();
    let health = UsageInsightsHealth {
        total_requests: requests.request_count,
        success_requests: requests.success_count,
        failed_requests,
        cancelled_requests: requests.cancelled_count,
        incomplete_requests: requests.incomplete_count,
        caller_error_requests: requests.caller_error_count,
        success_rate: rate_or_zero(requests.success_count, health_requests),
        completion_rate: rate_or_zero(
            requests
                .success_count
                .saturating_add(requests.failure_count),
            requests.request_count,
        ),
        points: trend
            .iter()
            .map(|point| {
                let health_window = HealthWindow::from_metrics(&point.metrics);
                UsageInsightsHealthPoint {
                    total_requests: point.metrics.request_count,
                    bucket_start: point.bucket_start,
                    success_requests: point.metrics.success_count,
                    failed_requests: health_window.failed_requests,
                    cancelled_requests: point.metrics.cancelled_count,
                    incomplete_requests: point.metrics.incomplete_count,
                    caller_error_requests: point.metrics.caller_error_count,
                    error_rate: rate_or_zero(
                        health_window.non_success_requests(),
                        health_window.eligible_requests(),
                    ),
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

/// 在后台投影时恢复费用场景，查询不再逐请求读取计费快照
pub struct ProviderUsageBillingProjector {
    providers: ProviderAdminRegistry,
}

impl ProviderUsageBillingProjector {
    #[must_use]
    pub fn new(providers: ProviderAdminRegistry) -> Self {
        Self { providers }
    }
}

impl UsageBillingProjector for ProviderUsageBillingProjector {
    fn project(
        &self,
        fact: &UsageCalculatedBillingFact,
    ) -> Result<UsageBillingProjection, AdminError> {
        let Ok(provider_kind) = ProviderKind::new(fact.provider_kind.clone()) else {
            return Ok(UsageBillingProjection::default());
        };
        let input = ProviderBillingInput {
            upstream_model_id: fact.upstream_model_id.clone(),
            service_tier: fact.service_tier.clone(),
            input_tokens: fact.input_tokens,
            output_tokens: fact.output_tokens,
            cached_input_tokens: fact.cached_input_tokens,
            cache_write_input_tokens: fact.cache_write_input_tokens,
            total: fact.total.clone(),
        };
        let breakdown = match fact
            .breakdown
            .as_ref()
            .filter(|breakdown| breakdown.total_amount == input.total)
            .cloned()
            .map_or_else(
                || self.providers.calculated_billing(&provider_kind, &input),
                |breakdown| Ok(Some(breakdown)),
            ) {
            Ok(breakdown) => breakdown,
            Err(error) if error.kind() == ProviderAdminErrorKind::Unsupported => {
                return Ok(UsageBillingProjection::default());
            }
            Err(error) => return Err(map_provider_error(error, "usage billing")),
        };
        let Some(breakdown) = breakdown else {
            return Ok(UsageBillingProjection::default());
        };
        if !breakdown
            .standard_amount
            .currency
            .eq_ignore_ascii_case("USD")
        {
            return Ok(UsageBillingProjection::default());
        }
        Ok(UsageBillingProjection {
            standard_amount: Some(breakdown.standard_amount.amount.clone()),
            no_cache_amount: no_cache_cost(fact, &breakdown),
        })
    }
}

fn no_cache_cost(
    fact: &UsageCalculatedBillingFact,
    breakdown: &crate::model::observability::CalculatedBillingBreakdown,
) -> Option<DecimalAmount> {
    let image = breakdown.image.as_ref();
    let input_tokens = fact
        .input_tokens?
        .checked_sub(image.map_or(0, |i| i.input_tokens))?;
    let cached_input_tokens = fact
        .cached_input_tokens
        .unwrap_or_default()
        .checked_sub(image.map_or(0, |i| i.cached_input_tokens))?
        .min(input_tokens);
    let cache_write_input_tokens = fact
        .cache_write_input_tokens
        .unwrap_or_default()
        .min(input_tokens.saturating_sub(cached_input_tokens));
    let input_rate = decimal(&breakdown.input_price_per_million.amount)?;
    let replaced_input_amount = input_rate
        .scaled()
        .checked_mul(u128::from(
            cached_input_tokens.saturating_add(cache_write_input_tokens),
        ))?
        .checked_div(1_000_000)?;
    let mut total = decimal(&breakdown.total_amount.amount)?
        .scaled()
        .checked_sub(decimal(&breakdown.cache_read_amount.amount)?.scaled())?
        .checked_sub(decimal(&breakdown.cache_write_amount.amount)?.scaled())?
        .checked_add(replaced_input_amount)?;
    if let Some(image) = image {
        let image_no_cache = decimal(&image.input_price_per_million.amount)?
            .scaled()
            .checked_mul(u128::from(image.cached_input_tokens))?
            .checked_div(1_000_000)?;
        total = total
            .checked_sub(decimal(&image.cache_read_amount.amount)?.scaled())?
            .checked_add(image_no_cache)?;
    }
    DecimalAmount::from_str(
        &gateway_core::metering::Decimal::from_scaled(total)
            .ok()?
            .canonical(),
    )
    .ok()
}

fn amount_difference(
    larger: Option<&DecimalAmount>,
    smaller: Option<&DecimalAmount>,
) -> Option<DecimalAmount> {
    let difference = decimal(larger?)?
        .scaled()
        .checked_sub(decimal(smaller?)?.scaled())?;
    DecimalAmount::from_str(
        &gateway_core::metering::Decimal::from_scaled(difference)
            .ok()?
            .canonical(),
    )
    .ok()
}

fn decimal(value: &DecimalAmount) -> Option<gateway_core::metering::Decimal> {
    value.as_str().parse().ok()
}

fn usd_cost(costs: &[CurrencyCost]) -> Option<DecimalAmount> {
    costs
        .iter()
        .find(|cost| cost.currency.eq_ignore_ascii_case("USD"))
        .map(|cost| cost.amount.clone())
}

impl DefaultObservabilityService {
    async fn enrich_dashboard_quotas(&self, accounts: &mut [DashboardAccountUsage]) {
        if accounts.is_empty() {
            return;
        }
        let now = Utc::now();
        let ids = accounts
            .iter()
            .map(|account| account.account_id.clone())
            .collect::<Vec<_>>();
        let mut rolling_usage = self
            .accounts
            .load_account_usage(
                TimeRange {
                    start: now - Duration::hours(24),
                    end: now,
                },
                &ids,
            )
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|usage| (usage.account_id.clone(), usage))
            .collect::<HashMap<_, _>>();
        let providers = self.providers.clone();
        let peer = self.upstream_info.as_deref();
        let quota_reads = accounts.iter().map(|account| {
            let rolling_usage = rolling_usage.remove(&account.account_id);
            let account_id = ProviderAccountId::new(account.account_id.clone()).ok();
            let provider = ProviderKind::new(account.provider_kind.clone())
                .ok()
                .and_then(|kind| providers.require(&kind).ok());
            async move {
                let (Some(account_id), Some(provider)) = (account_id, provider) else {
                    return None;
                };
                if let Some(peer) = peer
                    && let Ok(Some(quota)) = peer.read_quota(&account_id).await
                {
                    return Some(quota);
                }
                provider
                    .quota(ProviderQuotaRequest {
                        account_id,
                        refresh: false,
                        rolling_usage,
                    })
                    .await
                    .ok()
            }
        });
        let quotas = futures::future::join_all(quota_reads).await;
        for (account, quota) in accounts.iter_mut().zip(quotas) {
            if let Some(mut quota) = quota {
                quota.fill_missing_plan_type(&mut account.plan_type);
                quota.apply_limit_reached_display();
                account.quota_used_percent = quota.representative_used_percent();
                account.quota_window = quota.representative_window().cloned();
                account.upstream = quota.upstream;
            }
        }
    }

    /// 补充当前套餐展示名称，逐条尽力把可校验的总额升级为完整分解；单条脏数据（非法 Provider kind、
    /// 不支持的来源或费用规则失败）只保留该条已存的总额，不影响整页返回
    fn enrich_list_records(&self, records: &mut [crate::model::observability::UsageListRecord]) {
        for record in records {
            record.provider_account_plan_type_display =
                record.provider_kind.as_deref().and_then(|provider| {
                    self.providers
                        .plan_type_display(provider, record.provider_account_plan_type.as_deref())
                });
            let Some(UsageBilling::Total { source, total }) = record.billing.as_ref() else {
                continue;
            };
            if !matches!(source.as_str(), "calculated" | "provider_reported") {
                continue;
            }
            let (Some(provider), Some(upstream_model_id)) = (
                record.provider_kind.as_deref(),
                record.upstream_model_id.as_ref(),
            ) else {
                continue;
            };
            let Ok(provider_kind) = ProviderKind::new(provider.to_owned()) else {
                continue;
            };
            let input = ProviderBillingInput {
                upstream_model_id: upstream_model_id.clone(),
                service_tier: record.service_tier.clone(),
                input_tokens: record.input_tokens,
                output_tokens: record.output_tokens,
                cached_input_tokens: record.cached_input_tokens,
                cache_write_input_tokens: record.cache_write_input_tokens,
                total: total.clone(),
            };
            if let Ok(Some(breakdown)) = self.providers.calculated_billing(&provider_kind, &input) {
                record.billing = Some(UsageBilling::Calculated(Box::new(breakdown)));
            }
        }
    }

    fn enrich_detail_billing(&self, records: &mut [crate::model::observability::UsageRecord]) {
        for record in records {
            let Some(UsageBilling::Total { source, total }) = record.billing.as_ref() else {
                continue;
            };
            if !matches!(source.as_str(), "calculated" | "provider_reported") {
                continue;
            }
            let (Some(provider), Some(upstream_model_id)) = (
                record.provider_kind.as_deref(),
                record.upstream_model_id.as_ref(),
            ) else {
                continue;
            };
            let Ok(provider_kind) = ProviderKind::new(provider.to_owned()) else {
                continue;
            };
            let input = ProviderBillingInput {
                upstream_model_id: upstream_model_id.clone(),
                service_tier: record.service_tier.clone(),
                input_tokens: record.input_tokens,
                output_tokens: record.output_tokens,
                cached_input_tokens: record.cached_input_tokens,
                cache_write_input_tokens: record.cache_write_input_tokens,
                total: total.clone(),
            };
            if let Ok(Some(breakdown)) = self.providers.calculated_billing(&provider_kind, &input) {
                record.billing = Some(UsageBilling::Calculated(Box::new(breakdown)));
            }
        }
    }
}

/// 按部署时区的自然日计算 15 分钟健康桶，桶数随夏令时变化
pub(super) fn health_timeline_at(
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
                HealthWindow::default(),
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
        .fold(HealthWindow::default(), |mut totals, (_, bucket)| {
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

fn trend(
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
    let service_failure_count = service_failure_count(&metrics);
    let success_rate = ratio(
        metrics.success_count,
        HealthWindow::from_metrics(&metrics).eligible_requests(),
    );
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
            service_failure_count,
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
        cached_token_rate: rate_or_zero(
            point.metrics.cached_input_tokens,
            point.metrics.input_tokens,
        ),
        cache_hit_request_rate: ratio(
            point.metrics.cache_hit_request_count,
            point.metrics.cache_eligible_request_count,
        ),
        success_rate: ratio(
            point.metrics.success_count,
            HealthWindow::from_metrics(&point.metrics).eligible_requests(),
        ),
        metrics: point.metrics,
        cost_coverage: point.cost_coverage,
        costs: point.costs,
    }
}

fn dashboard_period_metrics<'a>(
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
    fn from_metrics(metrics: &RequestMetrics) -> Self {
        Self {
            success_requests: metrics.success_count,
            failed_requests: service_failure_count(metrics),
            cancelled_requests: metrics.cancelled_count,
            incomplete_requests: metrics.incomplete_count,
            caller_error_requests: metrics.caller_error_count,
        }
    }

    // 未完成保留交付终态，但上游失败、超时和进程中断都不能视为服务成功
    fn non_success_requests(self) -> u64 {
        self.failed_requests
            .saturating_add(self.incomplete_requests)
    }

    fn eligible_requests(self) -> u64 {
        self.success_requests
            .saturating_add(self.non_success_requests())
    }

    fn add_metrics(&mut self, metrics: &RequestMetrics) {
        self.add_window(Self::from_metrics(metrics));
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

fn health_reliability(bucket: HealthWindow) -> Option<f64> {
    let eligible_requests = bucket.eligible_requests();
    (eligible_requests > 0)
        .then(|| bucket.success_requests as f64 / eligible_requests as f64 * 100.0)
}

fn service_failure_count(metrics: &RequestMetrics) -> u64 {
    metrics
        .failure_count
        .saturating_sub(metrics.caller_error_count)
}
