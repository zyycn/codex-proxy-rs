//! Client 自助查询领域事实到安全 wire DTO 的白名单投影。

use chrono::{DateTime, Duration, Utc};
use gateway_admin::model::{
    client_usage::{ClientOverview, ClientUsageKey},
    observability::{
        DiagnosticsResult, Granularity, OpsErrorPage, RequestMetricPoint, RequestMetrics,
        UsageInsights, UsageListRecord, UsagePage, UsageSummary,
    },
};
use gateway_core::{engine::budget::ClientBudgetStatus, metering::Decimal};
use serde::Serialize;

use crate::admin::observability::{
    BillingView, DashboardWireProfileView, DiagnosticDimension as WireDiagnosticDimension,
    DiagnosticsView, HealthTimelineView, OverviewCostView, OverviewHealthView,
    OverviewPerformancePointView, OverviewPerformanceView, TokenDetailsView, china_datetime,
    diagnostics_view, health_timeline_view, usage_insights_view, usage_list_record_view,
    usage_summary_view, wire_profile_view,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientUsageRecordsData {
    items: Vec<ClientUsageRecordData>,
    current_page: u32,
    page_size: u16,
    total: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientUsageRecordData {
    id: String,
    route: String,
    model: Option<String>,
    requested_model: Option<String>,
    upstream_model: Option<String>,
    client_transport: String,
    upstream_transport: Option<String>,
    reasoning_effort: Option<String>,
    reasoning_preset: Option<String>,
    subagent_kind: Option<String>,
    compact: bool,
    token_details: TokenDetailsView,
    billing: Option<BillingView>,
    latency_details: ClientUsageLatencyDetailsData,
    first_token_latency_ms: Option<u64>,
    latency_ms: Option<u64>,
    created_at: DateTime<Utc>,
    created_at_display: String,
    client_ip: Option<String>,
    user_agent: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientUsageLatencyDetailsData {
    #[serde(skip_serializing_if = "Option::is_none")]
    admission_decision_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_decision_wait_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ws_connect_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream_headers_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_event_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_reasoning_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_text_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_token_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    openai_processing_ms: Option<u64>,
}

impl From<UsagePage> for ClientUsageRecordsData {
    fn from(page: UsagePage) -> Self {
        Self {
            items: page
                .items
                .into_iter()
                .map(ClientUsageRecordData::from)
                .collect(),
            current_page: page.current_page,
            page_size: page.page_size,
            total: page.total,
        }
    }
}

impl From<UsageListRecord> for ClientUsageRecordData {
    fn from(record: UsageListRecord) -> Self {
        let view = usage_list_record_view(record);
        Self {
            id: view.id,
            route: view.route,
            model: view.model,
            requested_model: view.requested_model,
            upstream_model: view.upstream_model,
            client_transport: view.client_transport,
            upstream_transport: view.upstream_transport,
            reasoning_effort: view.reasoning_effort,
            reasoning_preset: view.reasoning_preset,
            subagent_kind: view.subagent_kind,
            compact: view.compact,
            token_details: view.token_details,
            billing: view.billing,
            latency_details: ClientUsageLatencyDetailsData {
                admission_decision_ms: view.latency_details.admission_decision_ms,
                transport_decision_wait_ms: view.latency_details.transport_decision_wait_ms,
                ws_connect_ms: view.latency_details.ws_connect_ms,
                upstream_headers_ms: view.latency_details.upstream_headers_ms,
                first_event_ms: view.latency_details.first_event_ms,
                first_reasoning_ms: view.latency_details.first_reasoning_ms,
                first_text_ms: view.latency_details.first_text_ms,
                first_token_ms: view.latency_details.first_token_ms,
                openai_processing_ms: view.latency_details.openai_processing_ms,
            },
            first_token_latency_ms: view.first_token_latency_ms,
            latency_ms: view.latency_ms,
            created_at: view.created_at,
            created_at_display: view.created_at_display,
            client_ip: view.client_ip,
            user_agent: view.user_agent,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientUsageSummaryData {
    total_requests: String,
    input_tokens: String,
    output_tokens: String,
    cached_tokens: String,
    cache_write_tokens: String,
    total_tokens: String,
    average_latency_ms: String,
}

impl From<UsageSummary> for ClientUsageSummaryData {
    fn from(summary: UsageSummary) -> Self {
        let view = usage_summary_view(summary);
        Self {
            total_requests: view.total_requests,
            input_tokens: view.input_tokens,
            output_tokens: view.output_tokens,
            cached_tokens: view.cached_tokens,
            cache_write_tokens: view.cache_write_tokens,
            total_tokens: view.total_tokens,
            average_latency_ms: view.average_latency_ms,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientUsageInsightsData {
    granularity: String,
    health: OverviewHealthView,
    performance: ClientUsagePerformanceData,
    cost: OverviewCostView,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientUsagePerformanceData {
    latency_p50_ms: Option<f64>,
    latency_p95_ms: Option<f64>,
    latency_p99_ms: Option<f64>,
    first_token_p50_ms: Option<f64>,
    first_token_p95_ms: Option<f64>,
    first_token_p99_ms: Option<f64>,
    output_throughput_p10: Option<u64>,
    output_throughput_p50: Option<u64>,
    output_throughput_p90: Option<u64>,
    latency_coverage: f64,
    first_token_coverage: f64,
    points: Vec<ClientUsagePerformancePointData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientUsagePerformancePointData {
    bucket: DateTime<Utc>,
    label: String,
    latency_p50_ms: Option<f64>,
    latency_p95_ms: Option<f64>,
    latency_p99_ms: Option<f64>,
    first_token_p50_ms: Option<f64>,
    first_token_p95_ms: Option<f64>,
    first_token_p99_ms: Option<f64>,
    output_throughput_p10: Option<u64>,
    output_throughput_p50: Option<u64>,
    output_throughput_p90: Option<u64>,
}

impl From<UsageInsights> for ClientUsageInsightsData {
    fn from(insights: UsageInsights) -> Self {
        let view = usage_insights_view(insights);
        Self {
            granularity: view.granularity,
            health: view.health,
            performance: ClientUsagePerformanceData::from(view.performance),
            cost: view.cost,
        }
    }
}

impl From<OverviewPerformanceView> for ClientUsagePerformanceData {
    fn from(performance: OverviewPerformanceView) -> Self {
        Self {
            latency_p50_ms: performance.latency_p50_ms,
            latency_p95_ms: performance.latency_p95_ms,
            latency_p99_ms: performance.latency_p99_ms,
            first_token_p50_ms: performance.first_token_p50_ms,
            first_token_p95_ms: performance.first_token_p95_ms,
            first_token_p99_ms: performance.first_token_p99_ms,
            output_throughput_p10: performance.output_throughput_p10,
            output_throughput_p50: performance.output_throughput_p50,
            output_throughput_p90: performance.output_throughput_p90,
            latency_coverage: performance.latency_coverage,
            first_token_coverage: performance.first_token_coverage,
            points: performance
                .points
                .into_iter()
                .map(ClientUsagePerformancePointData::from)
                .collect(),
        }
    }
}

impl From<OverviewPerformancePointView> for ClientUsagePerformancePointData {
    fn from(point: OverviewPerformancePointView) -> Self {
        Self {
            bucket: point.bucket,
            label: point.label,
            latency_p50_ms: point.latency_p50_ms,
            latency_p95_ms: point.latency_p95_ms,
            latency_p99_ms: point.latency_p99_ms,
            first_token_p50_ms: point.first_token_p50_ms,
            first_token_p95_ms: point.first_token_p95_ms,
            first_token_p99_ms: point.first_token_p99_ms,
            output_throughput_p10: point.output_throughput_p10,
            output_throughput_p50: point.output_throughput_p50,
            output_throughput_p90: point.output_throughput_p90,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub(super) struct ClientDiagnosticsData(DiagnosticsView);

impl From<DiagnosticsResult> for ClientDiagnosticsData {
    fn from(result: DiagnosticsResult) -> Self {
        let dimension = client_dimension(result.dimension);
        Self(diagnostics_view(result, dimension))
    }
}

fn client_dimension(
    dimension: gateway_admin::model::observability::DiagnosticDimension,
) -> WireDiagnosticDimension {
    use gateway_admin::model::observability::DiagnosticDimension;
    match dimension {
        DiagnosticDimension::Model => WireDiagnosticDimension::Model,
        DiagnosticDimension::Transport => WireDiagnosticDimension::Transport,
        DiagnosticDimension::Failure => WireDiagnosticDimension::Failure,
        _ => WireDiagnosticDimension::Model,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientOpsErrorsData {
    items: Vec<ClientOpsErrorData>,
    current_page: u32,
    page_size: u16,
    total: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientOpsErrorData {
    id: String,
    request_id: Option<String>,
    kind: String,
    route: String,
    model: Option<String>,
    requested_model: Option<String>,
    upstream_model: Option<String>,
    client_transport: Option<String>,
    upstream_transport: Option<String>,
    failure_class: String,
    upstream_send_state: Option<String>,
    client_status_code: Option<u16>,
    upstream_status_code: Option<u16>,
    latency_ms: Option<u64>,
    client_ip: Option<String>,
    user_agent: Option<String>,
    reasoning_effort: Option<String>,
    reasoning_preset: Option<String>,
    subagent_kind: Option<String>,
    compact: bool,
    message: String,
    recovered_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    created_at_display: String,
}

impl From<OpsErrorPage> for ClientOpsErrorsData {
    fn from(page: OpsErrorPage) -> Self {
        Self {
            items: page
                .items
                .into_iter()
                .map(|error| {
                    let model = error
                        .upstream_model_id
                        .clone()
                        .or_else(|| error.requested_model_id.clone());
                    ClientOpsErrorData {
                        id: error.event_id,
                        request_id: error.request_id,
                        kind: error.component,
                        route: error.endpoint.unwrap_or_default(),
                        model,
                        requested_model: error.requested_model_id,
                        upstream_model: error.upstream_model_id,
                        client_transport: error.client_transport,
                        upstream_transport: error.upstream_transport,
                        message: error.failure_kind.clone(),
                        failure_class: error.failure_kind,
                        upstream_send_state: error.upstream_send_state,
                        client_status_code: error.client_status_code,
                        upstream_status_code: error.upstream_status_code,
                        latency_ms: error.latency_ms,
                        client_ip: error.client_ip,
                        user_agent: error.user_agent,
                        reasoning_effort: error.reasoning_effort,
                        reasoning_preset: error.reasoning_preset,
                        subagent_kind: error.subagent_kind,
                        compact: error.compact.unwrap_or(false),
                        recovered_at: error.recovered_at,
                        created_at: error.occurred_at,
                        created_at_display: china_datetime(&error.occurred_at),
                    }
                })
                .collect(),
            current_page: page.current_page,
            page_size: page.page_size,
            total: page.total,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientKeyData {
    name: String,
    label: Option<String>,
    prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_used_at: Option<String>,
}

impl From<&ClientUsageKey> for ClientKeyData {
    fn from(key: &ClientUsageKey) -> Self {
        Self {
            name: key.name.clone(),
            label: key.label.clone(),
            prefix: key.prefix.clone(),
            last_used_at: key.last_used_at.map(|value| value.to_rfc3339()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientOverviewData {
    as_of: String,
    timezone: &'static str,
    currency: &'static str,
    key: ClientKeyData,
    budget: BudgetData,
    limits: LimitsData,
    today: TodayData,
    lifetime: LifetimeData,
    health_timeline: HealthTimelineView,
    wire_profiles: Vec<DashboardWireProfileView>,
    usage_records: Vec<ClientUsageRecordData>,
}

impl From<&ClientOverview> for ClientOverviewData {
    fn from(snapshot: &ClientOverview) -> Self {
        Self {
            as_of: snapshot.as_of.to_rfc3339(),
            timezone: "Asia/Shanghai",
            currency: "USD",
            key: ClientKeyData::from(&snapshot.key),
            budget: BudgetData::from(&snapshot.key.budget),
            limits: LimitsData {
                max_concurrency: snapshot.key.limits.max_concurrency,
                requests_per_minute: snapshot.key.limits.requests_per_minute,
            },
            today: TodayData {
                range: TodayRangeData {
                    starts_at: snapshot.range.start.to_rfc3339(),
                    ends_at: snapshot.range.end.to_rfc3339(),
                    granularity: snapshot
                        .trend
                        .first()
                        .map_or("hour", |point| granularity(point.granularity)),
                },
                totals: TodayTotalsData::from(&snapshot.overview.requests),
                billed_usd: usd_cost(&snapshot.overview.attempts.costs),
                trend: snapshot.trend.iter().map(TrendPointData::from).collect(),
            },
            lifetime: LifetimeData {
                request_count: snapshot.totals.request_count,
                input_tokens: snapshot.totals.input_tokens,
                cached_tokens: snapshot.totals.cached_tokens,
                total_tokens: snapshot.totals.total_tokens,
                billed_usd: snapshot
                    .totals
                    .billing_usd
                    .as_ref()
                    .map_or_else(|| "0".to_owned(), |value| value.as_str().to_owned()),
            },
            health_timeline: health_timeline_view(snapshot.health_timeline.clone()),
            wire_profiles: snapshot
                .wire_profiles
                .iter()
                .cloned()
                .map(wire_profile_view)
                .collect(),
            usage_records: snapshot
                .usage_records
                .iter()
                .cloned()
                .map(ClientUsageRecordData::from)
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct BudgetData {
    daily: BudgetWindowData,
    weekly: BudgetWindowData,
}

impl From<&ClientBudgetStatus> for BudgetData {
    fn from(budget: &ClientBudgetStatus) -> Self {
        Self {
            daily: BudgetWindowData::new(
                budget.limits.daily_usd,
                budget.daily_used_usd,
                budget.daily_resets_at.map(DateTime::<Utc>::from),
                Duration::days(1),
            ),
            weekly: BudgetWindowData::new(
                budget.limits.weekly_usd,
                budget.weekly_used_usd,
                budget.weekly_resets_at.map(DateTime::<Utc>::from),
                Duration::days(7),
            ),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BudgetWindowData {
    limit_usd: String,
    used_usd: String,
    remaining_usd: Option<String>,
    starts_at: Option<String>,
    resets_at: Option<String>,
}

impl BudgetWindowData {
    fn new(
        limit: Decimal,
        used: Decimal,
        resets_at: Option<DateTime<Utc>>,
        width: Duration,
    ) -> Self {
        let remaining_usd = (limit != Decimal::ZERO).then(|| {
            Decimal::from_scaled(limit.scaled().saturating_sub(used.scaled()))
                .unwrap_or(Decimal::ZERO)
                .canonical()
        });
        Self {
            limit_usd: limit.canonical(),
            used_usd: used.canonical(),
            remaining_usd,
            starts_at: resets_at.map(|value| (value - width).to_rfc3339()),
            resets_at: resets_at.map(|value| value.to_rfc3339()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LimitsData {
    max_concurrency: u64,
    requests_per_minute: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TodayData {
    range: TodayRangeData,
    totals: TodayTotalsData,
    billed_usd: String,
    trend: Vec<TrendPointData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TodayRangeData {
    starts_at: String,
    ends_at: String,
    granularity: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TodayTotalsData {
    request_count: u64,
    success_count: u64,
    failure_count: u64,
    cancelled_count: u64,
    incomplete_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    total_tokens: u64,
    average_first_token_latency_ms: Option<u64>,
}

impl From<&RequestMetrics> for TodayTotalsData {
    fn from(metrics: &RequestMetrics) -> Self {
        Self {
            request_count: metrics.request_count,
            success_count: metrics.success_count,
            failure_count: metrics.failure_count,
            cancelled_count: metrics.cancelled_count,
            incomplete_count: metrics.incomplete_count,
            input_tokens: metrics.input_tokens,
            output_tokens: metrics.output_tokens,
            cached_tokens: metrics.cached_tokens,
            total_tokens: metrics.total_tokens,
            average_first_token_latency_ms: average(
                metrics.first_token_latency_sum_ms,
                metrics.first_token_latency_count,
            ),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LifetimeData {
    request_count: u64,
    input_tokens: u64,
    cached_tokens: u64,
    total_tokens: u64,
    billed_usd: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrendPointData {
    bucket_start: String,
    request_count: u64,
    success_count: u64,
    failure_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    total_tokens: u64,
    first_token_p50_ms: Option<f64>,
    first_token_p95_ms: Option<f64>,
    latency_p50_ms: Option<f64>,
    latency_p95_ms: Option<f64>,
    output_throughput_p50: Option<u64>,
    billed_usd: String,
}

impl From<&RequestMetricPoint> for TrendPointData {
    fn from(point: &RequestMetricPoint) -> Self {
        Self {
            bucket_start: point.bucket_start.to_rfc3339(),
            request_count: point.metrics.request_count,
            success_count: point.metrics.success_count,
            failure_count: point.metrics.failure_count,
            input_tokens: point.metrics.input_tokens,
            output_tokens: point.metrics.output_tokens,
            cached_tokens: point.metrics.cached_tokens,
            total_tokens: point.metrics.total_tokens,
            first_token_p50_ms: point
                .metrics
                .first_token_latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            first_token_p95_ms: point
                .metrics
                .first_token_latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            latency_p50_ms: point
                .metrics
                .latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            latency_p95_ms: point
                .metrics
                .latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            output_throughput_p50: point.metrics.output_throughput_p50,
            billed_usd: usd_cost(&point.costs),
        }
    }
}

fn usd_cost(costs: &[gateway_admin::model::observability::CurrencyCost]) -> String {
    costs
        .iter()
        .find(|cost| cost.currency == "USD")
        .map_or_else(|| "0".to_owned(), |cost| cost.amount.as_str().to_owned())
}

const fn granularity(value: Granularity) -> &'static str {
    match value {
        Granularity::FifteenMinutes => "15m",
        Granularity::Hour => "hour",
        Granularity::Day => "day",
    }
}

fn average(total: u64, count: u64) -> Option<u64> {
    (count > 0).then(|| total / count)
}
