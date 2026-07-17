//! Dashboard 跨域查询与账号/quota join。

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use crate::{
    admin_queries::accounts::RefreshActivityQuery,
    fleet::{
        account::{Account, AccountStatus},
        pool::{AccountCapacitySummary, AccountPoolService},
        quota::QuotaSnapshot,
        refresh::token_refresh_status_eligible,
        store::AccountStore,
    },
    infra::time::{china_day_start, china_quarter_hour_start},
    settings::service::SettingsService,
    telemetry::{
        account_usage::query::{
            AccountUsageQueryService, AccountUsageTimeBucket, RetainedUsageSummary,
        },
        usage::{
            insights::RequestHealthTimeBucket,
            query::{UsageQueryFilter, UsageQueryService},
            types::UsageRecord,
        },
    },
};

const ACCOUNT_USAGE_LIMIT: u32 = 4;
const USAGE_RECORD_LIMIT: u32 = 10;
const TIME_BUCKET_MINUTES: i64 = 15;
const TIME_BUCKET_SLOTS: i64 = 7 * 24 * 4;

/// Dashboard 消费的请求画像快照端口。
pub trait DashboardWireProfileQuery: Send + Sync {
    fn snapshot(&self) -> DashboardWireProfile;
}

/// Dashboard 消费的 Desktop 发布观测端口。
pub trait DashboardDesktopReleaseQuery: Send + Sync {
    fn snapshot(&self) -> DashboardDesktopReleaseSnapshot;
}

#[derive(Debug, Clone)]
pub struct DashboardWireProfile {
    pub originator: String,
    pub codex_version: String,
    pub desktop_version: String,
    pub desktop_build: String,
    pub os_type: String,
    pub os_version: String,
    pub arch: String,
    pub terminal: String,
    pub user_agent: String,
    pub verified_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct DashboardDesktopReleaseSnapshot {
    pub checked_at: Option<DateTime<Utc>>,
    pub latest: Option<DashboardDesktopRelease>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DashboardDesktopRelease {
    pub version: String,
    pub build: String,
    pub published_at: Option<DateTime<Utc>>,
    pub minimum_system_version: Option<String>,
    pub hardware_requirements: Option<String>,
    pub download_url: Option<String>,
    pub download_size: Option<u64>,
    pub signature_present: bool,
}

#[derive(Debug, Clone)]
pub struct DashboardReadModel {
    pub account_counts: DashboardAccountCounts,
    pub retained_usage: RetainedUsageSummary,
    pub time_buckets: Vec<AccountUsageTimeBucket>,
    pub health_buckets: Vec<RequestHealthTimeBucket>,
    pub account_usage: Vec<DashboardAccountUsage>,
    pub usage_records: Vec<UsageRecord>,
    pub account_emails: HashMap<String, String>,
    pub pool: DashboardPoolSummary,
    pub capacity: AccountCapacitySummary,
    pub rotation_strategy: String,
    pub wire_profile: DashboardWireProfile,
    pub desktop_release: DashboardDesktopReleaseSnapshot,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DashboardAccountCounts {
    pub total: u64,
    pub enabled: u64,
    pub abnormal: u64,
}

#[derive(Debug, Clone)]
pub struct DashboardAccountUsage {
    pub id: String,
    pub email: String,
    pub plan_type: Option<String>,
    pub total_tokens: u64,
    pub quota_used_percent: Option<f64>,
    pub last_used_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DashboardPoolSummary {
    pub total: usize,
    pub active: usize,
    pub expired: usize,
    pub quota_exhausted: usize,
    pub refreshing: usize,
    pub disabled: usize,
    pub banned: usize,
}

#[derive(Debug, Error)]
pub enum DashboardQueryError {
    #[error("failed to load dashboard {0}")]
    Data(&'static str),
}

#[derive(Clone)]
pub struct DashboardQueryService {
    accounts: Arc<dyn AccountStore>,
    usage: Arc<AccountUsageQueryService>,
    usage_records: Arc<UsageQueryService>,
    account_pool: Arc<AccountPoolService>,
    refresh_activity: Arc<dyn RefreshActivityQuery>,
    settings: Arc<SettingsService>,
    wire_profile: Arc<dyn DashboardWireProfileQuery>,
    desktop_release: Arc<dyn DashboardDesktopReleaseQuery>,
}

pub struct DashboardQueryServiceParts {
    pub accounts: Arc<dyn AccountStore>,
    pub usage: Arc<AccountUsageQueryService>,
    pub usage_records: Arc<UsageQueryService>,
    pub account_pool: Arc<AccountPoolService>,
    pub refresh_activity: Arc<dyn RefreshActivityQuery>,
    pub settings: Arc<SettingsService>,
    pub wire_profile: Arc<dyn DashboardWireProfileQuery>,
    pub desktop_release: Arc<dyn DashboardDesktopReleaseQuery>,
}

impl DashboardQueryService {
    pub fn new(parts: DashboardQueryServiceParts) -> Self {
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

    pub async fn summary(
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

    pub async fn time_buckets(
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

fn account_counts(accounts: &[Account]) -> DashboardAccountCounts {
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
