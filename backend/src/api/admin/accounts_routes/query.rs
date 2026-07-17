use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use super::*;

use crate::{
    fleet::{
        manage::AccountManageService,
        quota::{ClassifiedQuotaWindow, QuotaSnapshot},
        refresh::{TokenRefreshService, TokenRefresher, token_refresh_status_eligible},
    },
    infra::json::NumberedPage,
    telemetry::{
        account_usage::query::AccountUsageQueryService,
        billing,
        buckets::query::{ModelUsageWindow, UsageBucketWindow},
    },
};

const ACCOUNT_STATS_PAGE_LIMIT: u32 = 200;

#[derive(Debug, Clone)]
pub(crate) struct AccountListQuery {
    pub(crate) page: u32,
    pub(crate) page_size: u32,
    pub(crate) search: Option<String>,
    pub(crate) status: Option<AccountStatus>,
    pub(crate) sort: Option<AccountListSort>,
}

#[derive(Debug, Clone)]
pub(crate) struct AccountListResult {
    pub(crate) page: NumberedPage<AccountListItem>,
    pub(crate) summary: AccountSummary,
}

#[derive(Debug, Clone)]
pub(crate) struct AccountListItem {
    pub(crate) account: ManagedAccount,
    pub(crate) usage: Option<AccountUsageRecord>,
    pub(crate) quota: Option<AccountQuotaReadModel>,
    pub(crate) models: Vec<AccountModelUsage>,
    pub(crate) token_refreshing: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AccountSummary {
    pub(crate) total: u64,
    pub(crate) active: u64,
    pub(crate) quota_exhausted: u64,
    pub(crate) attention: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct AccountQuotaReadModel {
    pub(crate) fetched_at: Option<DateTime<Utc>>,
    pub(crate) windows: Vec<AccountQuotaWindowReadModel>,
}

#[derive(Debug, Clone)]
pub(crate) struct AccountQuotaWindowReadModel {
    pub(crate) quota: ClassifiedQuotaWindow,
    pub(crate) local_usage: Option<AccountQuotaWindowLocalUsage>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AccountQuotaWindowLocalUsage {
    pub(crate) request_count: i64,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cached_tokens: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct AccountQuotaUsageWindow {
    pub(crate) key: String,
    pub(crate) start: DateTime<Utc>,
    pub(crate) end: DateTime<Utc>,
    pub(crate) window_seconds: u64,
}

impl AccountQuotaReadModel {
    pub(crate) fn from_snapshot(
        snapshot: QuotaSnapshot,
        fetched_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            fetched_at,
            windows: snapshot
                .classified_windows()
                .into_iter()
                .map(|quota| AccountQuotaWindowReadModel {
                    quota,
                    local_usage: None,
                })
                .collect(),
        }
    }

    pub(crate) fn usage_windows(&self) -> Vec<AccountQuotaUsageWindow> {
        self.windows
            .iter()
            .filter_map(AccountQuotaWindowReadModel::usage_window)
            .collect()
    }

    fn apply_local_usage(
        &mut self,
        usage_by_window: &HashMap<String, AccountQuotaWindowLocalUsage>,
    ) {
        for window in &mut self.windows {
            if window.usage_window().is_some() {
                window.local_usage = Some(
                    usage_by_window
                        .get(&window.quota.key)
                        .copied()
                        .unwrap_or_default(),
                );
            }
        }
    }
}

impl AccountQuotaWindowReadModel {
    fn usage_window(&self) -> Option<AccountQuotaUsageWindow> {
        let end = self.quota.window.reset_datetime()?;
        let window_seconds = self.quota.window.window_seconds()?;
        let seconds = i64::try_from(window_seconds).ok()?;
        let start = end.checked_sub_signed(Duration::seconds(seconds))?;
        (start <= end).then(|| AccountQuotaUsageWindow {
            key: self.quota.key.clone(),
            start,
            end,
            window_seconds,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AccountModelUsage {
    pub(crate) model: String,
    pub(crate) request_count: i64,
    pub(crate) error_count: i64,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cached_tokens: i64,
    pub(crate) billing_amount_usd: f64,
    pub(crate) last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Error)]
pub(crate) enum AccountListQueryError {
    #[error("failed to list accounts")]
    Accounts,
    #[error("failed to load account usage")]
    Usage,
    #[error("failed to load account quota usage")]
    QuotaUsage,
    #[error("failed to load account model usage")]
    ModelUsage,
    #[error("failed to load token refresh activity")]
    RefreshActivity,
}

#[async_trait]
pub(crate) trait RefreshActivityQuery: Send + Sync + 'static {
    async fn refreshing_account_ids(
        &self,
        account_ids: &[String],
        now: DateTime<Utc>,
    ) -> Result<std::collections::HashSet<String>, AccountListQueryError>;
}

#[async_trait]
impl<R> RefreshActivityQuery for TokenRefreshService<R>
where
    R: TokenRefresher,
{
    async fn refreshing_account_ids(
        &self,
        account_ids: &[String],
        now: DateTime<Utc>,
    ) -> Result<std::collections::HashSet<String>, AccountListQueryError> {
        TokenRefreshService::refreshing_account_ids(self, account_ids, now)
            .await
            .map_err(|_| AccountListQueryError::RefreshActivity)
    }
}

#[derive(Clone)]
pub(crate) struct AccountListQueryService {
    accounts: Arc<AccountManageService>,
    usage: Arc<AccountUsageQueryService>,
    refresh_activity: Arc<dyn RefreshActivityQuery>,
}

impl AccountListQueryService {
    pub(crate) fn new(
        accounts: Arc<AccountManageService>,
        usage: Arc<AccountUsageQueryService>,
        refresh_activity: Arc<dyn RefreshActivityQuery>,
    ) -> Self {
        Self {
            accounts,
            usage,
            refresh_activity,
        }
    }

    pub(crate) async fn query(
        &self,
        query: AccountListQuery,
    ) -> Result<AccountListResult, AccountListQueryError> {
        let page = self
            .accounts
            .list_page(
                query.page,
                query.page_size,
                query.search,
                query.status,
                query.sort,
            )
            .await
            .map_err(|_| AccountListQueryError::Accounts)?;
        let all_accounts = self.list_all_accounts().await?;
        let summary = account_summary(&all_accounts);
        let mut quota_by_account = self.quota_by_account().await?;
        let account_ids = page
            .items
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let usage_records = self
            .usage
            .list_by_account_ids(&account_ids)
            .await
            .map_err(|_| AccountListQueryError::Usage)?;
        let refreshing = self
            .refresh_activity
            .refreshing_account_ids(&account_ids, Utc::now())
            .await?;

        let quota_windows = quota_usage_windows(&page.items, &quota_by_account);
        let quota_usage = self.quota_local_usage(&quota_windows).await?;
        for account in &page.items {
            if let Some(quota) = quota_by_account.get_mut(&account.id) {
                quota.apply_local_usage(quota_usage.get(&account.id).unwrap_or(&HashMap::new()));
            }
        }
        let selected_windows = selected_quota_windows(&page.items, &quota_by_account);
        let usage_by_account = usage_records
            .into_iter()
            .map(|usage| {
                let usage = apply_selected_window_usage(usage, &selected_windows, &quota_usage);
                (usage.account_id.clone(), usage)
            })
            .collect::<HashMap<_, _>>();
        let models_by_account = self.model_usage(&usage_by_account).await?;

        let items = page
            .items
            .into_iter()
            .map(|account| {
                let account_id = account.id.clone();
                AccountListItem {
                    token_refreshing: token_refresh_status_eligible(account.status)
                        && refreshing.contains(&account_id),
                    usage: usage_by_account.get(&account_id).cloned(),
                    quota: quota_by_account.remove(&account_id),
                    models: models_by_account
                        .get(&account_id)
                        .cloned()
                        .unwrap_or_default(),
                    account,
                }
            })
            .collect();
        Ok(AccountListResult {
            page: NumberedPage {
                items,
                total: page.total,
                page: page.page,
                page_size: page.page_size,
            },
            summary,
        })
    }

    pub(crate) async fn enrich_account(
        &self,
        account: ManagedAccount,
        quota: AccountQuotaReadModel,
    ) -> Result<AccountListItem, AccountListQueryError> {
        let account_id = account.id.clone();
        let mut quota_by_account = HashMap::from([(account_id.clone(), quota)]);
        let windows = quota_usage_windows(std::slice::from_ref(&account), &quota_by_account);
        let quota_usage = self.quota_local_usage(&windows).await?;
        if let Some(quota) = quota_by_account.get_mut(&account_id) {
            quota.apply_local_usage(quota_usage.get(&account_id).unwrap_or(&HashMap::new()));
        }
        let selected = selected_quota_windows(std::slice::from_ref(&account), &quota_by_account);
        let mut usage = self
            .usage
            .list_by_account_ids(std::slice::from_ref(&account_id))
            .await
            .map_err(|_| AccountListQueryError::Usage)?
            .into_iter()
            .next()
            .map(|usage| apply_selected_window_usage(usage, &selected, &quota_usage));
        let usage_by_account = usage
            .iter()
            .cloned()
            .map(|usage| (usage.account_id.clone(), usage))
            .collect::<HashMap<_, _>>();
        let models = self
            .model_usage(&usage_by_account)
            .await?
            .remove(&account_id)
            .unwrap_or_default();
        let refreshing = self
            .refresh_activity
            .refreshing_account_ids(std::slice::from_ref(&account_id), Utc::now())
            .await?;
        Ok(AccountListItem {
            token_refreshing: token_refresh_status_eligible(account.status)
                && refreshing.contains(&account_id),
            account,
            usage: usage.take(),
            quota: quota_by_account.remove(&account_id),
            models,
        })
    }

    async fn list_all_accounts(&self) -> Result<Vec<ManagedAccount>, AccountListQueryError> {
        let mut page_number = 1;
        let mut accounts = Vec::new();
        loop {
            let page = self
                .accounts
                .list_page(page_number, ACCOUNT_STATS_PAGE_LIMIT, None, None, None)
                .await
                .map_err(|_| AccountListQueryError::Accounts)?;
            let total = page.total;
            accounts.extend(page.items);
            if accounts.len() as u64 >= total || total == 0 {
                return Ok(accounts);
            }
            page_number = page_number.saturating_add(1);
        }
    }

    async fn quota_by_account(
        &self,
    ) -> Result<HashMap<String, AccountQuotaReadModel>, AccountListQueryError> {
        let snapshots = self
            .accounts
            .quota_snapshots()
            .await
            .map_err(|_| AccountListQueryError::Accounts)?;
        Ok(snapshots
            .into_iter()
            .filter_map(|snapshot| {
                let quota = QuotaSnapshot::from_json(&snapshot.quota_json).ok()?;
                Some((
                    snapshot.account_id,
                    AccountQuotaReadModel::from_snapshot(quota, snapshot.quota_fetched_at),
                ))
            })
            .collect())
    }

    async fn quota_local_usage(
        &self,
        windows: &[AccountQuotaWindowSelection],
    ) -> Result<HashMap<String, HashMap<String, AccountQuotaWindowLocalUsage>>, AccountListQueryError>
    {
        let queries = windows
            .iter()
            .map(|selection| UsageBucketWindow {
                account_id: selection.account_id.clone(),
                key: selection.window.key.clone(),
                start: selection.window.start,
                end: selection.window.end,
            })
            .collect::<Vec<_>>();
        self.usage
            .usage_by_windows(&queries)
            .await
            .map_err(|_| AccountListQueryError::QuotaUsage)
            .map(|usage| {
                usage
                    .into_iter()
                    .map(|(account_id, windows)| {
                        let windows = windows
                            .into_iter()
                            .map(|(key, usage)| {
                                (
                                    key,
                                    AccountQuotaWindowLocalUsage {
                                        request_count: usage.request_count,
                                        input_tokens: usage.input_tokens,
                                        output_tokens: usage.output_tokens,
                                        cached_tokens: usage.cached_tokens,
                                    },
                                )
                            })
                            .collect();
                        (account_id, windows)
                    })
                    .collect()
            })
    }

    async fn model_usage(
        &self,
        usage_by_account: &HashMap<String, AccountUsageRecord>,
    ) -> Result<HashMap<String, Vec<AccountModelUsage>>, AccountListQueryError> {
        let now = Utc::now();
        let windows = usage_by_account
            .values()
            .filter_map(|usage| {
                current_usage_window(usage, now).map(|(start, end)| ModelUsageWindow {
                    account_id: usage.account_id.clone(),
                    start,
                    end,
                })
            })
            .collect::<Vec<_>>();
        if windows.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = self
            .usage
            .model_usage_by_windows(&windows)
            .await
            .map_err(|_| AccountListQueryError::ModelUsage)?;
        let mut records = HashMap::<(String, String), AccountModelUsage>::new();
        for row in rows {
            let billing_amount_usd = billing::calculate_billing_amount(
                nonnegative_i64_to_u64(row.input_tokens),
                nonnegative_i64_to_u64(row.output_tokens),
                nonnegative_i64_to_u64(row.cached_tokens),
                nonnegative_i64_to_u64(row.cache_write_tokens),
                &row.model,
                row.service_tier.as_deref(),
            );
            let record = records
                .entry((row.account_id.clone(), row.model.clone()))
                .or_insert_with(|| AccountModelUsage {
                    model: row.model,
                    request_count: 0,
                    error_count: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    cached_tokens: 0,
                    billing_amount_usd: 0.0,
                    last_used_at: None,
                });
            record.request_count += row.request_count;
            record.error_count += row.error_count;
            record.input_tokens += row.input_tokens;
            record.output_tokens += row.output_tokens;
            record.cached_tokens += row.cached_tokens;
            record.billing_amount_usd += billing_amount_usd;
            record.last_used_at = record.last_used_at.max(row.last_used_at);
        }
        let mut by_account = HashMap::<String, Vec<AccountModelUsage>>::new();
        for ((account_id, _), record) in records {
            by_account.entry(account_id).or_default().push(record);
        }
        for records in by_account.values_mut() {
            records.sort_by(|left, right| {
                right
                    .request_count
                    .cmp(&left.request_count)
                    .then_with(|| right.last_used_at.cmp(&left.last_used_at))
                    .then_with(|| left.model.cmp(&right.model))
            });
        }
        Ok(by_account)
    }
}

#[derive(Debug, Clone)]
struct AccountQuotaWindowSelection {
    account_id: String,
    window: AccountQuotaUsageWindow,
}

fn account_summary(accounts: &[ManagedAccount]) -> AccountSummary {
    AccountSummary {
        total: accounts.len() as u64,
        active: accounts
            .iter()
            .filter(|account| account.status == AccountStatus::Active)
            .count() as u64,
        quota_exhausted: accounts
            .iter()
            .filter(|account| account.status == AccountStatus::QuotaExhausted)
            .count() as u64,
        attention: accounts
            .iter()
            .filter(|account| {
                matches!(
                    account.status,
                    AccountStatus::Expired | AccountStatus::Disabled | AccountStatus::Banned
                )
            })
            .count() as u64,
    }
}

fn quota_usage_windows(
    accounts: &[ManagedAccount],
    quota_by_account: &HashMap<String, AccountQuotaReadModel>,
) -> Vec<AccountQuotaWindowSelection> {
    accounts
        .iter()
        .filter_map(|account| {
            quota_by_account
                .get(&account.id)
                .map(|quota| (account.id.as_str(), quota))
        })
        .flat_map(|(account_id, quota)| {
            quota
                .usage_windows()
                .into_iter()
                .map(move |window| AccountQuotaWindowSelection {
                    account_id: account_id.to_string(),
                    window,
                })
        })
        .collect()
}

fn selected_quota_windows(
    accounts: &[ManagedAccount],
    quota_by_account: &HashMap<String, AccountQuotaReadModel>,
) -> HashMap<String, AccountQuotaUsageWindow> {
    accounts
        .iter()
        .filter_map(|account| {
            let window = quota_by_account
                .get(&account.id)?
                .usage_windows()
                .into_iter()
                .max_by(|left, right| {
                    left.window_seconds
                        .cmp(&right.window_seconds)
                        .then_with(|| left.end.cmp(&right.end))
                        .then_with(|| left.key.cmp(&right.key))
                })?;
            Some((account.id.clone(), window))
        })
        .collect()
}

fn apply_selected_window_usage(
    mut usage: AccountUsageRecord,
    selected: &HashMap<String, AccountQuotaUsageWindow>,
    quota_usage: &HashMap<String, HashMap<String, AccountQuotaWindowLocalUsage>>,
) -> AccountUsageRecord {
    let Some(window) = selected.get(&usage.account_id) else {
        return usage;
    };
    let stats = quota_usage
        .get(&usage.account_id)
        .and_then(|windows| windows.get(&window.key))
        .copied()
        .unwrap_or_default();
    usage.window_request_count = stats.request_count;
    usage.window_input_tokens = stats.input_tokens;
    usage.window_output_tokens = stats.output_tokens;
    usage.window_cached_tokens = stats.cached_tokens;
    usage.window_started_at = Some(window.start);
    usage.window_reset_at = Some(window.end);
    usage.limit_window_seconds = Some(window.window_seconds);
    usage
}

fn current_usage_window(
    usage: &AccountUsageRecord,
    now: DateTime<Utc>,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = usage.window_started_at?;
    let end = usage.window_reset_at.unwrap_or(now);
    (start <= end).then_some((start, end))
}

pub(super) fn account_display_status(
    status: AccountStatus,
    token_refreshing: bool,
) -> &'static str {
    if token_refreshing {
        "refreshing"
    } else {
        status.as_str()
    }
}

pub(super) fn account_list_item_data(item: AccountListItem) -> AdminAccountData {
    let AccountListItem {
        account,
        usage,
        quota,
        models,
        token_refreshing,
    } = item;
    AdminAccountData::from_parts(
        account,
        usage.as_ref(),
        quota.map(quota_data),
        models.into_iter().map(model_usage_data).collect(),
        token_refreshing,
    )
}

pub(super) fn model_usage_data(usage: AccountModelUsage) -> AdminAccountModelUsageData {
    let request_count = nonnegative_i64_to_u64(usage.request_count);
    let error_count = nonnegative_i64_to_u64(usage.error_count);
    let input_tokens = nonnegative_i64_to_u64(usage.input_tokens);
    let output_tokens = nonnegative_i64_to_u64(usage.output_tokens);
    let cached_tokens = nonnegative_i64_to_u64(usage.cached_tokens);
    let total_tokens = input_tokens + output_tokens;
    let success_rate = if request_count > 0 {
        ((request_count.saturating_sub(error_count)) as f64 / request_count as f64 * 1000.0).round()
            / 10.0
    } else {
        0.0
    };

    AdminAccountModelUsageData {
        model: usage.model,
        request_count,
        request_count_display: format_plain_number(request_count),
        success_rate,
        success_rate_display: format_percent(success_rate),
        input_tokens,
        input_tokens_display: format_tokens(input_tokens),
        output_tokens,
        output_tokens_display: format_tokens(output_tokens),
        cached_tokens,
        cached_tokens_display: format_tokens(cached_tokens),
        total_tokens,
        total_tokens_display: format_tokens(total_tokens),
        billing_amount_usd: usage.billing_amount_usd,
        billing_amount_usd_display: format_billing_amount(usage.billing_amount_usd),
        last_used_at: usage.last_used_at.map(|value| china_rfc3339(&value)),
        last_used_at_display: china_relative_time(usage.last_used_at, Utc::now()),
    }
}

pub(super) fn account_status_filter(
    status: Option<String>,
) -> Result<Option<AccountStatus>, AdminError> {
    let Some(status) = status.map(|value| value.trim().to_string()) else {
        return Ok(None);
    };
    if status.is_empty() {
        return Ok(None);
    }
    AccountStatus::parse(&status)
        .map(Some)
        .ok_or_else(|| AdminError::bad_request("Invalid account status"))
}

pub(super) fn account_list_sort(
    sort_by: Option<String>,
    sort_direction: Option<String>,
) -> Result<Option<AccountListSort>, AdminError> {
    let (sort_by, sort_direction) = match (sort_by, sort_direction) {
        (None, None) => return Ok(None),
        (Some(sort_by), Some(sort_direction)) => (sort_by, sort_direction),
        _ => {
            return Err(AdminError::bad_request(
                "Account sort field and direction must be provided together",
            ));
        }
    };
    let field = match sort_by.trim() {
        "email" => AccountSortField::Email,
        "status" => AccountSortField::Status,
        "planType" => AccountSortField::PlanType,
        "usage" => AccountSortField::Usage,
        "lastUsedAt" => AccountSortField::LastUsedAt,
        "expiresAt" => AccountSortField::ExpiresAt,
        _ => return Err(AdminError::bad_request("Invalid account sort field")),
    };
    let direction = SortDirection::parse(&sort_direction)
        .ok_or_else(|| AdminError::bad_request("Invalid account sort direction"))?;
    Ok(Some(AccountListSort { field, direction }))
}

pub(super) struct ParsedAccountUpdate {
    pub(super) id: String,
    pub(super) update: AccountUpdate,
}

pub(super) fn parse_account_update(payload: &Value) -> Result<ParsedAccountUpdate, AdminError> {
    let payload = parse_editable_update(
        payload,
        EditableUpdateMessages {
            object_required: "Account update request must be an object",
            invalid: "Invalid account update request",
            empty_update: "Account update request must include editable fields",
            unknown_field_editable: true,
        },
    )?;
    let update = AccountUpdate {
        label: payload.label.map(|label| {
            label.and_then(|value| {
                let value = value.trim();
                (!value.is_empty()).then(|| value.to_string())
            })
        }),
        status: payload.status,
    };
    if !update.any() {
        return Err(AdminError::bad_request(
            "Account update request must include editable fields",
        ));
    }
    Ok(ParsedAccountUpdate {
        id: payload.id,
        update,
    })
}

pub(super) fn account_error(error: &AccountManageError) -> AdminError {
    match error {
        AccountManageError::InvalidStatus(_)
        | AccountManageError::LabelTooLong
        | AccountManageError::EmptyIds
        | AccountManageError::NoImportableAccounts
        | AccountManageError::NoModels
        | AccountManageError::InvalidAccessTokenExpiresAt
        | AccountManageError::TokenRequired
        | AccountManageError::InvalidToken(_)
        | AccountManageError::RefreshTokenExchange(_)
        | AccountManageError::OAuthSessionInvalid
        | AccountManageError::OAuthCallbackInvalid
        | AccountManageError::OAuthStateMismatch
        | AccountManageError::NoValidCookies => AdminError::bad_request(error.to_string()),
        AccountManageError::OAuthCodeExchange(_) | AccountManageError::RefreshModels(_) => {
            AdminError::bad_gateway(error.to_string())
        }
        AccountManageError::NotFound => account_not_found(),
        AccountManageError::Inactive(_) => AdminError::conflict(error.to_string()),
        _ => AdminError::internal(error.to_string()),
    }
}

pub(super) fn account_refresh_outcome_str(outcome: AccountRefreshOutcome) -> &'static str {
    match outcome {
        AccountRefreshOutcome::Alive => "alive",
        AccountRefreshOutcome::Dead => "dead",
        AccountRefreshOutcome::Skipped => "skipped",
    }
}

pub(super) fn account_not_found() -> AdminError {
    AdminError::not_found("Account not found")
}

pub(super) fn account_export_ids(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|ids| ids.split(','))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect()
}
