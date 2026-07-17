//! 管理端 Dashboard 聚合视图。

use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    api::AppState,
    api::admin::{
        accounts_routes::query::RefreshActivityQuery,
        dashboard_presenter::{
            DashboardAccountCounts, DashboardCardsData, DashboardHealthTimelineData,
            DashboardTrendData, DashboardTrendKind, dashboard_cards,
            dashboard_health_timeline_data, dashboard_trend_data,
        },
        response::{AdminEnvelope, AdminError, AdminResponse},
        session::AdminAuth,
        usage_routes::{
            UsageRecordBillingData, UsageRecordTokenDetailsData, usage_billing,
            usage_record_models, usage_token_details,
        },
    },
    fleet::{
        account::{Account, AccountStatus},
        pool::{AccountCapacitySummary, AccountPoolService},
        quota::QuotaSnapshot,
        refresh::token_refresh_status_eligible,
        store::AccountStore,
    },
    infra::{
        format::{format_duration_ms, format_tokens},
        time::{china_datetime, china_day_start, china_quarter_hour_start, china_relative_time},
    },
    settings::service::SettingsService,
    telemetry::{
        account_usage::query::{
            AccountUsageQueryService, AccountUsageTimeBucket, RetainedUsageSummary,
        },
        usage::{
            insights::RequestHealthTimeBucket,
            query::{UsageQueryFilter, UsageQueryService},
            types::{UsageRecord, metadata_i64, metadata_string},
        },
    },
};

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
    first_token_latency_ms: Option<i64>,
    latency_ms: Option<i64>,
    latency_details: DashboardUsageLatencyDetailsData,
    first_token_latency_ms_display: String,
    latency_ms_display: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardUsageLatencyDetailsData {
    first_reasoning_ms: Option<i64>,
    first_text_ms: Option<i64>,
    transport_decision_wait_ms: Option<i64>,
    ws_connect_ms: Option<i64>,
    upstream_headers_ms: Option<i64>,
    first_event_ms: Option<i64>,
    openai_processing_ms: Option<i64>,
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

impl From<DashboardPoolSummary> for AccountPoolDiagnostics {
    fn from(summary: DashboardPoolSummary) -> Self {
        Self {
            total: summary.total,
            active: summary.active,
            expired: summary.expired,
            quota_exhausted: summary.quota_exhausted,
            refreshing: summary.refreshing,
            disabled: summary.disabled,
            banned: summary.banned,
        }
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

/// `GET /api/admin/dashboard/summary`
pub(crate) async fn dashboard_summary(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<DashboardTrendQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let now = Utc::now();
    let dashboard = state
        .services
        .dashboard
        .summary(now)
        .await
        .map_err(dashboard_query_error)?;
    let trend = dashboard_trend_data(&dashboard.time_buckets, query.kind.unwrap_or_default());
    let dashboard_usage_records =
        dashboard_usage_record_items(dashboard.usage_records, &dashboard.account_emails);

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DashboardSummaryData {
            cards: dashboard_cards(
                DashboardAccountCounts {
                    total: dashboard.account_counts.total,
                    enabled: dashboard.account_counts.enabled,
                    abnormal: dashboard.account_counts.abnormal,
                },
                &dashboard.time_buckets,
                &dashboard.retained_usage,
            ),
            trend,
            health_timeline: dashboard_health_timeline_data(&dashboard.health_buckets),
            account_usage: account_usage_data(&dashboard.account_usage, now),
            wire_profile: wire_profile_data(dashboard.wire_profile, dashboard.desktop_release),
            usage_records: dashboard_usage_records,
            pool_summary: AccountPoolDiagnostics::from(dashboard.pool),
            capacity_info: AccountCapacityDiagnostics::from(dashboard.capacity),
            rotation_strategy: Some(dashboard.rotation_strategy),
        }),
    ))
}

/// `GET /api/admin/dashboard/trend?kind=usage|latency|errors`
pub(crate) async fn dashboard_trend(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<DashboardTrendQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let time_buckets = state
        .services
        .dashboard
        .time_buckets(Utc::now())
        .await
        .map_err(dashboard_query_error)?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(dashboard_trend_data(
            &time_buckets,
            query.kind.unwrap_or_default(),
        )),
    ))
}

fn account_usage_data(
    usage_records: &[DashboardAccountUsage],
    now: DateTime<Utc>,
) -> Vec<DashboardAccountUsageData> {
    usage_records
        .iter()
        .map(|usage| DashboardAccountUsageData {
            id: usage.id.clone(),
            email: usage.email.clone(),
            plan_type: usage.plan_type.clone(),
            tokens: format_tokens(usage.total_tokens),
            quota_used_percent: usage.quota_used_percent,
            last_used: china_relative_time(Some(usage.last_used_at), now),
        })
        .collect()
}

fn dashboard_query_error(error: DashboardQueryError) -> AdminError {
    tracing::error!(error = %error, "Failed to load dashboard data");
    AdminError::internal("Failed to load dashboard data")
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
    let latency_details = dashboard_usage_latency_details(&record.metadata);

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
        first_token_latency_ms,
        latency_ms: record.latency_ms,
        latency_details,
        first_token_latency_ms_display: format_duration_ms(first_token_latency_ms),
        latency_ms_display: format_duration_ms(record.latency_ms),
    }
}

fn dashboard_usage_latency_details(metadata: &Value) -> DashboardUsageLatencyDetailsData {
    DashboardUsageLatencyDetailsData {
        first_reasoning_ms: metadata_duration_ms(metadata, "firstReasoningMs"),
        first_text_ms: metadata_duration_ms(metadata, "firstTextMs"),
        transport_decision_wait_ms: metadata_duration_ms(metadata, "transportDecisionWaitMs"),
        ws_connect_ms: metadata_duration_ms(metadata, "wsConnectMs"),
        upstream_headers_ms: metadata_duration_ms(metadata, "upstreamHeadersMs"),
        first_event_ms: metadata_duration_ms(metadata, "firstEventMs"),
        openai_processing_ms: metadata_duration_ms(metadata, "openaiProcessingMs"),
    }
}

fn metadata_duration_ms(metadata: &Value, key: &str) -> Option<i64> {
    metadata_i64(metadata, &[key]).filter(|value| *value >= 0)
}

fn wire_profile_data(
    profile: DashboardWireProfile,
    release_snapshot: DashboardDesktopReleaseSnapshot,
) -> DashboardWireProfileData {
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
        user_agent: profile.user_agent,
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

const ACCOUNT_USAGE_LIMIT: u32 = 4;
const USAGE_RECORD_LIMIT: u32 = 10;
const TIME_BUCKET_MINUTES: i64 = 15;
const TIME_BUCKET_SLOTS: i64 = 7 * 24 * 4;

/// Dashboard 消费的请求画像快照端口。
pub(crate) trait DashboardWireProfileQuery: Send + Sync {
    fn snapshot(&self) -> DashboardWireProfile;
}

/// Dashboard 消费的 Desktop 发布观测端口。
pub(crate) trait DashboardDesktopReleaseQuery: Send + Sync {
    fn snapshot(&self) -> DashboardDesktopReleaseSnapshot;
}

#[derive(Debug, Clone)]
pub(crate) struct DashboardWireProfile {
    pub(crate) originator: String,
    pub(crate) codex_version: String,
    pub(crate) desktop_version: String,
    pub(crate) desktop_build: String,
    pub(crate) os_type: String,
    pub(crate) os_version: String,
    pub(crate) arch: String,
    pub(crate) terminal: String,
    pub(crate) user_agent: String,
    pub(crate) verified_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DashboardDesktopReleaseSnapshot {
    pub(crate) checked_at: Option<DateTime<Utc>>,
    pub(crate) latest: Option<DashboardDesktopRelease>,
    pub(crate) last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DashboardDesktopRelease {
    pub(crate) version: String,
    pub(crate) build: String,
    pub(crate) published_at: Option<DateTime<Utc>>,
    pub(crate) minimum_system_version: Option<String>,
    pub(crate) hardware_requirements: Option<String>,
    pub(crate) download_url: Option<String>,
    pub(crate) download_size: Option<u64>,
    pub(crate) signature_present: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DashboardReadModel {
    pub(crate) account_counts: DashboardQueryAccountCounts,
    pub(crate) retained_usage: RetainedUsageSummary,
    pub(crate) time_buckets: Vec<AccountUsageTimeBucket>,
    pub(crate) health_buckets: Vec<RequestHealthTimeBucket>,
    pub(crate) account_usage: Vec<DashboardAccountUsage>,
    pub(crate) usage_records: Vec<UsageRecord>,
    pub(crate) account_emails: HashMap<String, String>,
    pub(crate) pool: DashboardPoolSummary,
    pub(crate) capacity: AccountCapacitySummary,
    pub(crate) rotation_strategy: String,
    pub(crate) wire_profile: DashboardWireProfile,
    pub(crate) desktop_release: DashboardDesktopReleaseSnapshot,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DashboardQueryAccountCounts {
    pub(crate) total: u64,
    pub(crate) enabled: u64,
    pub(crate) abnormal: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct DashboardAccountUsage {
    pub(crate) id: String,
    pub(crate) email: String,
    pub(crate) plan_type: Option<String>,
    pub(crate) total_tokens: u64,
    pub(crate) quota_used_percent: Option<f64>,
    pub(crate) last_used_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DashboardPoolSummary {
    pub(crate) total: usize,
    pub(crate) active: usize,
    pub(crate) expired: usize,
    pub(crate) quota_exhausted: usize,
    pub(crate) refreshing: usize,
    pub(crate) disabled: usize,
    pub(crate) banned: usize,
}

#[derive(Debug, Error)]
pub(crate) enum DashboardQueryError {
    #[error("failed to load dashboard {0}")]
    Data(&'static str),
}

#[derive(Clone)]
pub(crate) struct DashboardQueryService {
    accounts: Arc<dyn AccountStore>,
    usage: Arc<AccountUsageQueryService>,
    usage_records: Arc<UsageQueryService>,
    account_pool: Arc<AccountPoolService>,
    refresh_activity: Arc<dyn RefreshActivityQuery>,
    settings: Arc<SettingsService>,
    wire_profile: Arc<dyn DashboardWireProfileQuery>,
    desktop_release: Arc<dyn DashboardDesktopReleaseQuery>,
}

pub(crate) struct DashboardQueryServiceParts {
    pub(crate) accounts: Arc<dyn AccountStore>,
    pub(crate) usage: Arc<AccountUsageQueryService>,
    pub(crate) usage_records: Arc<UsageQueryService>,
    pub(crate) account_pool: Arc<AccountPoolService>,
    pub(crate) refresh_activity: Arc<dyn RefreshActivityQuery>,
    pub(crate) settings: Arc<SettingsService>,
    pub(crate) wire_profile: Arc<dyn DashboardWireProfileQuery>,
    pub(crate) desktop_release: Arc<dyn DashboardDesktopReleaseQuery>,
}

impl DashboardQueryService {
    pub(crate) fn new(parts: DashboardQueryServiceParts) -> Self {
        Self {
            accounts: parts.accounts,
            usage: parts.usage,
            usage_records: parts.usage_records,
            account_pool: parts.account_pool,
            refresh_activity: parts.refresh_activity,
            settings: parts.settings,
            wire_profile: parts.wire_profile,
            desktop_release: parts.desktop_release,
        }
    }

    pub(crate) async fn summary(
        &self,
        now: DateTime<Utc>,
    ) -> Result<DashboardReadModel, DashboardQueryError> {
        let accounts = self
            .accounts
            .list_pool_accounts()
            .await
            .map_err(|_| DashboardQueryError::Data("accounts"))?;
        let capacity = self.account_pool.capacity_summary_now().await;
        let filter = UsageQueryFilter {
            start_time: Some(china_day_start(now)),
            end_time: Some(now),
            ..UsageQueryFilter::default()
        };
        let retained_usage = self
            .usage
            .retained_summary()
            .await
            .map_err(|_| DashboardQueryError::Data("retained usage summary"))?;
        let account_usage_records = self
            .usage_records
            .account_usage(filter.clone(), ACCOUNT_USAGE_LIMIT)
            .await
            .map_err(|_| DashboardQueryError::Data("account usage ranking"))?;
        let usage_records = self
            .usage_records
            .list_recent(USAGE_RECORD_LIMIT, filter)
            .await
            .map_err(|_| DashboardQueryError::Data("recent usage records"))?;
        let time_buckets = self.time_buckets(now).await?;
        let health_buckets = self.health_buckets(now).await?;
        let account_ids = accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let refreshing = self
            .refresh_activity
            .refreshing_account_ids(&account_ids, now)
            .await
            .map_err(|_| DashboardQueryError::Data("refreshing accounts"))?;
        let quota_used = self.quota_used_by_account(&account_usage_records).await?;
        let account_emails = self
            .usage_records
            .account_email_map(&usage_records)
            .await
            .map_err(|_| DashboardQueryError::Data("usage record accounts"))?;

        Ok(DashboardReadModel {
            account_counts: account_counts(&accounts),
            retained_usage,
            time_buckets,
            health_buckets,
            account_usage: account_usage(&accounts, &account_usage_records, &quota_used),
            usage_records,
            account_emails,
            pool: pool_summary(&accounts, &refreshing),
            capacity,
            rotation_strategy: self.settings.current().rotation_strategy,
            wire_profile: self.wire_profile.snapshot(),
            desktop_release: self.desktop_release.snapshot(),
        })
    }

    pub(crate) async fn time_buckets(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<AccountUsageTimeBucket>, DashboardQueryError> {
        let current_slot = china_quarter_hour_start(now);
        let start = current_slot - Duration::minutes(TIME_BUCKET_MINUTES * (TIME_BUCKET_SLOTS - 1));
        self.usage
            .time_buckets(start, now)
            .await
            .map_err(|_| DashboardQueryError::Data("time buckets"))
    }

    async fn health_buckets(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<RequestHealthTimeBucket>, DashboardQueryError> {
        self.usage_records
            .health_timeline(china_day_start(now), now)
            .await
            .map_err(|_| DashboardQueryError::Data("health timeline"))
    }

    async fn quota_used_by_account(
        &self,
        usage: &[crate::telemetry::usage::query::UsageRecordAccountUsage],
    ) -> Result<HashMap<String, f64>, DashboardQueryError> {
        let mut quota_used = HashMap::with_capacity(usage.len());
        for usage in usage {
            let quota_json = self
                .accounts
                .get_quota_json(&usage.account_id)
                .await
                .map_err(|_| DashboardQueryError::Data("account quota"))?;
            let Some(quota) = quota_json
                .as_deref()
                .and_then(|value| QuotaSnapshot::from_json(value).ok())
            else {
                continue;
            };
            if let Some(used_percent) = quota.representative_used_percent() {
                quota_used.insert(usage.account_id.clone(), used_percent);
            }
        }
        Ok(quota_used)
    }
}

fn account_counts(accounts: &[Account]) -> DashboardQueryAccountCounts {
    DashboardQueryAccountCounts {
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

fn pool_summary(
    accounts: &[Account],
    refreshing: &std::collections::HashSet<String>,
) -> DashboardPoolSummary {
    let mut summary = DashboardPoolSummary {
        total: accounts.len(),
        ..DashboardPoolSummary::default()
    };
    for account in accounts {
        match account.status {
            status if token_refresh_status_eligible(status) && refreshing.contains(&account.id) => {
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

fn account_usage(
    accounts: &[Account],
    usage_records: &[crate::telemetry::usage::query::UsageRecordAccountUsage],
    quota_used: &HashMap<String, f64>,
) -> Vec<DashboardAccountUsage> {
    let accounts = accounts
        .iter()
        .map(|account| (account.id.as_str(), account))
        .collect::<HashMap<_, _>>();
    usage_records
        .iter()
        .map(|usage| {
            let account = accounts.get(usage.account_id.as_str()).copied();
            DashboardAccountUsage {
                id: usage.account_id.clone(),
                email: account
                    .and_then(|account| account.email.clone())
                    .unwrap_or_else(|| usage.account_id.clone()),
                plan_type: account.and_then(|account| account.plan_type.clone()),
                total_tokens: usage.total_tokens,
                quota_used_percent: quota_used.get(&usage.account_id).copied().or_else(|| {
                    matches!(
                        account.map(|account| account.status),
                        Some(AccountStatus::QuotaExhausted)
                    )
                    .then_some(100.0)
                }),
                last_used_at: usage.last_used_at,
            }
        })
        .collect()
}
