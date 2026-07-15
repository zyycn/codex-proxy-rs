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
    api::AppState,
    api::admin::{
        response::{AdminEnvelope, AdminError, AdminResponse},
        session::AdminAuth,
        usage_routes::{
            UsageRecordBillingData, UsageRecordTokenDetailsData, usage_billing,
            usage_record_models, usage_token_details,
        },
    },
    fleet::{
        account::{Account, AccountStatus},
        pool::AccountCapacitySummary,
        refresh::token_refresh_status_eligible,
    },
    infra::{
        format::{format_duration_ms, format_tokens},
        time::{china_datetime, china_day_start, china_quarter_hour_start, china_relative_time},
    },
    telemetry::{
        account_usage::query::AccountUsageTimeBucket,
        dashboard::{
            DashboardAccountCounts, DashboardCardsData, DashboardHealthTimelineData,
            DashboardTrendData, DashboardTrendKind, dashboard_cards,
            dashboard_health_timeline_data, dashboard_trend_data,
        },
        usage::{
            insights::RequestHealthTimeBucket,
            query::{UsageQueryFilter, UsageRecordAccountUsage},
            types::{UsageRecord, metadata_string},
        },
    },
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
    wire_profile: DashboardWireProfileData,
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
    reasoning_preset: Option<String>,
    compact: bool,
    request_kind: Option<String>,
    subagent_kind: Option<String>,
    token_details: UsageRecordTokenDetailsData,
    billing: Option<UsageRecordBillingData>,
    first_token_latency_ms_display: String,
    latency_ms_display: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardWireProfileData {
    originator: String,
    codex_version: String,
    desktop_version: String,
    desktop_build: String,
    target: DashboardWireTargetData,
    user_agent: String,
    verified_at: DateTime<Utc>,
    release: DashboardDesktopReleaseData,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardWireTargetData {
    os_type: String,
    os_version: String,
    arch: String,
    terminal: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardDesktopReleaseData {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    checked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum_system_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hardware_requirements: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    download_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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
        .map_err(|error| dashboard_data_error("accounts", &error))?;
    let capacity = state.services.account_pool.capacity_summary_now().await;
    let now = Utc::now();
    let today_filter = today_usage_record_filter(now);
    let retained_usage = state
        .services
        .usage
        .retained_summary()
        .await
        .map_err(|error| dashboard_data_error("retained usage summary", &error))?;
    let account_usage_records = state
        .services
        .usage_records
        .account_usage(today_filter.clone(), DASHBOARD_ACCOUNT_USAGE_LIMIT)
        .await
        .map_err(|error| dashboard_data_error("account usage ranking", &error))?;
    let recent_events = state
        .services
        .usage_records
        .list_recent(DASHBOARD_USAGE_RECORD_LIMIT, today_filter)
        .await
        .map_err(|error| dashboard_data_error("recent usage records", &error))?;

    let (time_buckets, health_buckets) = tokio::try_join!(
        dashboard_time_buckets(&state, now),
        dashboard_health_buckets(&state, now),
    )?;
    let account_ids = accounts
        .iter()
        .map(|account| account.id.clone())
        .collect::<Vec<_>>();
    let refreshing_account_ids = state
        .services
        .token_refresh
        .refreshing_account_ids(&account_ids, now)
        .await
        .map_err(|error| dashboard_data_error("refreshing accounts", &error))?;
    let pool_summary = account_pool_summary(&accounts, &refreshing_account_ids);
    let trend = dashboard_trend_data(&time_buckets, query.kind.unwrap_or_default());
    let quota_used_by_account =
        account_quota_used_percent_by_id(&state, &account_usage_records).await?;
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
                &retained_usage,
            ),
            trend,
            health_timeline: dashboard_health_timeline_data(&health_buckets),
            account_usage: account_usage_data(
                &accounts,
                &account_usage_records,
                &quota_used_by_account,
                now,
            ),
            wire_profile: wire_profile_data(&state),
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
    let time_buckets = dashboard_time_buckets(&state, Utc::now()).await?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(dashboard_trend_data(
            &time_buckets,
            query.kind.unwrap_or_default(),
        )),
    ))
}

async fn dashboard_time_buckets(
    state: &AppState,
    now: DateTime<Utc>,
) -> Result<Vec<AccountUsageTimeBucket>, AdminError> {
    let current_slot = china_quarter_hour_start(now);
    let start = current_slot
        - Duration::minutes(DASHBOARD_TIME_BUCKET_MINUTES * (DASHBOARD_TIME_BUCKET_SLOTS - 1));
    state
        .services
        .usage
        .time_buckets(start, now)
        .await
        .map_err(|error| dashboard_data_error("time buckets", &error))
}

async fn dashboard_health_buckets(
    state: &AppState,
    now: DateTime<Utc>,
) -> Result<Vec<RequestHealthTimeBucket>, AdminError> {
    state
        .services
        .usage_records
        .health_timeline(china_day_start(now), now)
        .await
        .map_err(|error| dashboard_data_error("health timeline", &error))
}

fn dashboard_account_counts(accounts: &[Account]) -> DashboardAccountCounts {
    DashboardAccountCounts {
        total: accounts.len() as u64,
        enabled: accounts
            .iter()
            .filter(|account| account.status == AccountStatus::Active)
            .count() as u64,
        abnormal: accounts
            .iter()
            .filter(|account| account.status != AccountStatus::Active)
            .count() as u64,
    }
}

fn account_pool_summary(
    accounts: &[Account],
    refreshing_account_ids: &HashSet<String>,
) -> AccountPoolDiagnostics {
    let mut summary = AccountPoolDiagnostics {
        total: accounts.len(),
        ..AccountPoolDiagnostics::default()
    };
    for account in accounts {
        match account.status {
            status
                if token_refresh_status_eligible(status)
                    && refreshing_account_ids.contains(&account.id) =>
            {
                summary.refreshing += 1;
            }
            AccountStatus::Active => summary.active += 1,
            AccountStatus::Expired => summary.expired += 1,
            AccountStatus::QuotaExhausted => summary.quota_exhausted += 1,
            AccountStatus::Disabled => summary.disabled += 1,
            AccountStatus::Banned => summary.banned += 1,
        }
    }
    summary
}

fn account_usage_data(
    accounts: &[Account],
    usage_records: &[UsageRecordAccountUsage],
    quota_used_by_account: &HashMap<String, f64>,
    now: DateTime<Utc>,
) -> Vec<DashboardAccountUsageData> {
    let account_by_id = accounts
        .iter()
        .map(|account| (account.id.as_str(), account))
        .collect::<HashMap<_, _>>();
    usage_records
        .iter()
        .map(|usage| {
            let account = account_by_id.get(usage.account_id.as_str()).copied();
            let quota_used_percent = quota_used_by_account
                .get(&usage.account_id)
                .copied()
                .or_else(|| {
                    matches!(
                        account.map(|account| account.status),
                        Some(AccountStatus::QuotaExhausted)
                    )
                    .then_some(100.0)
                });
            DashboardAccountUsageData {
                id: usage.account_id.clone(),
                email: account
                    .and_then(|account| account.email.as_ref())
                    .cloned()
                    .unwrap_or_else(|| usage.account_id.clone()),
                plan_type: account
                    .and_then(|account| account.plan_type.as_ref())
                    .cloned(),
                tokens: format_tokens(usage.total_tokens),
                quota_used_percent,
                last_used: china_relative_time(Some(usage.last_used_at), now),
            }
        })
        .collect()
}

async fn account_quota_used_percent_by_id(
    state: &AppState,
    usage_records: &[UsageRecordAccountUsage],
) -> Result<HashMap<String, f64>, AdminError> {
    let mut quota_used_by_account = HashMap::with_capacity(usage_records.len());
    for usage in usage_records {
        let quota_json = state
            .services
            .accounts
            .get_quota_json(&usage.account_id)
            .await
            .map_err(|error| dashboard_data_error("account quota", &error))?;
        let Some(quota_json) = quota_json else {
            continue;
        };
        if let Some(used_percent) = quota_used_percent(&quota_json) {
            quota_used_by_account.insert(usage.account_id.clone(), used_percent);
        }
    }
    Ok(quota_used_by_account)
}

fn dashboard_data_error(source: &'static str, error: &impl std::fmt::Display) -> AdminError {
    tracing::error!(source, error = %error, "Failed to load dashboard data");
    AdminError::internal("Failed to load dashboard data")
}

fn quota_used_percent(quota_json: &str) -> Option<f64> {
    let quota = serde_json::from_str::<Value>(quota_json).ok()?;
    let mut selected = None;
    if let Some(monthly_limit) = quota.get("monthly_limit") {
        select_quota_used_percent(
            &mut selected,
            quota_window_priority(monthly_limit, QuotaWindowPriority::Monthly),
            monthly_limit.get("used_percent").and_then(percent_value),
        );
    }
    if let Some(snapshots) = quota.get("snapshots").and_then(Value::as_array) {
        for snapshot in snapshots {
            for role in ["primary", "secondary"] {
                if let Some(window) = snapshot.get(role) {
                    select_quota_used_percent(
                        &mut selected,
                        quota_window_priority(window, QuotaWindowPriority::Other),
                        window.get("used_percent").and_then(percent_value),
                    );
                }
            }
        }
    }
    selected.map(|(_, used_percent)| used_percent)
}

fn select_quota_used_percent(
    selected: &mut Option<(QuotaWindowPriority, f64)>,
    priority: QuotaWindowPriority,
    used_percent: Option<f64>,
) {
    let Some(used_percent) = used_percent else {
        return;
    };
    match selected {
        Some((selected_priority, selected_percent))
            if priority > *selected_priority
                || (priority == *selected_priority && used_percent <= *selected_percent) => {}
        _ => *selected = Some((priority, used_percent)),
    }
}

fn quota_window_priority(window: &Value, fallback: QuotaWindowPriority) -> QuotaWindowPriority {
    let window_seconds = window
        .get("window_minutes")
        .and_then(Value::as_u64)
        .and_then(|minutes| minutes.checked_mul(60));
    match window_seconds {
        Some(seconds) if quota_window_matches(seconds, FIVE_HOUR_WINDOW_SECONDS) => {
            QuotaWindowPriority::FiveHour
        }
        Some(seconds) if quota_window_matches(seconds, WEEK_WINDOW_SECONDS) => {
            QuotaWindowPriority::Weekly
        }
        Some(seconds) if quota_window_matches(seconds, MONTH_WINDOW_SECONDS) => {
            QuotaWindowPriority::Monthly
        }
        _ => fallback,
    }
}

fn quota_window_matches(actual: u64, expected: u64) -> bool {
    actual > 0 && actual.abs_diff(expected) <= expected / 20
}

fn percent_value(value: &Value) -> Option<f64> {
    let percent = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))?;
    percent.is_finite().then_some(percent.clamp(0.0, 100.0))
}

fn dashboard_usage_record_items(
    items: Vec<UsageRecord>,
    account_emails: &HashMap<String, String>,
) -> Vec<DashboardUsageRecordData> {
    items
        .into_iter()
        .map(|record| dashboard_usage_record_data(record, account_emails))
        .collect()
}

fn dashboard_usage_record_data(
    record: UsageRecord,
    account_emails: &HashMap<String, String>,
) -> DashboardUsageRecordData {
    let account_email = account_emails
        .get(&record.account_id)
        .cloned()
        .or_else(|| metadata_string(&record.metadata, &["accountEmail", "account_email"]));
    let (requested_model, upstream_model) = usage_record_models(&record);
    let client_ip = metadata_string(&record.metadata, &["clientIp", "ipAddress", "ip_address"]);
    let user_agent = metadata_string(&record.metadata, &["userAgent", "user_agent"]);
    let reasoning_effort =
        metadata_string(&record.metadata, &["reasoningEffort", "reasoning_effort"]);
    let reasoning_preset = metadata_string(&record.metadata, &["reasoningPreset"]);
    let compact = record
        .metadata
        .get("compact")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_kind = metadata_string(&record.metadata, &["requestKind", "request_kind"]);
    let subagent_kind = metadata_string(&record.metadata, &["subagentKind", "subagent_kind"]);
    let token_details = usage_token_details(&record);
    let billing = usage_billing(&record, upstream_model.as_deref(), &token_details);
    let first_token_latency_ms = record.first_token_ms;

    DashboardUsageRecordData {
        id: record.id,
        route: record.route,
        model: record.model,
        status_code: record.status_code,
        transport: record.transport,
        created_at_display: china_datetime(&record.created_at),
        account_email,
        requested_model,
        upstream_model,
        client_ip,
        user_agent,
        reasoning_effort,
        reasoning_preset,
        compact,
        request_kind,
        subagent_kind,
        token_details,
        billing,
        first_token_latency_ms_display: format_duration_ms(first_token_latency_ms),
        latency_ms_display: format_duration_ms(record.latency_ms),
    }
}

fn wire_profile_data(state: &AppState) -> DashboardWireProfileData {
    let profile = &state.services.wire_profile;
    let release_snapshot = state.services.desktop_release.snapshot();
    let release_status = if release_snapshot.last_error.is_some() {
        "check_failed"
    } else if let Some(latest) = &release_snapshot.latest {
        if latest.version == profile.desktop_version && latest.build == profile.desktop_build {
            "aligned"
        } else {
            "review_required"
        }
    } else {
        "unchecked"
    };
    let latest = release_snapshot.latest;

    DashboardWireProfileData {
        originator: profile.originator.clone(),
        codex_version: profile.codex_version.clone(),
        desktop_version: profile.desktop_version.clone(),
        desktop_build: profile.desktop_build.clone(),
        target: DashboardWireTargetData {
            os_type: profile.os_type.clone(),
            os_version: profile.os_version.clone(),
            arch: profile.arch.clone(),
            terminal: profile.terminal.clone(),
        },
        user_agent: profile.user_agent(),
        verified_at: profile.verified_at,
        release: DashboardDesktopReleaseData {
            status: release_status,
            checked_at: release_snapshot.checked_at,
            latest_version: latest.as_ref().map(|release| release.version.clone()),
            latest_build: latest.as_ref().map(|release| release.build.clone()),
            published_at: latest.as_ref().and_then(|release| release.published_at),
            minimum_system_version: latest
                .as_ref()
                .and_then(|release| release.minimum_system_version.clone()),
            hardware_requirements: latest
                .as_ref()
                .and_then(|release| release.hardware_requirements.clone()),
            download_url: latest
                .as_ref()
                .and_then(|release| release.download_url.clone()),
            download_size: latest.as_ref().and_then(|release| release.download_size),
            signature_present: latest.as_ref().map(|release| release.signature_present),
            error: release_snapshot.last_error,
        },
    }
}
