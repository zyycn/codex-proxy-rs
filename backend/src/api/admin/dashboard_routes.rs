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
        pool::AccountCapacitySummary,
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
    api::AppState,
    infra::{
        format::{format_duration_ms, format_tokens},
        time::{
            china_datetime, china_datetime_rfc3339_str, china_day_start, china_quarter_hour_start,
            china_relative_time,
        },
    },
    telemetry::{
        account_usage::query::AccountUsageTimeBucket,
        dashboard::{
            dashboard_cards, dashboard_health_timeline_data, dashboard_trend_data,
            DashboardAccountCounts, DashboardCardsData, DashboardHealthTimelineData,
            DashboardTrendData, DashboardTrendKind,
        },
        usage::{
            query::UsageQueryFilter,
            store::UsageRecordAccountUsage,
            types::{metadata_string, UsageRecord},
        },
    },
    upstream::openai::fingerprint::Fingerprint,
};

const DASHBOARD_ACCOUNT_USAGE_LIMIT: u32 = 4;
const DASHBOARD_USAGE_RECORD_LIMIT: u32 = 10;
const DASHBOARD_TIME_BUCKET_MINUTES: i64 = 15;
const DASHBOARD_TIME_BUCKET_SLOTS: i64 = 7 * 24 * 4;
const FIVE_HOUR_WINDOW_SECONDS: u64 = 18_000;
const WEEK_WINDOW_SECONDS: u64 = 604_800;
const MONTH_WINDOW_SECONDS: u64 = 2_592_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DashboardTrendQuery {
    kind: Option<DashboardTrendKind>,
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

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountPoolDiagnostics {
    total: usize,
    active: usize,
    expired: usize,
    quota_exhausted: usize,
    refreshing: usize,
    disabled: usize,
    banned: usize,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountCapacityDiagnostics {
    max_concurrent_per_account: usize,
    total_slots: usize,
    used_slots: usize,
    available_slots: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FingerprintDiagnostics {
    source: &'static str,
    originator: String,
    app_version: String,
    build_number: String,
    platform: String,
    arch: String,
    chromium_version: String,
    user_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

fn fingerprint_diagnostics(fingerprint: &Fingerprint) -> FingerprintDiagnostics {
    FingerprintDiagnostics {
        source: "runtime",
        originator: fingerprint.originator.clone(),
        app_version: fingerprint.app_version.clone(),
        build_number: fingerprint.build_number.clone(),
        platform: fingerprint.platform.clone(),
        arch: fingerprint.arch.clone(),
        chromium_version: fingerprint.chromium_version.clone(),
        user_agent: fingerprint.user_agent(),
        updated_at: fingerprint.updated_at.clone(),
    }
}

impl From<AccountCapacitySummary> for AccountCapacityDiagnostics {
    fn from(summary: AccountCapacitySummary) -> Self {
        Self {
            max_concurrent_per_account: summary.max_concurrent_per_account,
            total_slots: summary.total_slots,
            used_slots: summary.used_slots,
            available_slots: summary.available_slots,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum QuotaWindowPriority {
    FiveHour,
    Weekly,
    Monthly,
    Other,
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
    let lifetime_usage = state.services.usage.summary().await.unwrap_or_default();
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
            cards: dashboard_cards(
                dashboard_account_counts(&accounts),
                &time_buckets,
                &lifetime_usage,
            ),
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
