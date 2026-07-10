//! 管理端 Dashboard 聚合视图。

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    accounts::{
        account::{Account, AccountStatus},
        refresh::token_refresh_status_eligible,
    },
    api::admin::{
        response::{AdminEnvelope, AdminError, AdminResponse},
        session::AdminAuth,
        usage_routes::{
            usage_cost_details, usage_record_models, usage_token_details,
            UsageRecordCostDetailsData, UsageRecordTokenDetailsData,
        },
    },
    bootstrap::state::AppState,
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
    telemetry::{
        account_usage::query::AccountUsageTimeBucket,
        diagnostics::{
            fingerprint_diagnostics, AccountCapacityDiagnostics, AccountPoolDiagnostics,
        },
        usage::{
            query::UsageQueryFilter,
            store::{UsageRecordAccountUsage, UsageRecordSummary},
            types::{metadata_string, UsageRecord},
        },
    },
};

const DASHBOARD_ACCOUNT_USAGE_LIMIT: u32 = 4;
const DASHBOARD_USAGE_RECORD_LIMIT: u32 = 10;
const HEALTH_TIMELINE_SLOT_MINUTES: i64 = 15;
const DASHBOARD_TIME_BUCKET_SLOTS: i64 = 7 * 24 * 4;
const HEALTH_TIMELINE_SLOTS: i64 = 24 * 4;
const HEALTH_TIMELINE_STABLE_SUCCESS_THRESHOLD: u64 = 3;
const FIVE_HOUR_WINDOW_SECONDS: u64 = 18_000;
const WEEK_WINDOW_SECONDS: u64 = 604_800;
const MONTH_WINDOW_SECONDS: u64 = 2_592_000;

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
    usage_records: Vec<DashboardUsageRecordData>,
    pool_summary: AccountPoolDiagnostics,
    capacity_info: AccountCapacityDiagnostics,
    rotation_strategy: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardHealthTimelineData {
    title: String,
    description: String,
    reliability_display: String,
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
    today_cost_usd: String,
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
    cache_hit_rate_value: f64,
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
    id: String,
    email: String,
    plan_type: Option<String>,
    tokens: String,
    quota_used_percent: Option<f64>,
    last_used: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardUsageRecordData {
    id: String,
    route: Option<String>,
    model: String,
    status_code: i64,
    transport: Option<String>,
    created_at_display: String,
    account_email: Option<String>,
    requested_model: Option<String>,
    upstream_model: Option<String>,
    client_ip: Option<String>,
    user_agent: Option<String>,
    reasoning_effort: Option<String>,
    token_details: UsageRecordTokenDetailsData,
    cost_details: Option<UsageRecordCostDetailsData>,
    first_token_latency_ms_display: String,
    latency_ms_display: String,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum QuotaWindowPriority {
    FiveHour,
    Weekly,
    Monthly,
    Other,
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
    let now = Utc::now();
    let today_filter = today_usage_record_filter(now);
    let usage_summary = state
        .services
        .usage_records
        .summary(UsageQueryFilter::default())
        .await
        .unwrap_or_else(|_| UsageRecordSummary::default());
    let account_usage_records = state
        .services
        .usage_records
        .account_usage(today_filter.clone(), DASHBOARD_ACCOUNT_USAGE_LIMIT)
        .await
        .unwrap_or_default();
    let recent_events = state
        .services
        .usage_records
        .list(None, DASHBOARD_USAGE_RECORD_LIMIT, today_filter)
        .await
        .map(|page| page.items)
        .unwrap_or_default();

    let time_buckets = dashboard_time_buckets(&state, now).await;
    let account_ids = accounts
        .iter()
        .map(|account| account.id.clone())
        .collect::<Vec<_>>();
    let refreshing_account_ids = state
        .services
        .token_refresh
        .refreshing_account_ids(&account_ids, now)
        .await
        .map_err(|error| AdminError::internal(error.to_string()))?;
    let pool_summary = account_pool_summary(&accounts, &refreshing_account_ids);
    let trend = dashboard_trend_data(&time_buckets, query.kind.unwrap_or_default());
    let quota_used_by_account =
        account_quota_used_percent_by_id(&state, &account_usage_records).await;
    let settings = state.services.settings.current();
    let recent_usage_records = recent_events;
    let account_emails = state
        .services
        .usage_records
        .account_email_map(&recent_usage_records)
        .await
        .map_err(|error| AdminError::usage_record_accounts_failed(error.to_string()))?;
    let dashboard_usage_records =
        dashboard_usage_record_items(recent_usage_records, &account_emails);

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DashboardSummaryData {
            cards: dashboard_cards(&accounts, &time_buckets, &usage_summary),
            trend,
            health_timeline: dashboard_health_timeline_data(&time_buckets),
            account_usage: account_usage_data(
                &accounts,
                &account_usage_records,
                &quota_used_by_account,
                now,
            ),
            service_statuses: service_status_data(&state),
            usage_records: dashboard_usage_records,
            pool_summary,
            capacity_info: AccountCapacityDiagnostics::from(capacity),
            rotation_strategy: Some(settings.rotation_strategy.clone()),
        }),
    ))
}

fn today_usage_record_filter(now: DateTime<Utc>) -> UsageQueryFilter {
    UsageQueryFilter {
        start_time: Some(china_day_start(now)),
        end_time: Some(now),
        ..UsageQueryFilter::default()
    }
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

mod data;
use data::*;
