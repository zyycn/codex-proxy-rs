//! 管理端 Dashboard 聚合视图。

use std::{cmp::Reverse, collections::HashMap};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    admin::{
        auth::session::AdminAuth,
        monitoring::{
            account_usage_service::{
                AdminUsageRecord, AdminUsageSummary, AdminUsageTimeBucketRecord,
            },
            diagnostics::{
                fingerprint_diagnostics, AccountCapacityDiagnostics, AccountPoolDiagnostics,
            },
            usage_record_routes::{usage_record_items, UsageRecordData},
            usage_record_service::AdminUsageRecordFilter,
        },
        response::{AdminEnvelope, AdminError, AdminResponse},
    },
    infra::{
        format::{
            format_compact_number, format_cost, format_duration_ms, format_percent, format_rate,
            format_tokens, nonnegative_i64_to_u64,
        },
        time::{
            china_datetime, china_datetime_rfc3339_str, china_day_start, china_hour,
            china_hour_start, china_quarter_hour_start, china_relative_time,
        },
    },
    runtime::state::AppState,
    upstream::accounts::model::{Account, AccountStatus},
};

const DASHBOARD_USAGE_RECORD_LIMIT: u32 = 1000;
const HEALTH_TIMELINE_SLOT_MINUTES: i64 = 15;
const DASHBOARD_TIME_BUCKET_SLOTS: i64 = 7 * 24 * 4;
const HEALTH_TIMELINE_SLOTS: i64 = 24 * 4;
const HEALTH_TIMELINE_STABLE_SUCCESS_THRESHOLD: u64 = 3;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DashboardTrendQuery {
    kind: Option<DashboardTrendKind>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum DashboardTrendKind {
    #[default]
    Usage,
    Latency,
    Errors,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardSummaryData {
    cards: DashboardCardsData,
    trend: DashboardTrendData,
    health_timeline: DashboardHealthTimelineData,
    account_usage: Vec<DashboardAccountUsageData>,
    service_statuses: Vec<DashboardServiceStatusData>,
    usage_records: Vec<UsageRecordData>,
    pool_summary: AccountPoolDiagnostics,
    capacity_info: AccountCapacityDiagnostics,
    rotation_strategy: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardHealthTimelineData {
    title: String,
    description: String,
    range_display: String,
    reliability_display: String,
    oldest_label: String,
    newest_label: String,
    points: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardCardsData {
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
    yesterday_requests: String,
    yesterday_requests_value: u64,
    total_requests: String,
    rpm: String,
    tpm: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardTokenCardData {
    today_tokens: String,
    today_tokens_value: u64,
    yesterday_tokens: String,
    yesterday_tokens_value: u64,
    total_tokens: String,
    today_cost_usd: String,
    today_cost_usd_value: f64,
    total_cost_usd: String,
    total_cost_usd_value: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardCacheCardData {
    today_hit_rate: String,
    today_hit_rate_value: Option<f64>,
    yesterday_hit_rate: String,
    yesterday_hit_rate_value: Option<f64>,
    total_hit_rate: String,
    total_cached_tokens: String,
    first_token_latency_ms: String,
    completion_latency_ms: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardTrendData {
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
    tokens: String,
    tokens_value: u64,
    errors: String,
    errors_value: u64,
    latency: String,
    latency_value: u64,
    max_latency: String,
    max_latency_value: u64,
    min_latency: String,
    min_latency_value: u64,
    success_rate: String,
    success_rate_value: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardTrendSummaryData {
    label: String,
    value: String,
    ratio: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardAccountUsageData {
    name: String,
    email: String,
    plan: String,
    requests: String,
    tokens: String,
    quota_used_percent: Option<f64>,
    last_used: String,
    status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardServiceStatusData {
    label: String,
    value: String,
    detail: String,
    tone: String,
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
    latency_sum: u64,
    latency_count: u64,
}

impl UsageWindow {
    fn tokens(self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    fn cache_hit_rate(self) -> Option<f64> {
        if self.input_tokens > 0 {
            Some(self.cached_tokens as f64 / self.input_tokens as f64)
        } else {
            None
        }
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

    fn avg_latency(self) -> Option<u64> {
        self.latency_sum.checked_div(self.latency_count)
    }
}

/// `GET /api/admin/dashboard/summary`
pub(crate) async fn dashboard_summary(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<DashboardTrendQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let accounts = state
        .services
        .accounts
        .list_pool_accounts()
        .await
        .unwrap_or_default();
    let capacity = state.services.account_pool.capacity_summary_now().await;
    let summary = state
        .services
        .usage
        .summary()
        .await
        .unwrap_or_else(|_| empty_usage_summary());
    let usage_records = state
        .services
        .usage
        .list(None, 200)
        .await
        .map(|page| page.items)
        .unwrap_or_default();
    let recent_events = state
        .services
        .usage_records
        .list(
            None,
            DASHBOARD_USAGE_RECORD_LIMIT,
            AdminUsageRecordFilter::default(),
        )
        .await
        .map(|page| page.items)
        .unwrap_or_default();

    let now = Utc::now();
    let time_buckets = dashboard_time_buckets(&state, now).await;
    let pool_summary = account_pool_summary(&accounts);
    let trend = dashboard_trend_data(&time_buckets, query.kind.unwrap_or_default());
    let quota_used_by_account = account_quota_used_percent_by_id(&state, &usage_records).await;
    let settings = state.services.settings.current();
    let recent_usage_records = recent_events.into_iter().take(10).collect::<Vec<_>>();
    let account_emails = state
        .services
        .usage_records
        .account_email_map(&recent_usage_records)
        .await
        .map_err(|error| AdminError::usage_record_accounts_failed(error.to_string()))?;
    let dashboard_usage_records = usage_record_items(recent_usage_records, &account_emails);

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DashboardSummaryData {
            cards: dashboard_cards(&accounts, &summary, &time_buckets),
            trend,
            health_timeline: dashboard_health_timeline_data(&time_buckets),
            account_usage: account_usage_data(&accounts, &usage_records, &quota_used_by_account),
            service_statuses: service_status_data(&state),
            usage_records: dashboard_usage_records,
            pool_summary,
            capacity_info: AccountCapacityDiagnostics::from(capacity),
            rotation_strategy: Some(settings.auth.rotation_strategy.clone()),
        }),
    ))
}

/// `GET /api/admin/dashboard/trend?kind=usage|latency|errors`
pub(crate) async fn dashboard_trend(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<DashboardTrendQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let time_buckets = dashboard_time_buckets(&state, Utc::now()).await;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(dashboard_trend_data(
            &time_buckets,
            query.kind.unwrap_or_default(),
        )),
    ))
}

fn dashboard_cards(
    accounts: &[Account],
    summary: &AdminUsageSummary,
    buckets: &[AdminUsageTimeBucketRecord],
) -> DashboardCardsData {
    dashboard_cards_at(accounts, summary, buckets, Utc::now())
}

fn dashboard_cards_at(
    accounts: &[Account],
    summary: &AdminUsageSummary,
    buckets: &[AdminUsageTimeBucketRecord],
    now: DateTime<Utc>,
) -> DashboardCardsData {
    let today_start = china_day_start(now);
    let yesterday_start = today_start - Duration::days(1);
    let today = usage_window(buckets, today_start, now);
    let yesterday = usage_window(buckets, yesterday_start, today_start);
    let today_cost = cost_window(buckets, today_start, now).unwrap_or(0.0);
    let total_cost = total_cost(buckets);

    let total_input = nonnegative_i64_to_u64(summary.input_tokens);
    let total_cached = nonnegative_i64_to_u64(summary.cached_tokens);
    let total_accounts = accounts.len() as u64;
    let enabled_accounts = accounts
        .iter()
        .filter(|account| account.status == AccountStatus::Active)
        .count() as u64;
    let abnormal_accounts = accounts
        .iter()
        .filter(|account| {
            matches!(
                account.status,
                AccountStatus::Expired
                    | AccountStatus::QuotaExhausted
                    | AccountStatus::Disabled
                    | AccountStatus::Banned
            )
        })
        .count() as u64;
    let total_requests = nonnegative_i64_to_u64(summary.request_count);
    let total_tokens = nonnegative_i64_to_u64(summary.input_tokens + summary.output_tokens);
    let total_hit_rate = if total_input > 0 {
        Some(total_cached as f64 / total_input as f64)
    } else {
        None
    };

    DashboardCardsData {
        accounts: DashboardAccountsCardData {
            total: format_compact_number(total_accounts),
            total_value: total_accounts,
            enabled: format_compact_number(enabled_accounts),
            enabled_value: enabled_accounts,
            abnormal: format_compact_number(abnormal_accounts),
            abnormal_value: abnormal_accounts,
        },
        traffic: DashboardTrafficCardData {
            today_requests: format_compact_number(today.requests),
            today_requests_value: today.requests,
            yesterday_requests: format_compact_number(yesterday.requests),
            yesterday_requests_value: yesterday.requests,
            total_requests: format_compact_number(total_requests),
            rpm: format_compact_number(today.requests),
            tpm: format_tokens(today.tokens()),
        },
        tokens: DashboardTokenCardData {
            today_tokens: format_tokens(today.tokens()),
            today_tokens_value: today.tokens(),
            yesterday_tokens: format_tokens(yesterday.tokens()),
            yesterday_tokens_value: yesterday.tokens(),
            total_tokens: format_tokens(total_tokens),
            today_cost_usd: format_optional_cost(Some(today_cost)),
            today_cost_usd_value: today_cost,
            total_cost_usd: format_optional_cost(Some(total_cost)),
            total_cost_usd_value: total_cost,
        },
        cache: DashboardCacheCardData {
            today_hit_rate: format_rate(today.cache_hit_rate()),
            today_hit_rate_value: today.cache_hit_rate(),
            yesterday_hit_rate: format_rate(yesterday.cache_hit_rate()),
            yesterday_hit_rate_value: yesterday.cache_hit_rate(),
            total_hit_rate: format_rate(total_hit_rate),
            total_cached_tokens: format_tokens(total_cached),
            first_token_latency_ms: format_optional_duration_ms(today.avg_first_token_latency()),
            completion_latency_ms: format_optional_duration_ms(today.avg_latency()),
        },
    }
}

fn format_optional_cost(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), format_cost)
}

fn format_optional_duration_ms(value: Option<u64>) -> String {
    format_duration_ms(value.and_then(|value| i64::try_from(value).ok()))
}

async fn dashboard_time_buckets(
    state: &AppState,
    now: DateTime<Utc>,
) -> Vec<AdminUsageTimeBucketRecord> {
    let current_slot = china_quarter_hour_start(now);
    let start = current_slot
        - Duration::minutes(HEALTH_TIMELINE_SLOT_MINUTES * (DASHBOARD_TIME_BUCKET_SLOTS - 1));
    state
        .services
        .usage
        .time_buckets(start, now)
        .await
        .unwrap_or_default()
}

fn dashboard_trend_data(
    buckets: &[AdminUsageTimeBucketRecord],
    kind: DashboardTrendKind,
) -> DashboardTrendData {
    dashboard_trend_data_at(buckets, kind, Utc::now())
}

fn dashboard_trend_data_at(
    records: &[AdminUsageTimeBucketRecord],
    kind: DashboardTrendKind,
    now: DateTime<Utc>,
) -> DashboardTrendData {
    let current_hour = china_hour_start(now);
    let start = current_hour - Duration::hours(23);
    let mut buckets = (0..24)
        .map(|index| {
            let bucket_start = start + Duration::hours(index);
            (bucket_start, UsageWindow::default())
        })
        .collect::<Vec<_>>();

    for record in records {
        if record.bucket_start < start || record.bucket_start > now {
            continue;
        }
        let record_hour = china_hour_start(record.bucket_start);
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(bucket_start, _)| *bucket_start == record_hour)
        {
            apply_bucket(bucket, record);
        }
    }

    let points = buckets
        .iter()
        .map(|(bucket_start, bucket)| {
            let latency = bucket.avg_first_token_latency().unwrap_or(0);
            let max_latency = bucket.max_first_token_bucket_latency().unwrap_or(0);
            let min_latency = bucket.min_first_token_bucket_latency().unwrap_or(0);
            let tokens = bucket.tokens();
            let success_rate = if bucket.requests > 0 {
                ((bucket.requests - bucket.errors) as f64 / bucket.requests as f64 * 1000.0).round()
                    / 10.0
            } else {
                0.0
            };
            DashboardTrendPointData {
                time: format!("{:02}", china_hour(bucket_start)),
                requests: format_compact_number(bucket.requests),
                requests_value: bucket.requests,
                input_tokens: format_tokens(bucket.input_tokens),
                input_tokens_value: bucket.input_tokens,
                output_tokens: format_tokens(bucket.output_tokens),
                output_tokens_value: bucket.output_tokens,
                cached_tokens: format_tokens(bucket.cached_tokens),
                cached_tokens_value: bucket.cached_tokens,
                tokens: format_tokens(tokens),
                tokens_value: tokens,
                errors: format_compact_number(bucket.errors),
                errors_value: bucket.errors,
                latency: format_optional_duration_ms(Some(latency)),
                latency_value: latency,
                max_latency: format_optional_duration_ms(Some(max_latency)),
                max_latency_value: max_latency,
                min_latency: format_optional_duration_ms(Some(min_latency)),
                min_latency_value: min_latency,
                success_rate: format_percent(success_rate),
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

fn dashboard_health_timeline_data(
    buckets: &[AdminUsageTimeBucketRecord],
) -> DashboardHealthTimelineData {
    dashboard_health_timeline_data_at(buckets, Utc::now())
}

fn dashboard_health_timeline_data_at(
    records: &[AdminUsageTimeBucketRecord],
    now: DateTime<Utc>,
) -> DashboardHealthTimelineData {
    let current_slot = china_quarter_hour_start(now);
    let start = china_day_start(now);
    let mut buckets = (0..HEALTH_TIMELINE_SLOTS)
        .map(|index| {
            let bucket_start = start + Duration::minutes(HEALTH_TIMELINE_SLOT_MINUTES * index);
            (bucket_start, UsageWindow::default())
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
            apply_bucket(bucket, record);
        }
    }

    let requests = buckets
        .iter()
        .filter(|(bucket_start, _)| *bucket_start <= current_slot)
        .map(|(_, bucket)| bucket.requests)
        .sum::<u64>();
    let errors = buckets
        .iter()
        .filter(|(bucket_start, _)| *bucket_start <= current_slot)
        .map(|(_, bucket)| bucket.errors)
        .sum::<u64>();
    let successes = requests.saturating_sub(errors);
    let reliability = if requests > 0 {
        (successes as f64 / requests as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };
    DashboardHealthTimelineData {
        title: "请求健康时间线".to_string(),
        description: "今日请求可靠性".to_string(),
        range_display: format!("{} - {}", china_datetime(&start), china_datetime(&now)),
        reliability_display: if requests > 0 {
            format!("{reliability:.1}%")
        } else {
            "-".to_string()
        },
        oldest_label: "00:00".to_string(),
        newest_label: "24:00".to_string(),
        points: buckets
            .into_iter()
            .map(|(bucket_start, bucket)| {
                let success_rate = if bucket.requests > 0 {
                    (bucket.requests.saturating_sub(bucket.errors) as f64 / bucket.requests as f64
                        * 1000.0)
                        .round()
                        / 10.0
                } else {
                    0.0
                };
                health_tone(bucket, success_rate, bucket_start > current_slot)
            })
            .collect(),
    }
}

fn health_tone(bucket: UsageWindow, success_rate: f64, is_future: bool) -> char {
    let successes = bucket.requests.saturating_sub(bucket.errors);
    if is_future {
        '0'
    } else if bucket.requests == 0 {
        '1'
    } else if successes == 0 {
        '2'
    } else if success_rate < 90.0 {
        '3'
    } else if successes < HEALTH_TIMELINE_STABLE_SUCCESS_THRESHOLD {
        '4'
    } else {
        '5'
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
                points.iter().map(|point| point.input_tokens_value).sum(),
                None,
            ),
            trend_summary_item(
                kind,
                "输出",
                points.iter().map(|point| point.output_tokens_value).sum(),
                None,
            ),
            trend_summary_item(
                kind,
                "缓存",
                points.iter().map(|point| point.cached_tokens_value).sum(),
                None,
            ),
        ],
        DashboardTrendKind::Latency => {
            let samples = points
                .iter()
                .filter(|point| point.latency_value > 0)
                .collect::<Vec<_>>();
            let avg = if samples.is_empty() {
                0
            } else {
                samples.iter().map(|point| point.latency_value).sum::<u64>() / samples.len() as u64
            };
            vec![
                trend_summary_item(kind, "平均", avg, None),
                trend_summary_item(
                    kind,
                    "最高",
                    samples
                        .iter()
                        .map(|point| point.max_latency_value)
                        .max()
                        .unwrap_or(0),
                    None,
                ),
                trend_summary_item(
                    kind,
                    "最低",
                    samples
                        .iter()
                        .filter_map(|point| {
                            (point.min_latency_value > 0).then_some(point.min_latency_value)
                        })
                        .min()
                        .unwrap_or(0),
                    None,
                ),
            ]
        }
        DashboardTrendKind::Errors => {
            let errors = points.iter().map(|point| point.errors_value).sum::<u64>();
            let requests = points.iter().map(|point| point.requests_value).sum::<u64>();
            let success_rate = if requests > 0 {
                Some(((requests - errors) as f64 / requests as f64 * 1000.0).round() / 10.0)
            } else {
                None
            };
            vec![
                trend_summary_item(kind, "错误数", errors, None),
                trend_summary_item(kind, "成功率", 0, success_rate),
                trend_summary_item(kind, "总请求", requests, None),
            ]
        }
    }
}

fn trend_summary_item(
    kind: DashboardTrendKind,
    label: &str,
    value: u64,
    ratio: Option<f64>,
) -> DashboardTrendSummaryData {
    DashboardTrendSummaryData {
        label: label.to_string(),
        value: trend_summary_value_display(kind, value),
        ratio: ratio.map(format_percent),
    }
}

fn trend_summary_value_display(kind: DashboardTrendKind, value: u64) -> String {
    match kind {
        DashboardTrendKind::Usage => format_tokens(value),
        DashboardTrendKind::Latency => format_optional_duration_ms(Some(value)),
        DashboardTrendKind::Errors => format_compact_number(value),
    }
}

fn usage_window(
    records: &[AdminUsageTimeBucketRecord],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> UsageWindow {
    let mut window = UsageWindow::default();
    for record in records {
        if record.bucket_start >= start && record.bucket_start < end {
            apply_bucket(&mut window, record);
        }
    }
    window
}

fn cost_window(
    records: &[AdminUsageTimeBucketRecord],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Option<f64> {
    let mut total = 0.0;
    let mut has_usage = false;
    for record in records {
        if record.bucket_start >= start && record.bucket_start < end {
            if let Some(cost) = record.cost_usd {
                total += cost;
                has_usage = true;
            }
        }
    }
    has_usage.then_some(total)
}

fn total_cost(records: &[AdminUsageTimeBucketRecord]) -> f64 {
    let mut total = 0.0;
    let mut has_usage = false;
    for record in records {
        if let Some(cost) = record.cost_usd {
            total += cost;
            has_usage = true;
        }
    }
    if has_usage {
        return total;
    }

    0.0
}

fn apply_bucket(window: &mut UsageWindow, record: &AdminUsageTimeBucketRecord) {
    window.requests += nonnegative_i64_to_u64(record.request_count);
    window.input_tokens += nonnegative_i64_to_u64(record.input_tokens);
    window.output_tokens += nonnegative_i64_to_u64(record.output_tokens);
    window.cached_tokens += nonnegative_i64_to_u64(record.cached_tokens);
    window.errors += nonnegative_i64_to_u64(record.error_count);
    window.first_token_latency_sum += nonnegative_i64_to_u64(record.first_token_latency_sum);
    window.first_token_latency_count += nonnegative_i64_to_u64(record.first_token_latency_count);
    let bucket_first_token_latency = first_token_bucket_latency(record);
    if let Some(latency) = bucket_first_token_latency {
        window.max_first_token_bucket_latency = window.max_first_token_bucket_latency.max(latency);
        window.min_first_token_bucket_latency = if window.min_first_token_bucket_latency == 0 {
            latency
        } else {
            window.min_first_token_bucket_latency.min(latency)
        };
    }
    window.latency_sum += nonnegative_i64_to_u64(record.latency_sum);
    window.latency_count += nonnegative_i64_to_u64(record.latency_count);
}

fn first_token_bucket_latency(record: &AdminUsageTimeBucketRecord) -> Option<u64> {
    let sum = nonnegative_i64_to_u64(record.first_token_latency_sum);
    let count = nonnegative_i64_to_u64(record.first_token_latency_count);
    sum.checked_div(count).filter(|latency| *latency > 0)
}

fn account_pool_summary(accounts: &[Account]) -> AccountPoolDiagnostics {
    let mut summary = AccountPoolDiagnostics {
        total: accounts.len(),
        ..AccountPoolDiagnostics::default()
    };
    for account in accounts {
        match account.status {
            AccountStatus::Active => summary.active += 1,
            AccountStatus::Expired => summary.expired += 1,
            AccountStatus::QuotaExhausted => summary.quota_exhausted += 1,
            AccountStatus::Refreshing => summary.refreshing += 1,
            AccountStatus::Disabled => summary.disabled += 1,
            AccountStatus::Banned => summary.banned += 1,
        }
    }
    summary
}

fn account_usage_data(
    accounts: &[Account],
    usage_records: &[AdminUsageRecord],
    quota_used_by_account: &HashMap<String, f64>,
) -> Vec<DashboardAccountUsageData> {
    let account_by_id = accounts
        .iter()
        .map(|account| (account.id.as_str(), account))
        .collect::<HashMap<_, _>>();
    let mut records = usage_records.iter().collect::<Vec<_>>();
    records.sort_by_key(|record| Reverse((record.last_used_at, record.account_id.clone())));
    let mut rows = records
        .iter()
        .map(|usage| {
            let account = account_by_id.get(usage.account_id.as_str()).copied();
            let name = account
                .and_then(|account| account.label.clone())
                .or_else(|| usage.label.clone())
                .or_else(|| {
                    usage
                        .email
                        .as_ref()
                        .and_then(|email| email.split('@').next().map(ToString::to_string))
                })
                .unwrap_or_else(|| usage.account_id.chars().take(8).collect());
            let status = account.map_or_else(
                || "active".to_string(),
                |account| account.status.to_string(),
            );
            let quota_used_percent = quota_used_by_account
                .get(&usage.account_id)
                .copied()
                .or_else(|| (status == "quota_exhausted").then_some(100.0));
            let requests = nonnegative_i64_to_u64(usage.request_count);
            let tokens = nonnegative_i64_to_u64(usage.input_tokens + usage.output_tokens);
            DashboardAccountUsageData {
                name,
                email: usage.email.clone().unwrap_or_else(|| "-".to_string()),
                plan: usage
                    .plan_type
                    .clone()
                    .unwrap_or_else(|| "free".to_string()),
                requests: format_compact_number(requests),
                tokens: format_tokens(tokens),
                quota_used_percent,
                last_used: china_relative_time(usage.last_used_at, Utc::now()),
                status,
            }
        })
        .collect::<Vec<_>>();
    rows.truncate(4);
    rows
}

async fn account_quota_used_percent_by_id(
    state: &AppState,
    usage_records: &[AdminUsageRecord],
) -> HashMap<String, f64> {
    let mut records = usage_records.iter().collect::<Vec<_>>();
    records.sort_by_key(|record| Reverse((record.last_used_at, record.account_id.clone())));
    records.truncate(4);

    let mut quota_used_by_account = HashMap::with_capacity(records.len());
    for usage in records {
        let Ok(Some(quota_json)) = state
            .services
            .accounts
            .get_quota_json(&usage.account_id)
            .await
        else {
            continue;
        };
        if let Some(used_percent) = quota_used_percent(&quota_json) {
            quota_used_by_account.insert(usage.account_id.clone(), used_percent);
        }
    }
    quota_used_by_account
}

fn quota_used_percent(quota_json: &str) -> Option<f64> {
    let quota = serde_json::from_str::<Value>(quota_json).ok()?;
    let mut values = Vec::new();
    if let Some(used_percent) = quota
        .pointer("/monthly_limit/used_percent")
        .and_then(percent_value)
    {
        values.push(used_percent);
    }
    if let Some(snapshots) = quota.get("snapshots").and_then(Value::as_array) {
        for snapshot in snapshots {
            for role in ["primary", "secondary"] {
                if let Some(used_percent) = snapshot
                    .get(role)
                    .and_then(|window| window.get("used_percent"))
                    .and_then(percent_value)
                {
                    values.push(used_percent);
                }
            }
        }
    }
    values.into_iter().max_by(f64::total_cmp)
}

fn percent_value(value: &Value) -> Option<f64> {
    let percent = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))?;
    percent.is_finite().then_some(percent.clamp(0.0, 100.0))
}

fn service_status_data(state: &AppState) -> Vec<DashboardServiceStatusData> {
    let fingerprint = fingerprint_diagnostics(&state.services.fingerprint);
    vec![
        DashboardServiceStatusData {
            label: "客户端版本".to_string(),
            value: empty_dash(fingerprint.app_version),
            detail: if fingerprint.build_number.trim().is_empty() {
                "-".to_string()
            } else {
                format!("Build {}", fingerprint.build_number)
            },
            tone: "info".to_string(),
        },
        DashboardServiceStatusData {
            label: "平台架构".to_string(),
            value: empty_dash(fingerprint.platform),
            detail: empty_dash(fingerprint.arch),
            tone: "info".to_string(),
        },
        DashboardServiceStatusData {
            label: "Chromium".to_string(),
            value: if fingerprint.chromium_version.trim().is_empty() {
                "-".to_string()
            } else {
                format!("v{}", fingerprint.chromium_version)
            },
            detail: empty_dash(fingerprint.originator),
            tone: "normal".to_string(),
        },
        DashboardServiceStatusData {
            label: "更新时间".to_string(),
            value: format_fingerprint_updated_at(fingerprint.updated_at),
            detail: String::new(),
            tone: "normal".to_string(),
        },
        DashboardServiceStatusData {
            label: "User Agent".to_string(),
            value: fingerprint.user_agent,
            detail: String::new(),
            tone: "normal".to_string(),
        },
    ]
}

fn empty_dash(value: String) -> String {
    if value.trim().is_empty() {
        "-".to_string()
    } else {
        value
    }
}

fn format_fingerprint_updated_at(value: Option<String>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    let value = value.trim();
    if value.is_empty() {
        return "-".to_string();
    }
    china_datetime_rfc3339_str(value)
}

fn empty_usage_summary() -> AdminUsageSummary {
    AdminUsageSummary {
        account_count: 0,
        request_count: 0,
        empty_response_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 0,
        image_input_tokens: 0,
        image_output_tokens: 0,
        image_request_count: 0,
        image_request_failed_count: 0,
    }
}
