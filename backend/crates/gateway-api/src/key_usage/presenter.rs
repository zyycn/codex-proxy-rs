//! 用量响应不含凭据；配置响应仅在显式读取时返回当前 Key 的名称与明文

use chrono::{DateTime, Utc};
use gateway_admin::model::{
    client_keys::ClientKeySecret,
    key_usage::{KeyUsageOverview, KeyUsageRecords},
    observability::{
        CostCoverage, CurrencyCost, Granularity, OpsError, RequestMetrics, UsageListRecord,
    },
    system::SystemVersion,
};
use serde::Serialize;

use crate::admin::observability::{
    BillingView, HealthTimelineView, PageData, TokenDetailsView, billing_view,
    health_timeline_view, usage_list_token_details,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VersionView {
    version: String,
    git_sha: String,
}

pub(super) fn version(version: SystemVersion) -> VersionView {
    // 密钥用户仅查看构建标识，不暴露部署环境、更新状态或内部诊断
    VersionView {
        version: version.version,
        git_sha: version.git_sha,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConfigView {
    name: String,
    plaintext_key: String,
}

pub(super) fn config(secret: ClientKeySecret) -> ConfigView {
    let plaintext_key = secret.expose_for_response().to_owned();
    ConfigView {
        name: secret.record.name,
        plaintext_key,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OverviewView {
    as_of: DateTime<Utc>,
    as_of_display: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    key: KeyView,
    summary: MetricsView,
    trend: Vec<TrendPointView>,
    health_timeline: HealthTimelineView,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KeyView {
    name: String,
    prefix: String,
    max_concurrency: u64,
    requests_per_minute: u64,
    daily_limit_usd: String,
    daily_used_usd: String,
    daily_resets_at: Option<DateTime<Utc>>,
    daily_resets_at_display: Option<String>,
    weekly_limit_usd: String,
    weekly_used_usd: String,
    weekly_resets_at: Option<DateTime<Utc>>,
    weekly_resets_at_display: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricsView {
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd: Option<String>,
    cost_incomplete: bool,
}

fn metrics(value: &RequestMetrics, costs: &[CurrencyCost], coverage: &CostCoverage) -> MetricsView {
    MetricsView {
        requests: value.request_count,
        input_tokens: value.input_tokens,
        output_tokens: value.output_tokens,
        cached_tokens: value.cached_tokens,
        cache_write_tokens: value.cache_write_tokens,
        reasoning_tokens: value.reasoning_tokens,
        total_tokens: value.total_tokens,
        cost_usd: costs
            .iter()
            .find(|cost| cost.currency.eq_ignore_ascii_case("USD"))
            .map(|cost| cost.amount.as_str().to_owned())
            .or_else(|| (value.request_count == 0).then(|| "0".to_owned())),
        cost_incomplete: coverage.partial_count > 0 || coverage.unavailable_count > 0,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrendPointView {
    time: DateTime<Utc>,
    bucket_seconds: u32,
    label: String,
    #[serde(flatten)]
    metrics: MetricsView,
}

pub(super) fn overview(value: KeyUsageOverview, time: crate::time::TimePresenter) -> OverviewView {
    let key = value.key;
    OverviewView {
        as_of: value.overview.range.end,
        as_of_display: time.time(&value.overview.range.end),
        start_time: value.overview.range.start,
        end_time: value.overview.range.end,
        key: KeyView {
            name: key.name,
            prefix: key.prefix,
            max_concurrency: key.limits.max_concurrency,
            requests_per_minute: key.limits.requests_per_minute,
            daily_limit_usd: key.budget.limits.daily_usd.canonical(),
            daily_used_usd: key.budget.daily_used_usd.canonical(),
            daily_resets_at_display: key
                .budget
                .daily_resets_at
                .map(|value| time.datetime(&value.into())),
            daily_resets_at: key.budget.daily_resets_at.map(DateTime::from),
            weekly_limit_usd: key.budget.limits.weekly_usd.canonical(),
            weekly_used_usd: key.budget.weekly_used_usd.canonical(),
            weekly_resets_at_display: key
                .budget
                .weekly_resets_at
                .map(|value| time.datetime(&value.into())),
            weekly_resets_at: key.budget.weekly_resets_at.map(DateTime::from),
        },
        summary: metrics(
            &value.overview.requests,
            &value.overview.attempts.costs,
            &value.overview.attempts.cost_coverage,
        ),
        trend: value
            .trend
            .into_iter()
            .map(|point| TrendPointView {
                time: point.bucket_start,
                label: time.label(
                    point.bucket_start,
                    if point.granularity == Granularity::Day {
                        "%m-%d"
                    } else {
                        "%m-%d %H:%M"
                    },
                ),
                bucket_seconds: match point.granularity {
                    Granularity::FifteenMinutes => 900,
                    Granularity::Hour => 3600,
                    Granularity::Day => 86400,
                },
                metrics: metrics(&point.metrics, &point.costs, &point.cost_coverage),
            })
            .collect(),
        health_timeline: health_timeline_view(value.health_timeline, time),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RecordView {
    id: String,
    created_at: DateTime<Utc>,
    created_at_display: String,
    model: Option<String>,
    route: Option<String>,
    reasoning_effort: Option<String>,
    client_transport: Option<String>,
    upstream_transport: Option<String>,
    token_details: Option<TokenDetailsView>,
    billing: Option<BillingView>,
    latency_ms: Option<u64>,
    first_token_latency_ms: Option<u64>,
    latency_details: OutputTimingView,
    client_ip: Option<String>,
    user_agent: Option<String>,
    status: &'static str,
    status_code: Option<u16>,
}

// Key 只查看自身请求的输出时间，不包含账号容量、调度等待等管理侧观测
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputTimingView {
    #[serde(skip_serializing_if = "Option::is_none")]
    first_event_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_reasoning_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_text_ms: Option<u64>,
}

fn success_record(value: UsageListRecord, time: crate::time::TimePresenter) -> RecordView {
    let token_details = Some(usage_list_token_details(&value));
    let billing = billing_view(value.billing.as_ref());
    RecordView {
        id: value.id,
        created_at: value.started_at,
        created_at_display: time.datetime(&value.started_at),
        model: value.requested_model_id,
        route: Some(value.endpoint),
        reasoning_effort: value.reasoning_effort,
        client_transport: Some(value.client_transport),
        upstream_transport: value.upstream_transport,
        token_details,
        billing,
        latency_ms: value.latency_ms,
        first_token_latency_ms: value.first_token_ms,
        latency_details: OutputTimingView {
            first_event_ms: value.first_event_ms,
            first_reasoning_ms: value.first_reasoning_ms,
            first_text_ms: value.first_text_ms,
        },
        client_ip: value.client_ip,
        user_agent: value.user_agent,
        status: "success",
        // 成功记录不保存 HTTP 状态，不能用 200 伪造缺失的原始事实
        status_code: None,
    }
}

fn error_record(value: OpsError, time: crate::time::TimePresenter) -> RecordView {
    RecordView {
        id: value.event_id,
        created_at: value.occurred_at,
        created_at_display: time.datetime(&value.occurred_at),
        model: value.requested_model_id,
        route: value.endpoint,
        reasoning_effort: value.reasoning_effort,
        client_transport: value.client_transport,
        upstream_transport: value.upstream_transport,
        token_details: None,
        billing: None,
        latency_ms: value.latency_ms,
        first_token_latency_ms: None,
        latency_details: OutputTimingView::default(),
        client_ip: value.client_ip,
        user_agent: value.user_agent,
        status: "error",
        status_code: value.client_status_code,
    }
}

pub(super) fn records(
    value: KeyUsageRecords,
    time: crate::time::TimePresenter,
) -> PageData<RecordView> {
    match value {
        KeyUsageRecords::Success(page) => PageData {
            items: page
                .items
                .into_iter()
                .map(|value| success_record(value, time))
                .collect(),
            current_page: page.current_page,
            page_size: page.page_size,
            total: page.total,
        },
        KeyUsageRecords::Error(page) => PageData {
            items: page
                .items
                .into_iter()
                .map(|value| error_record(value, time))
                .collect(),
            current_page: page.current_page,
            page_size: page.page_size,
            total: page.total,
        },
    }
}
