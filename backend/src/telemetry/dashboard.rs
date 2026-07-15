//! Dashboard 遥测聚合与展示模型。

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::infra::{
    format::{
        format_billing_amount, format_compact_number, format_duration_ms, format_percent,
        format_rate, format_tokens, nonnegative_i64_to_u64,
    },
    time::{china_day_start, china_quarter_hour_start},
};

use super::account_usage::query::{AccountUsageTimeBucket, RetainedUsageSummary};
use super::usage::insights::RequestHealthTimeBucket;

const HEALTH_TIMELINE_SLOT_MINUTES: i64 = 15;
const HEALTH_TIMELINE_SLOTS: i64 = 24 * 4;
const HEALTH_TIMELINE_MIN_SAMPLE_SIZE: u64 = 10;
const HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD: u64 = 3;
const HEALTH_TIMELINE_STABLE_RELIABILITY: f64 = 99.0;
const TREND_SLOT_MINUTES: i64 = 15;

#[derive(Debug, Clone, Copy, Default)]
pub struct DashboardAccountCounts {
    pub total: u64,
    pub enabled: u64,
    pub abnormal: u64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DashboardTrendKind {
    #[default]
    Usage,
    Latency,
    Errors,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardHealthTimelineData {
    title: String,
    description: String,
    reliability_display: String,
    status: DashboardHealthTimelineStatus,
    success_requests: u64,
    failed_requests: u64,
    cancelled_requests: u64,
    caller_error_requests: u64,
    points: Vec<DashboardHealthTimelinePointData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardHealthTimelinePointData {
    time: String,
    status: DashboardHealthTimelineStatus,
    reliability_display: String,
    success_requests: u64,
    failed_requests: u64,
    cancelled_requests: u64,
    caller_error_requests: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum DashboardHealthTimelineStatus {
    Future,
    NoData,
    Unavailable,
    Unstable,
    LowSample,
    Stable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCardsData {
    accounts: DashboardAccountsCardData,
    traffic: DashboardTrafficCardData,
    tokens: DashboardTokenCardData,
    cache: DashboardCacheCardData,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardAccountsCardData {
    total: String,
    total_value: u64,
    enabled: String,
    enabled_value: u64,
    abnormal: String,
    abnormal_value: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardTrafficCardData {
    today_requests: String,
    today_requests_value: u64,
    yesterday_requests_value: u64,
    total_requests: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardTokenCardData {
    today_tokens: String,
    today_tokens_value: u64,
    yesterday_tokens_value: u64,
    total_tokens: String,
    total_billing_amount_usd: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardCacheCardData {
    today_hit_rate: String,
    today_hit_rate_value: Option<f64>,
    yesterday_hit_rate_value: Option<f64>,
    total_hit_rate: String,
    total_cached_tokens: String,
    average_first_token_latency_ms: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTrendData {
    kind: DashboardTrendKind,
    points: Vec<DashboardTrendPointData>,
    summary: Vec<DashboardTrendSummaryData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardTrendPointData {
    time: String,
    requests: String,
    requests_value: u64,
    input_tokens: String,
    input_tokens_value: u64,
    output_tokens: String,
    output_tokens_value: u64,
    cached_tokens: String,
    cached_tokens_value: u64,
    cache_hit_rate_value: f64,
    tokens_value: u64,
    errors: String,
    errors_value: u64,
    latency: String,
    latency_value: Option<u64>,
    max_latency: String,
    max_latency_value: Option<u64>,
    min_latency: String,
    min_latency_value: Option<u64>,
    success_rate: String,
    success_rate_value: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardTrendSummaryData {
    label: String,
    value: String,
    ratio: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageWindow {
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    errors: u64,
    first_token_latency_sum: u64,
    first_token_latency_count: u64,
    max_first_token_bucket_latency: u64,
    min_first_token_bucket_latency: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct HealthWindow {
    success_requests: u64,
    failed_requests: u64,
    cancelled_requests: u64,
    caller_error_requests: u64,
}

impl UsageWindow {
    fn tokens(self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    fn cache_hit_rate(self) -> Option<f64> {
        (self.input_tokens > 0).then(|| self.cached_tokens as f64 / self.input_tokens as f64)
    }

    fn avg_first_token_latency(self) -> Option<u64> {
        self.first_token_latency_sum
            .checked_div(self.first_token_latency_count)
    }

    fn max_first_token_bucket_latency(self) -> Option<u64> {
        (self.max_first_token_bucket_latency > 0).then_some(self.max_first_token_bucket_latency)
    }

    fn min_first_token_bucket_latency(self) -> Option<u64> {
        (self.min_first_token_bucket_latency > 0).then_some(self.min_first_token_bucket_latency)
    }
}

pub fn dashboard_cards(
    accounts: DashboardAccountCounts,
    buckets: &[AccountUsageTimeBucket],
    retained_usage: &RetainedUsageSummary,
) -> DashboardCardsData {
    dashboard_cards_at(accounts, buckets, retained_usage, Utc::now())
}

pub fn dashboard_cards_at(
    accounts: DashboardAccountCounts,
    buckets: &[AccountUsageTimeBucket],
    retained_usage: &RetainedUsageSummary,
    now: DateTime<Utc>,
) -> DashboardCardsData {
    let today_start = china_day_start(now);
    let yesterday_start = today_start - Duration::days(1);
    let today = usage_window(buckets, today_start, now);
    let yesterday = usage_window(buckets, yesterday_start, today_start);
    let total_requests = nonnegative_i64_to_u64(retained_usage.totals.request_count);
    let total_tokens = nonnegative_i64_to_u64(retained_usage.totals.input_tokens)
        .saturating_add(nonnegative_i64_to_u64(retained_usage.totals.output_tokens));
    let total_cached_tokens = nonnegative_i64_to_u64(retained_usage.totals.cached_tokens);

    DashboardCardsData {
        accounts: DashboardAccountsCardData {
            total: format_compact_number(accounts.total),
            total_value: accounts.total,
            enabled: format_compact_number(accounts.enabled),
            enabled_value: accounts.enabled,
            abnormal: format_compact_number(accounts.abnormal),
            abnormal_value: accounts.abnormal,
        },
        traffic: DashboardTrafficCardData {
            today_requests: format_compact_number(today.requests),
            today_requests_value: today.requests,
            yesterday_requests_value: yesterday.requests,
            total_requests: format_compact_number(total_requests),
        },
        tokens: DashboardTokenCardData {
            today_tokens: format_tokens(today.tokens()),
            today_tokens_value: today.tokens(),
            yesterday_tokens_value: yesterday.tokens(),
            total_tokens: format_tokens(total_tokens),
            total_billing_amount_usd: format_billing_amount(retained_usage.billing_amount_usd),
        },
        cache: DashboardCacheCardData {
            today_hit_rate: format_rate(today.cache_hit_rate()),
            today_hit_rate_value: today.cache_hit_rate(),
            yesterday_hit_rate_value: yesterday.cache_hit_rate(),
            total_hit_rate: format_rate(summary_cache_hit_rate(retained_usage)),
            total_cached_tokens: format_tokens(total_cached_tokens),
            average_first_token_latency_ms: format_optional_duration_ms(
                today.avg_first_token_latency(),
            ),
        },
    }
}

pub fn dashboard_trend_data(
    buckets: &[AccountUsageTimeBucket],
    kind: DashboardTrendKind,
) -> DashboardTrendData {
    dashboard_trend_data_at(buckets, kind, Utc::now())
}

pub fn dashboard_trend_data_at(
    records: &[AccountUsageTimeBucket],
    kind: DashboardTrendKind,
    now: DateTime<Utc>,
) -> DashboardTrendData {
    let current_slot = china_quarter_hour_start(now);
    let start = china_day_start(now);
    let elapsed_slots =
        current_slot.signed_duration_since(start).num_minutes() / TREND_SLOT_MINUTES;
    let mut buckets = (0..=elapsed_slots)
        .map(|index| {
            (
                start + Duration::minutes(TREND_SLOT_MINUTES * index),
                UsageWindow::default(),
            )
        })
        .collect::<Vec<_>>();

    for record in records {
        if record.bucket_start < start || record.bucket_start > now {
            continue;
        }
        let record_slot = china_quarter_hour_start(record.bucket_start);
        let slot_index =
            record_slot.signed_duration_since(start).num_minutes() / TREND_SLOT_MINUTES;
        let Ok(slot_index) = usize::try_from(slot_index) else {
            continue;
        };
        let Some((_, bucket)) = buckets.get_mut(slot_index) else {
            continue;
        };
        apply_bucket(bucket, record);
    }

    let points = buckets
        .iter()
        .map(|(bucket_start, bucket)| {
            let latency = bucket.avg_first_token_latency();
            let max_latency = bucket.max_first_token_bucket_latency();
            let min_latency = bucket.min_first_token_bucket_latency();
            let cache_hit_rate = bucket.cache_hit_rate().unwrap_or(0.0);
            let success_rate = (bucket.requests > 0).then(|| {
                (bucket.requests.saturating_sub(bucket.errors) as f64 / bucket.requests as f64
                    * 1000.0)
                    .round()
                    / 10.0
            });
            let elapsed_minutes = bucket_start.signed_duration_since(start).num_minutes();
            DashboardTrendPointData {
                time: format!("{:02}:{:02}", elapsed_minutes / 60, elapsed_minutes % 60),
                requests: format_compact_number(bucket.requests),
                requests_value: bucket.requests,
                input_tokens: format_tokens(bucket.input_tokens),
                input_tokens_value: bucket.input_tokens,
                output_tokens: format_tokens(bucket.output_tokens),
                output_tokens_value: bucket.output_tokens,
                cached_tokens: format_tokens(bucket.cached_tokens),
                cached_tokens_value: bucket.cached_tokens,
                cache_hit_rate_value: cache_hit_rate,
                tokens_value: bucket.tokens(),
                errors: format_compact_number(bucket.errors),
                errors_value: bucket.errors,
                latency: format_optional_duration_ms(latency),
                latency_value: latency,
                max_latency: format_optional_duration_ms(max_latency),
                max_latency_value: max_latency,
                min_latency: format_optional_duration_ms(min_latency),
                min_latency_value: min_latency,
                success_rate: success_rate
                    .map(format_percent)
                    .unwrap_or_else(|| "—".to_string()),
                success_rate_value: success_rate,
            }
        })
        .collect::<Vec<_>>();

    DashboardTrendData {
        kind,
        summary: trend_summary(kind, &points),
        points,
    }
}

pub fn dashboard_health_timeline_data(
    buckets: &[RequestHealthTimeBucket],
) -> DashboardHealthTimelineData {
    dashboard_health_timeline_data_at(buckets, Utc::now())
}

pub fn dashboard_health_timeline_data_at(
    records: &[RequestHealthTimeBucket],
    now: DateTime<Utc>,
) -> DashboardHealthTimelineData {
    let current_slot = china_quarter_hour_start(now);
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
        let record_slot = china_quarter_hour_start(record.bucket_start);
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(bucket_start, _)| *bucket_start == record_slot)
        {
            bucket.success_requests = bucket
                .success_requests
                .saturating_add(record.success_requests);
            bucket.failed_requests = bucket
                .failed_requests
                .saturating_add(record.failed_requests);
            bucket.cancelled_requests = bucket
                .cancelled_requests
                .saturating_add(record.cancelled_requests);
            bucket.caller_error_requests = bucket
                .caller_error_requests
                .saturating_add(record.caller_error_requests);
        }
    }

    let totals = buckets
        .iter()
        .filter(|(bucket_start, _)| *bucket_start <= current_slot)
        .fold(HealthWindow::default(), |mut totals, (_, bucket)| {
            totals.success_requests = totals
                .success_requests
                .saturating_add(bucket.success_requests);
            totals.failed_requests = totals
                .failed_requests
                .saturating_add(bucket.failed_requests);
            totals.cancelled_requests = totals
                .cancelled_requests
                .saturating_add(bucket.cancelled_requests);
            totals.caller_error_requests = totals
                .caller_error_requests
                .saturating_add(bucket.caller_error_requests);
            totals
        });
    DashboardHealthTimelineData {
        title: "请求健康时间线".to_string(),
        description: "有效请求可用性".to_string(),
        reliability_display: format_health_reliability(totals),
        status: health_status(totals, false),
        success_requests: totals.success_requests,
        failed_requests: totals.failed_requests,
        cancelled_requests: totals.cancelled_requests,
        caller_error_requests: totals.caller_error_requests,
        points: buckets
            .into_iter()
            .enumerate()
            .map(|(index, (bucket_start, bucket))| {
                let elapsed_minutes = index as i64 * HEALTH_TIMELINE_SLOT_MINUTES;
                DashboardHealthTimelinePointData {
                    time: format!("{:02}:{:02}", elapsed_minutes / 60, elapsed_minutes % 60),
                    status: health_status(bucket, bucket_start > current_slot),
                    reliability_display: format_health_reliability(bucket),
                    success_requests: bucket.success_requests,
                    failed_requests: bucket.failed_requests,
                    cancelled_requests: bucket.cancelled_requests,
                    caller_error_requests: bucket.caller_error_requests,
                }
            })
            .collect(),
    }
}

fn trend_summary(
    kind: DashboardTrendKind,
    points: &[DashboardTrendPointData],
) -> Vec<DashboardTrendSummaryData> {
    match kind {
        DashboardTrendKind::Usage => vec![
            trend_summary_item(
                kind,
                "输入",
                Some(points.iter().map(|p| p.input_tokens_value).sum()),
                None,
            ),
            trend_summary_item(
                kind,
                "输出",
                Some(points.iter().map(|p| p.output_tokens_value).sum()),
                None,
            ),
            trend_summary_item(
                kind,
                "缓存",
                Some(points.iter().map(|p| p.cached_tokens_value).sum()),
                None,
            ),
        ],
        DashboardTrendKind::Latency => {
            let (latency_sum, latency_count) = points
                .iter()
                .filter_map(|point| point.latency_value)
                .fold((0_u64, 0_u64), |(sum, count), latency| {
                    (sum + latency, count + 1)
                });
            let average = (latency_count > 0).then(|| latency_sum / latency_count);
            vec![
                trend_summary_item(kind, "平均", average, None),
                trend_summary_item(
                    kind,
                    "最高",
                    points
                        .iter()
                        .filter_map(|point| point.max_latency_value)
                        .max(),
                    None,
                ),
                trend_summary_item(
                    kind,
                    "最低",
                    points
                        .iter()
                        .filter_map(|point| point.min_latency_value)
                        .min(),
                    None,
                ),
            ]
        }
        DashboardTrendKind::Errors => {
            let errors = points.iter().map(|point| point.errors_value).sum::<u64>();
            let requests = points.iter().map(|point| point.requests_value).sum::<u64>();
            let success_rate = (requests > 0).then(|| {
                (requests.saturating_sub(errors) as f64 / requests as f64 * 1000.0).round() / 10.0
            });
            vec![
                trend_summary_item(kind, "错误数", Some(errors), None),
                trend_summary_item(kind, "成功率", None, success_rate),
                trend_summary_item(kind, "总请求", Some(requests), None),
            ]
        }
    }
}

fn trend_summary_item(
    kind: DashboardTrendKind,
    label: &str,
    value: Option<u64>,
    ratio: Option<f64>,
) -> DashboardTrendSummaryData {
    DashboardTrendSummaryData {
        label: label.to_string(),
        value: value
            .map(|value| match kind {
                DashboardTrendKind::Usage => format_tokens(value),
                DashboardTrendKind::Latency => format_optional_duration_ms(Some(value)),
                DashboardTrendKind::Errors => format_compact_number(value),
            })
            .unwrap_or_else(|| "—".to_string()),
        ratio: ratio.map(format_percent),
    }
}

fn usage_window(
    records: &[AccountUsageTimeBucket],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> UsageWindow {
    records
        .iter()
        .filter(|record| record.bucket_start >= start && record.bucket_start < end)
        .fold(UsageWindow::default(), |mut window, record| {
            apply_bucket(&mut window, record);
            window
        })
}

fn apply_bucket(window: &mut UsageWindow, record: &AccountUsageTimeBucket) {
    window.requests += nonnegative_i64_to_u64(record.request_count);
    window.input_tokens += nonnegative_i64_to_u64(record.input_tokens);
    window.output_tokens += nonnegative_i64_to_u64(record.output_tokens);
    window.cached_tokens += nonnegative_i64_to_u64(record.cached_tokens);
    window.errors += nonnegative_i64_to_u64(record.error_count);
    window.first_token_latency_sum += nonnegative_i64_to_u64(record.first_token_latency_sum);
    window.first_token_latency_count += nonnegative_i64_to_u64(record.first_token_latency_count);
    if let Some(latency) = first_token_bucket_latency(record) {
        window.max_first_token_bucket_latency = window.max_first_token_bucket_latency.max(latency);
        window.min_first_token_bucket_latency = if window.min_first_token_bucket_latency == 0 {
            latency
        } else {
            window.min_first_token_bucket_latency.min(latency)
        };
    }
}

fn first_token_bucket_latency(record: &AccountUsageTimeBucket) -> Option<u64> {
    nonnegative_i64_to_u64(record.first_token_latency_sum)
        .checked_div(nonnegative_i64_to_u64(record.first_token_latency_count))
        .filter(|latency| *latency > 0)
}

fn health_status(bucket: HealthWindow, is_future: bool) -> DashboardHealthTimelineStatus {
    let eligible_requests = bucket
        .success_requests
        .saturating_add(bucket.failed_requests);
    if is_future {
        DashboardHealthTimelineStatus::Future
    } else if eligible_requests == 0 {
        DashboardHealthTimelineStatus::NoData
    } else if bucket.success_requests == 0
        && bucket.failed_requests >= HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD
    {
        DashboardHealthTimelineStatus::Unavailable
    } else if eligible_requests < HEALTH_TIMELINE_MIN_SAMPLE_SIZE {
        DashboardHealthTimelineStatus::LowSample
    } else if health_reliability(bucket)
        .is_some_and(|reliability| reliability < HEALTH_TIMELINE_STABLE_RELIABILITY)
    {
        DashboardHealthTimelineStatus::Unstable
    } else {
        DashboardHealthTimelineStatus::Stable
    }
}

fn health_reliability(bucket: HealthWindow) -> Option<f64> {
    let eligible_requests = bucket
        .success_requests
        .saturating_add(bucket.failed_requests);
    (eligible_requests > 0)
        .then(|| bucket.success_requests as f64 / eligible_requests as f64 * 100.0)
}

fn format_health_reliability(bucket: HealthWindow) -> String {
    health_reliability(bucket)
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "-".to_string())
}

fn summary_cache_hit_rate(summary: &RetainedUsageSummary) -> Option<f64> {
    let input_tokens = nonnegative_i64_to_u64(summary.totals.input_tokens);
    (input_tokens > 0)
        .then(|| nonnegative_i64_to_u64(summary.totals.cached_tokens) as f64 / input_tokens as f64)
}

fn format_optional_duration_ms(value: Option<u64>) -> String {
    format_duration_ms(value.and_then(|value| i64::try_from(value).ok()))
}
