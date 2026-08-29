//! 观测查询、趋势、费用覆盖与健康阈值规则。

use std::{
    collections::{BTreeMap, btree_map::Entry},
    str::FromStr,
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Timelike as _, Utc};
use gateway_core::{engine::credential::ProviderAccountId, routing::ProviderKind};

use crate::{
    model::{
        AdminError,
        observability::{
            CostCoverage, CurrencyCost, DashboardAccountUsage, DashboardCapacity,
            DashboardPeriodMetrics, DashboardResult, DecimalAmount, DiagnosticDimension,
            DiagnosticsItem, DiagnosticsResult, HealthStatus, HealthTimeline, HealthTimelinePoint,
            OpsErrorPage, OpsErrorQuery, ProviderBillingInput, RequestMetricPoint, RequestMetrics,
            TimeRange, Trend, TrendKind, TrendPoint, TrendSummary, UsageBilling,
            UsageCalculatedBillingFact, UsageDetail, UsageFilter, UsageInsights, UsageInsightsCost,
            UsageInsightsCostPoint, UsageInsightsHealth, UsageInsightsHealthPoint,
            UsageInsightsPerformance, UsageInsightsPerformancePoint, UsageOverview, UsagePage,
            UsageQuery, UsageSummary, china_day_start,
        },
        provider_credentials::ProviderQuotaRequest,
    },
    ports::{
        provider::{ProviderAdminErrorKind, ProviderAdminRegistry},
        store::{ObservabilityStore, SettingsStore},
    },
};

use super::{map_provider_error, map_store_error};

const HEALTH_TIMELINE_SLOT_MINUTES: i64 = 15;
const HEALTH_TIMELINE_SLOTS: i64 = 24 * 4;
const HEALTH_TIMELINE_MIN_SAMPLE_SIZE: u64 = 10;
const HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD: u64 = 3;
const HEALTH_TIMELINE_STABLE_RELIABILITY: f64 = 99.0;

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
        let observed_at = Utc::now();
        let (mut observation, settings, runtime_slots) = futures::try_join!(
            self.store.dashboard_summary(range, observed_at),
            self.settings.load_runtime_settings(),
            self.store.dashboard_runtime_slots(observed_at),
        )
        .map_err(|error| map_store_error(error, "dashboard"))?;
        self.enrich_billing(&mut observation.recent_requests);
        self.enrich_dashboard_quotas(&mut observation.account_usage)
            .await;
        let today_start = china_day_start(observation.range.end);
        let yesterday_start = today_start - Duration::days(1);
        let today =
            dashboard_period_metrics(&observation.trend, today_start, observation.range.end);
        let yesterday = dashboard_period_metrics(&observation.trend, yesterday_start, today_start);
        let total_billing_usd = observation.totals.billing_usd.clone();
        let total_cached_token_rate = rate_or_zero(
            observation.totals.cached_tokens,
            observation.totals.input_tokens,
        );
        let average_first_token_latency_ms = average(
            observation.requests.first_token_latency_sum_ms,
            observation.requests.first_token_latency_count,
        );
        let trend = trend(kind, observation.trend.clone())?;
        let health_timeline = health_timeline_at(&observation.trend, Utc::now());
        let wire_profiles = self.providers.dashboard_wire_profiles();
        let max_concurrent_per_account = u64::from(settings.max_concurrent_per_account);
        let total_slots = runtime_slots.as_ref().map_or_else(
            || {
                observation
                    .provider_accounts
                    .normal
                    .saturating_mul(max_concurrent_per_account)
            },
            |slots| {
                slots
                    .inherited_accounts
                    .saturating_mul(max_concurrent_per_account)
                    .saturating_add(slots.overridden_slots)
            },
        );
        let used_slots = runtime_slots.and_then(|slots| slots.used_slots);
        Ok(DashboardResult {
            capacity: DashboardCapacity {
                max_concurrent_per_account,
                total_slots,
                used_slots,
                available_slots: used_slots.map(|used| total_slots.saturating_sub(used)),
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
        self.enrich_billing(&mut page.items);
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
        self.enrich_billing(std::slice::from_mut(&mut detail.request));
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
        let (overview, trend, billing_facts) = futures::try_join!(
            self.store.usage_summary(range, filter.clone()),
            self.store.usage_trend(range, filter.clone()),
            self.store.usage_calculated_billing_facts(range, filter),
        )
        .map_err(|error| map_store_error(error, "usage insights"))?;
        let standard_costs = recover_standard_costs(&self.providers, billing_facts)?;
        build_usage_insights(overview, trend, standard_costs)
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
        let mut items = items
            .into_iter()
            .map(|item| {
                let error_rate = rate_or_zero(item.failure_count, item.request_count);
                let non_completion_rate =
                    rate_or_zero(item.non_completion_count, item.request_count);
                let retry_rate = rate_or_zero(item.retry_count, item.request_count);
                let impact_score = diagnostic_impact_score(
                    item.request_count,
                    total_requests,
                    error_rate,
                    non_completion_rate,
                    retry_rate,
                    item.first_token_p95_ms,
                );
                DiagnosticsItem {
                    key: item.key,
                    name: item.name,
                    request_count: item.request_count,
                    success_count: item.success_count,
                    error_count: item.failure_count,
                    error_rate,
                    request_share: rate_or_zero(item.request_count, total_requests),
                    average_latency_ms: item.average_latency_ms,
                    latency_p95_ms: item.latency_p95_ms,
                    first_token_p95_ms: item.first_token_p95_ms,
                    non_completion_count: item.non_completion_count,
                    non_completion_rate,
                    retry_count: item.retry_count,
                    retry_rate,
                    impact_score,
                    estimated_cost: usd_cost(&item.costs),
                    attempt_count: item.attempt_count,
                    total_tokens: item.total_tokens,
                }
            })
            .collect::<Vec<_>>();
        items.sort_by(|left, right| {
            right
                .impact_score
                .total_cmp(&left.impact_score)
                .then_with(|| right.request_count.cmp(&left.request_count))
        });
        Ok(DiagnosticsResult { dimension, items })
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
    standard_costs: UsageCostScenarios,
) -> Result<UsageInsights, AdminError> {
    let granularity = trend.first().map_or(
        crate::model::observability::Granularity::FifteenMinutes,
        |point| point.granularity,
    );
    if trend.iter().any(|point| point.granularity != granularity) {
        return Err(AdminError::internal("用量洞察趋势包含不一致的时间粒度"));
    }

    let requests = &overview.requests;
    let failed_requests = service_failure_count(requests);
    let health_requests = requests.success_count.saturating_add(failed_requests);
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
                let failed_requests = service_failure_count(&point.metrics);
                let health_requests = point.metrics.success_count.saturating_add(failed_requests);
                UsageInsightsHealthPoint {
                    total_requests: point.metrics.request_count,
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
        admission_decision_percentiles: requests.admission_decision_percentiles,
        account_selection_wait_percentiles: requests.account_selection_wait_percentiles,
        admission_decision_coverage: rate_or_zero(
            requests.admission_decision_count,
            requests.request_count,
        ),
        account_selection_wait_coverage: rate_or_zero(
            requests.account_selection_wait_count,
            requests.request_count,
        ),
        output_throughput_p10: requests.output_throughput_p10,
        output_throughput_p50: requests.output_throughput_p50,
        output_throughput_p90: requests.output_throughput_p90,
        capacity_utilization_avg_basis_points: requests.capacity_utilization_avg_basis_points,
        capacity_utilization_p95_basis_points: requests.capacity_utilization_p95_basis_points,
        capacity_coverage: rate_or_zero(requests.capacity_sample_count, requests.request_count),
        points: trend
            .iter()
            .map(|point| UsageInsightsPerformancePoint {
                bucket_start: point.bucket_start,
                latency_percentiles: point.metrics.latency_percentiles,
                first_token_latency_percentiles: point.metrics.first_token_latency_percentiles,
                admission_decision_percentiles: point.metrics.admission_decision_percentiles,
                account_selection_wait_percentiles: point
                    .metrics
                    .account_selection_wait_percentiles,
                output_throughput_p10: point.metrics.output_throughput_p10,
                output_throughput_p50: point.metrics.output_throughput_p50,
                output_throughput_p90: point.metrics.output_throughput_p90,
                capacity_utilization_avg_basis_points: point
                    .metrics
                    .capacity_utilization_avg_basis_points,
                capacity_utilization_p95_basis_points: point
                    .metrics
                    .capacity_utilization_p95_basis_points,
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
                estimated_cost: usd_cost(&point.costs),
                standard_cost: standard_costs_by_bucket.get(&point.bucket_start).cloned(),
                no_cache_cost: no_cache_costs_by_bucket.get(&point.bucket_start).cloned(),
                cache_savings: amount_difference(
                    no_cache_costs_by_bucket.get(&point.bucket_start),
                    usd_cost(&point.costs).as_ref(),
                ),
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

fn recover_standard_costs(
    providers: &ProviderAdminRegistry,
    facts: Vec<UsageCalculatedBillingFact>,
) -> Result<UsageCostScenarios, AdminError> {
    let mut scenarios = UsageCostScenarios::default();
    for fact in facts {
        let Ok(provider_kind) = ProviderKind::new(fact.provider_kind.clone()) else {
            continue;
        };
        let input = ProviderBillingInput {
            upstream_model_id: fact.upstream_model_id.clone(),
            service_tier: fact.service_tier.clone(),
            input_tokens: fact.input_tokens,
            output_tokens: fact.output_tokens,
            cached_tokens: fact.cached_tokens,
            cache_write_tokens: fact.cache_write_tokens,
            total: fact.total.clone(),
        };
        let breakdown = match providers.calculated_billing(&provider_kind, &input) {
            Ok(breakdown) => breakdown,
            Err(error) if error.kind() == ProviderAdminErrorKind::Unsupported => continue,
            Err(error) => return Err(map_provider_error(error, "usage billing")),
        };
        let Some(breakdown) = breakdown else {
            continue;
        };
        if !breakdown
            .standard_amount
            .currency
            .eq_ignore_ascii_case("USD")
        {
            continue;
        }
        scenarios
            .standard
            .add(fact.bucket_start, breakdown.standard_amount.amount.clone())?;
        if let Some(no_cache_cost) = no_cache_cost(&fact, &breakdown) {
            scenarios.no_cache.add(fact.bucket_start, no_cache_cost)?;
        }
    }
    Ok(scenarios)
}

fn no_cache_cost(
    fact: &UsageCalculatedBillingFact,
    breakdown: &crate::model::observability::CalculatedBillingBreakdown,
) -> Option<DecimalAmount> {
    let input_tokens = fact.input_tokens?;
    let cached_tokens = fact.cached_tokens.unwrap_or_default().min(input_tokens);
    let cache_write_tokens = fact
        .cache_write_tokens
        .unwrap_or_default()
        .min(input_tokens.saturating_sub(cached_tokens));
    let input_rate = decimal(&breakdown.input_price_per_million.amount)?;
    let replaced_input_amount = input_rate
        .scaled()
        .checked_mul(u128::from(cached_tokens.saturating_add(cache_write_tokens)))?
        .checked_div(1_000_000)?;
    let total = decimal(&breakdown.total_amount.amount)?
        .scaled()
        .checked_sub(decimal(&breakdown.cache_read_amount.amount)?.scaled())?
        .checked_sub(decimal(&breakdown.cache_write_amount.amount)?.scaled())?
        .checked_add(replaced_input_amount)?;
    DecimalAmount::from_str(
        &gateway_core::accounting::Decimal::from_scaled(total)
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
        &gateway_core::accounting::Decimal::from_scaled(difference)
            .ok()?
            .canonical(),
    )
    .ok()
}

fn decimal(value: &DecimalAmount) -> Option<gateway_core::accounting::Decimal> {
    value.as_str().parse().ok()
}

fn diagnostic_impact_score(
    request_count: u64,
    total_requests: u64,
    error_rate: f64,
    non_completion_rate: f64,
    retry_rate: f64,
    first_token_p95_ms: Option<u64>,
) -> f64 {
    let request_share = rate_or_zero(request_count, total_requests);
    let slow_score = first_token_p95_ms
        .map_or(0.0, |value| value as f64 / 30_000.0)
        .min(1.0);
    error_rate * 0.35
        + non_completion_rate * 0.25
        + retry_rate.min(1.0) * 0.20
        + request_share * 0.10
        + slow_score * 0.10
}

fn usd_cost(costs: &[CurrencyCost]) -> Option<DecimalAmount> {
    costs
        .iter()
        .find(|cost| cost.currency.eq_ignore_ascii_case("USD"))
        .map(|cost| cost.amount.clone())
}

impl DefaultObservabilityService {
    async fn enrich_dashboard_quotas(&self, accounts: &mut [DashboardAccountUsage]) {
        let providers = self.providers.clone();
        let quota_reads = accounts.iter().map(|account| {
            let account_id = ProviderAccountId::new(account.account_id.clone()).ok();
            let provider = ProviderKind::new(account.provider_kind.clone())
                .ok()
                .and_then(|kind| providers.require(&kind).ok());
            async move {
                let (Some(account_id), Some(provider)) = (account_id, provider) else {
                    return None;
                };
                provider
                    .quota(ProviderQuotaRequest {
                        account_id,
                        refresh: false,
                        rolling_usage: None,
                    })
                    .await
                    .ok()
                    .and_then(|quota| quota.representative_used_percent())
            }
        });
        let quota_used = futures::future::join_all(quota_reads).await;
        for (account, used_percent) in accounts.iter_mut().zip(quota_used) {
            account.quota_used_percent = used_percent;
        }
    }

    /// 逐条尽力把可校验的总额升级为完整分解；单条脏数据（非法 Provider kind、
    /// 不支持的来源或费用规则失败）只保留该条已存的总额，不影响整页返回。
    fn enrich_billing(&self, records: &mut [crate::model::observability::UsageRecord]) {
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
                cached_tokens: record.cached_tokens,
                cache_write_tokens: record.cache_write_tokens,
                total: total.clone(),
            };
            if let Ok(Some(breakdown)) = self.providers.calculated_billing(&provider_kind, &input) {
                record.billing = Some(UsageBilling::Calculated(Box::new(breakdown)));
            }
        }
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
        metrics.success_count.saturating_add(service_failure_count),
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

fn quarter_hour_start(value: DateTime<Utc>) -> DateTime<Utc> {
    let elapsed = value
        .timestamp()
        .rem_euclid(HEALTH_TIMELINE_SLOT_MINUTES * 60);
    value - Duration::seconds(elapsed) - Duration::nanoseconds(i64::from(value.nanosecond()))
}
