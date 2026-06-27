//! 管理端 Dashboard 聚合视图。

use std::{cmp::Reverse, collections::HashMap};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    admin::{
        auth::session::require_admin_session,
        monitoring::{
            billing,
            diagnostics::{
                fingerprint_diagnostics, AccountCapacityDiagnostics, AccountPoolDiagnostics,
            },
            event_store::AdminLogFilter,
            events::{EventLevel, EventLog},
            service::{AdminUsageRecord, AdminUsageSummary, AdminUsageTimeBucketRecord},
        },
        response::{AdminEnvelope, AdminError, AdminResponse},
    },
    http::middleware::request_id::RequestId,
    infra::time::{
        china_datetime, china_datetime_rfc3339_str, china_day_start, china_hour, china_hour_start,
        china_quarter_hour_start, china_relative_time, china_time,
    },
    runtime::state::AppState,
    upstream::accounts::model::{Account, AccountStatus},
};

const DASHBOARD_LOG_LIMIT: u32 = 1000;
const HEALTH_TIMELINE_SLOT_MINUTES: i64 = 15;
const HEALTH_TIMELINE_SLOTS: i64 = 7 * 24 * 4;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTrendQuery {
    pub kind: Option<DashboardTrendKind>,
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
pub struct DashboardSummaryData {
    pub cards: DashboardCardsData,
    pub trend: DashboardTrendData,
    pub health_timeline: DashboardHealthTimelineData,
    pub account_usage: Vec<DashboardAccountUsageData>,
    pub service_statuses: Vec<DashboardServiceStatusData>,
    pub event_logs: Vec<DashboardEventLogData>,
    pub pool_summary: AccountPoolDiagnostics,
    pub capacity_info: AccountCapacityDiagnostics,
    pub rotation_strategy: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardHealthTimelineData {
    pub title: String,
    pub description: String,
    pub range_display: String,
    pub reliability_display: String,
    pub oldest_label: String,
    pub newest_label: String,
    pub points: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCardsData {
    pub accounts: DashboardAccountsCardData,
    pub traffic: DashboardTrafficCardData,
    pub tokens: DashboardTokenCardData,
    pub cache: DashboardCacheCardData,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAccountsCardData {
    pub total: u64,
    pub enabled: u64,
    pub abnormal: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTrafficCardData {
    pub today_requests: u64,
    pub yesterday_requests: u64,
    pub total_requests: u64,
    pub rpm: u64,
    pub tpm: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTokenCardData {
    pub today_tokens: u64,
    pub yesterday_tokens: u64,
    pub total_tokens: u64,
    pub today_cost_usd: Option<f64>,
    pub total_cost_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCacheCardData {
    pub today_hit_rate: Option<f64>,
    pub yesterday_hit_rate: Option<f64>,
    pub total_hit_rate: Option<f64>,
    pub total_cached_tokens: u64,
    pub first_token_latency_ms: Option<u64>,
    pub completion_latency_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTrendData {
    pub kind: DashboardTrendKind,
    pub points: Vec<DashboardTrendPointData>,
    pub summary: Vec<DashboardTrendSummaryData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTrendPointData {
    pub time: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub tokens: u64,
    pub errors: u64,
    pub latency: u64,
    pub max_latency: u64,
    pub min_latency: u64,
    pub success_rate: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTrendSummaryData {
    pub label: String,
    pub value: u64,
    pub ratio: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAccountUsageData {
    pub name: String,
    pub email: String,
    pub plan: String,
    pub requests: u64,
    pub tokens: u64,
    pub quota_used_percent: Option<f64>,
    pub last_used: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardEventLogData {
    pub id: String,
    pub time: String,
    pub level: EventLevel,
    pub request_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub latency_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardServiceStatusData {
    pub label: String,
    pub value: String,
    pub detail: String,
    pub tone: String,
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
    latency_sum: u64,
    latency_count: u64,
    max_latency: u64,
    min_latency: u64,
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

    fn avg_latency(self) -> Option<u64> {
        self.latency_sum.checked_div(self.latency_count)
    }
}

/// `GET /api/admin/dashboard/summary`
pub async fn dashboard_summary(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

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
    let logs = state
        .services
        .logs
        .list(None, DASHBOARD_LOG_LIMIT, AdminLogFilter::default())
        .await
        .map(|page| page.items)
        .unwrap_or_default();

    let now = Utc::now();
    let time_buckets = dashboard_time_buckets(&state, now).await;
    let pool_summary = account_pool_summary(&accounts);
    let dashboard_logs = logs.iter().take(10).map(dashboard_event_log_data).collect();
    let trend = dashboard_trend_data(&time_buckets, DashboardTrendKind::Usage);
    let quota_used_by_account = account_quota_used_percent_by_id(&state, &usage_records).await;
    let settings = state.services.settings.current();

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            DashboardSummaryData {
                cards: dashboard_cards(&accounts, &summary, &time_buckets),
                trend,
                health_timeline: dashboard_health_timeline_data(&time_buckets),
                account_usage: account_usage_data(
                    &accounts,
                    &usage_records,
                    &quota_used_by_account,
                ),
                service_statuses: service_status_data(&state),
                event_logs: dashboard_logs,
                pool_summary,
                capacity_info: AccountCapacityDiagnostics::from(capacity),
                rotation_strategy: Some(settings.auth.rotation_strategy.clone()),
            },
            request_id,
        ),
    ))
}

/// `GET /api/admin/dashboard/trend?kind=usage|latency|errors`
pub async fn dashboard_trend(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<DashboardTrendQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let time_buckets = dashboard_time_buckets(&state, Utc::now()).await;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            dashboard_trend_data(&time_buckets, query.kind.unwrap_or_default()),
            request_id,
        ),
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

    DashboardCardsData {
        accounts: DashboardAccountsCardData {
            total: accounts.len() as u64,
            enabled: accounts
                .iter()
                .filter(|account| account.status == AccountStatus::Active)
                .count() as u64,
            abnormal: accounts
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
                .count() as u64,
        },
        traffic: DashboardTrafficCardData {
            today_requests: today.requests,
            yesterday_requests: yesterday.requests,
            total_requests: nonnegative_i64_to_u64(summary.request_count),
            rpm: today.requests,
            tpm: today.tokens(),
        },
        tokens: DashboardTokenCardData {
            today_tokens: today.tokens(),
            yesterday_tokens: yesterday.tokens(),
            total_tokens: nonnegative_i64_to_u64(summary.input_tokens + summary.output_tokens),
            today_cost_usd: Some(today_cost),
            total_cost_usd: Some(total_cost),
        },
        cache: DashboardCacheCardData {
            today_hit_rate: today.cache_hit_rate(),
            yesterday_hit_rate: yesterday.cache_hit_rate(),
            total_hit_rate: if total_input > 0 {
                Some(total_cached as f64 / total_input as f64)
            } else {
                None
            },
            total_cached_tokens: total_cached,
            first_token_latency_ms: today.avg_first_token_latency(),
            completion_latency_ms: today.avg_latency(),
        },
    }
}

async fn dashboard_time_buckets(
    state: &AppState,
    now: DateTime<Utc>,
) -> Vec<AdminUsageTimeBucketRecord> {
    let current_slot = china_quarter_hour_start(now);
    let start = current_slot
        - Duration::minutes(HEALTH_TIMELINE_SLOT_MINUTES * (HEALTH_TIMELINE_SLOTS - 1));
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
        let log_hour = china_hour_start(record.bucket_start);
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(bucket_start, _)| *bucket_start == log_hour)
        {
            apply_bucket(bucket, record);
        }
    }

    let points = buckets
        .iter()
        .map(|(bucket_start, bucket)| {
            let latency = bucket.avg_latency().unwrap_or(0);
            DashboardTrendPointData {
                time: format!("{:02}", china_hour(bucket_start)),
                requests: bucket.requests,
                input_tokens: bucket.input_tokens,
                output_tokens: bucket.output_tokens,
                cached_tokens: bucket.cached_tokens,
                tokens: bucket.tokens(),
                errors: bucket.errors,
                latency,
                max_latency: bucket.max_latency,
                min_latency: bucket.min_latency,
                success_rate: if bucket.requests > 0 {
                    ((bucket.requests - bucket.errors) as f64 / bucket.requests as f64 * 1000.0)
                        .round()
                        / 10.0
                } else {
                    0.0
                },
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
    let start = current_slot
        - Duration::minutes(HEALTH_TIMELINE_SLOT_MINUTES * (HEALTH_TIMELINE_SLOTS - 1));
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
        let log_slot = china_quarter_hour_start(record.bucket_start);
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(bucket_start, _)| *bucket_start == log_slot)
        {
            apply_bucket(bucket, record);
        }
    }

    let requests = buckets
        .iter()
        .map(|(_, bucket)| bucket.requests)
        .sum::<u64>();
    let errors = buckets.iter().map(|(_, bucket)| bucket.errors).sum::<u64>();
    let reliability = if requests > 0 {
        ((requests - errors) as f64 / requests as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };
    let end = current_slot + Duration::minutes(HEALTH_TIMELINE_SLOT_MINUTES);

    DashboardHealthTimelineData {
        title: "请求健康时间线".to_string(),
        description: "最近 7 天请求可靠性".to_string(),
        range_display: format!("{} - {}", china_datetime(&start), china_datetime(&end)),
        reliability_display: if requests > 0 {
            format!("{reliability:.1}%")
        } else {
            "-".to_string()
        },
        oldest_label: "最早".to_string(),
        newest_label: "最新".to_string(),
        points: buckets
            .into_iter()
            .map(|(_, bucket)| {
                let success_rate = if bucket.requests > 0 {
                    ((bucket.requests - bucket.errors) as f64 / bucket.requests as f64 * 1000.0)
                        .round()
                        / 10.0
                } else {
                    0.0
                };
                health_tone(bucket, success_rate)
            })
            .collect(),
    }
}

fn health_tone(bucket: UsageWindow, success_rate: f64) -> char {
    if bucket.requests == 0 {
        '0'
    } else if bucket.errors == 0 {
        '1'
    } else if success_rate >= 90.0 {
        '2'
    } else {
        '3'
    }
}

fn trend_summary(
    kind: DashboardTrendKind,
    points: &[DashboardTrendPointData],
) -> Vec<DashboardTrendSummaryData> {
    match kind {
        DashboardTrendKind::Usage => vec![
            DashboardTrendSummaryData {
                label: "输入".to_string(),
                value: points.iter().map(|point| point.input_tokens).sum(),
                ratio: None,
            },
            DashboardTrendSummaryData {
                label: "输出".to_string(),
                value: points.iter().map(|point| point.output_tokens).sum(),
                ratio: None,
            },
            DashboardTrendSummaryData {
                label: "缓存".to_string(),
                value: points.iter().map(|point| point.cached_tokens).sum(),
                ratio: None,
            },
        ],
        DashboardTrendKind::Latency => {
            let samples = points
                .iter()
                .filter(|point| point.latency > 0)
                .collect::<Vec<_>>();
            let avg = if samples.is_empty() {
                0
            } else {
                samples.iter().map(|point| point.latency).sum::<u64>() / samples.len() as u64
            };
            vec![
                DashboardTrendSummaryData {
                    label: "平均".to_string(),
                    value: avg,
                    ratio: None,
                },
                DashboardTrendSummaryData {
                    label: "最高".to_string(),
                    value: samples
                        .iter()
                        .map(|point| point.max_latency)
                        .max()
                        .unwrap_or(0),
                    ratio: None,
                },
                DashboardTrendSummaryData {
                    label: "最低".to_string(),
                    value: samples
                        .iter()
                        .filter_map(|point| (point.min_latency > 0).then_some(point.min_latency))
                        .min()
                        .unwrap_or(0),
                    ratio: None,
                },
            ]
        }
        DashboardTrendKind::Errors => {
            let errors = points.iter().map(|point| point.errors).sum::<u64>();
            let requests = points.iter().map(|point| point.requests).sum::<u64>();
            let success_rate = if requests > 0 {
                Some(((requests - errors) as f64 / requests as f64 * 1000.0).round() / 10.0)
            } else {
                None
            };
            vec![
                DashboardTrendSummaryData {
                    label: "错误数".to_string(),
                    value: errors,
                    ratio: None,
                },
                DashboardTrendSummaryData {
                    label: "成功率".to_string(),
                    value: 0,
                    ratio: success_rate,
                },
                DashboardTrendSummaryData {
                    label: "总请求".to_string(),
                    value: requests,
                    ratio: None,
                },
            ]
        }
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
            if let Some(cost) = bucket_cost(record) {
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
        if let Some(cost) = bucket_cost(record) {
            total += cost;
            has_usage = true;
        }
    }
    if has_usage {
        return total;
    }

    0.0
}

fn bucket_cost(record: &AdminUsageTimeBucketRecord) -> Option<f64> {
    let input_tokens = nonnegative_i64_to_u64(record.input_tokens);
    let output_tokens = nonnegative_i64_to_u64(record.output_tokens);
    let cached_tokens = nonnegative_i64_to_u64(record.cached_tokens);
    if input_tokens == 0 && output_tokens == 0 && cached_tokens == 0 {
        return None;
    }

    let model = record.model.trim();
    if model.is_empty() {
        return None;
    }
    let service_tier = record.service_tier.as_deref();

    Some(billing::calculate_cost(
        input_tokens,
        output_tokens,
        cached_tokens,
        model,
        service_tier,
    ))
}

fn apply_bucket(window: &mut UsageWindow, record: &AdminUsageTimeBucketRecord) {
    window.requests += nonnegative_i64_to_u64(record.request_count);
    window.input_tokens += nonnegative_i64_to_u64(record.input_tokens);
    window.output_tokens += nonnegative_i64_to_u64(record.output_tokens);
    window.cached_tokens += nonnegative_i64_to_u64(record.cached_tokens);
    window.errors += nonnegative_i64_to_u64(record.error_count);
    window.first_token_latency_sum += nonnegative_i64_to_u64(record.first_token_latency_sum);
    window.first_token_latency_count += nonnegative_i64_to_u64(record.first_token_latency_count);
    window.latency_sum += nonnegative_i64_to_u64(record.latency_sum);
    window.latency_count += nonnegative_i64_to_u64(record.latency_count);
    let max_latency = nonnegative_i64_to_u64(record.max_latency_ms);
    if max_latency > 0 {
        window.max_latency = window.max_latency.max(max_latency);
    }
    let min_latency = nonnegative_i64_to_u64(record.min_latency_ms);
    if min_latency > 0 {
        window.min_latency = if window.min_latency == 0 {
            min_latency
        } else {
            window.min_latency.min(min_latency)
        };
    }
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
    let mut rows = usage_records
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
            DashboardAccountUsageData {
                name,
                email: usage.email.clone().unwrap_or_else(|| "-".to_string()),
                plan: usage
                    .plan_type
                    .clone()
                    .unwrap_or_else(|| "free".to_string()),
                requests: nonnegative_i64_to_u64(usage.request_count),
                tokens: nonnegative_i64_to_u64(usage.input_tokens + usage.output_tokens),
                quota_used_percent,
                last_used: china_relative_time(usage.last_used_at, Utc::now()),
                status,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| Reverse(row.requests));
    rows.truncate(4);
    rows
}

fn dashboard_event_log_data(log: &EventLog) -> DashboardEventLogData {
    DashboardEventLogData {
        id: log.id.clone(),
        time: china_time(&log.created_at),
        level: log.level,
        request_id: log.request_id.clone(),
        route: log.route.clone(),
        model: log.model.clone(),
        status_code: log.status_code,
        latency_ms: log.latency_ms,
    }
}

async fn account_quota_used_percent_by_id(
    state: &AppState,
    usage_records: &[AdminUsageRecord],
) -> HashMap<String, f64> {
    let mut records = usage_records.iter().collect::<Vec<_>>();
    records.sort_by_key(|record| Reverse(record.request_count));
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

fn nonnegative_i64_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_cards_should_use_china_day_boundary() {
        let now = utc("2026-06-24T18:00:00Z");
        let buckets = vec![
            usage_bucket_at("2026-06-24T16:30:00Z", 10),
            usage_bucket_at("2026-06-24T15:30:00Z", 20),
        ];

        let cards = dashboard_cards_at(&[], &empty_usage_summary(), &buckets, now);

        assert_eq!(cards.traffic.today_requests, 1);
        assert_eq!(cards.traffic.yesterday_requests, 1);
        assert_eq!(cards.tokens.today_tokens, 10);
        assert_eq!(cards.tokens.yesterday_tokens, 20);
    }

    #[test]
    fn dashboard_trend_should_bucket_and_label_by_china_hour() {
        let now = utc("2026-06-24T18:37:00Z");
        let buckets = vec![usage_bucket_at("2026-06-24T18:15:00Z", 10)];

        let trend = dashboard_trend_data_at(&buckets, DashboardTrendKind::Usage, now);
        let last = trend.points.last().expect("trend should contain points");

        assert_eq!(last.time, "02");
        assert_eq!(last.requests, 1);
    }

    fn usage_bucket_at(created_at: &str, input_tokens: i64) -> AdminUsageTimeBucketRecord {
        AdminUsageTimeBucketRecord {
            bucket_start: utc(created_at),
            model: "gpt-5.5".to_string(),
            service_tier: None,
            request_count: 1,
            error_count: 0,
            input_tokens,
            output_tokens: 0,
            cached_tokens: 0,
            first_token_latency_sum: 0,
            first_token_latency_count: 0,
            latency_sum: 0,
            latency_count: 0,
            max_latency_ms: 0,
            min_latency_ms: 0,
        }
    }

    fn utc(value: &str) -> DateTime<Utc> {
        value.parse::<DateTime<Utc>>().expect("valid RFC3339 UTC")
    }
}
