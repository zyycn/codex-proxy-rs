use super::*;

pub(super) async fn account_list_stats(
    state: &AppState,
    accounts: &[ManagedAccount],
    quota_by_account: &HashMap<String, AccountQuotaData>,
) -> Result<AccountListStats, AdminError> {
    let account_ids = accounts
        .iter()
        .map(|account| account.id.clone())
        .collect::<Vec<_>>();
    let usage_records = state
        .services
        .usage
        .list_by_account_ids(&account_ids)
        .await
        .unwrap_or_default();
    let mut quota_by_account = quota_by_account.clone();
    let all_quota_windows = quota_usage_windows_by_account(accounts, &quota_by_account);
    let quota_window_usage_stats =
        quota_window_local_usage_by_account(state, &all_quota_windows).await;
    apply_quota_window_local_usage(accounts, &mut quota_by_account, &quota_window_usage_stats);
    let selected_quota_windows = selected_quota_windows_by_account(accounts, &quota_by_account);
    let usage_records = usage_records
        .into_iter()
        .map(|usage| {
            apply_selected_quota_window_usage(
                usage,
                &selected_quota_windows,
                &quota_window_usage_stats,
            )
        })
        .collect::<Vec<_>>();
    let models_by_account = list_current_window_model_usage(state, &usage_records).await;
    let refreshing_account_ids = state
        .services
        .token_refresh
        .refreshing_account_ids(&account_ids, Utc::now())
        .await
        .map_err(|error| AdminError::internal(error.to_string()))?;

    Ok(AccountListStats {
        usage_by_account: usage_records
            .into_iter()
            .map(|usage| (usage.account_id.clone(), usage))
            .collect(),
        quota_by_account,
        models_by_account,
        refreshing_account_ids,
    })
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

pub(super) fn selected_quota_windows_by_account(
    accounts: &[ManagedAccount],
    quota_by_account: &HashMap<String, AccountQuotaData>,
) -> HashMap<String, AccountQuotaUsageWindow> {
    accounts
        .iter()
        .filter_map(|account| {
            quota_by_account
                .get(&account.id)
                .and_then(selected_quota_window)
                .map(|window| (account.id.clone(), window))
        })
        .collect()
}

pub(super) fn selected_quota_window(quota: &AccountQuotaData) -> Option<AccountQuotaUsageWindow> {
    quota.usage_windows().into_iter().max_by(|left, right| {
        left.duration_seconds()
            .cmp(&right.duration_seconds())
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.key.cmp(&right.key))
    })
}

pub(super) fn quota_usage_windows_by_account(
    accounts: &[ManagedAccount],
    quota_by_account: &HashMap<String, AccountQuotaData>,
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

pub(super) async fn apply_account_quota_window_local_usage(
    state: &AppState,
    account_id: &str,
    quota: &mut AccountQuotaData,
) {
    let windows = quota
        .usage_windows()
        .into_iter()
        .map(|window| AccountQuotaWindowSelection {
            account_id: account_id.to_string(),
            window,
        })
        .collect::<Vec<_>>();
    let usage_by_account = quota_window_local_usage_by_account(state, &windows).await;
    let empty_usage = HashMap::new();
    quota.apply_local_usage(usage_by_account.get(account_id).unwrap_or(&empty_usage));
}

pub(super) fn apply_quota_window_local_usage(
    accounts: &[ManagedAccount],
    quota_by_account: &mut HashMap<String, AccountQuotaData>,
    usage_by_account: &HashMap<String, HashMap<String, AccountQuotaWindowLocalUsage>>,
) {
    for account in accounts {
        if let Some(quota) = quota_by_account.get_mut(&account.id) {
            let empty_usage = HashMap::new();
            quota.apply_local_usage(usage_by_account.get(&account.id).unwrap_or(&empty_usage));
        }
    }
}

pub(super) async fn quota_window_local_usage_by_account(
    state: &AppState,
    windows: &[AccountQuotaWindowSelection],
) -> HashMap<String, HashMap<String, AccountQuotaWindowLocalUsage>> {
    let queries = windows
        .iter()
        .map(|window| UsageBucketWindow {
            account_id: window.account_id.clone(),
            key: window.window.key.clone(),
            start: window.window.start,
            end: window.window.end,
        })
        .collect::<Vec<_>>();
    state
        .services
        .usage
        .usage_by_windows(&queries)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(account_id, usage_by_window)| {
            let usage_by_window = usage_by_window
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
            (account_id, usage_by_window)
        })
        .collect()
}

pub(super) fn apply_selected_quota_window_usage(
    mut usage: AccountUsageRecord,
    selected_quota_windows: &HashMap<String, AccountQuotaUsageWindow>,
    quota_window_usage_stats: &HashMap<String, HashMap<String, AccountQuotaWindowLocalUsage>>,
) -> AccountUsageRecord {
    let Some(window) = selected_quota_windows.get(&usage.account_id) else {
        return usage;
    };
    let stats = quota_window_usage_stats
        .get(&usage.account_id)
        .and_then(|usage_by_window| usage_by_window.get(&window.key))
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

pub(super) async fn account_summary_data(state: &AppState) -> AdminAccountSummaryData {
    let accounts = list_all_account_metadata(state).await;
    let total = accounts.len() as u64;
    let active = accounts
        .iter()
        .filter(|account| account.status == AccountStatus::Active)
        .count() as u64;
    let quota_exhausted = accounts
        .iter()
        .filter(|account| account.status == AccountStatus::QuotaExhausted)
        .count() as u64;
    let attention = accounts
        .iter()
        .filter(|account| account_summary_needs_attention(account.status))
        .count() as u64;

    AdminAccountSummaryData {
        total,
        active,
        quota_exhausted,
        attention,
    }
}

pub(super) async fn quota_snapshots_by_account(
    state: &AppState,
) -> HashMap<String, AccountQuotaData> {
    state
        .services
        .admin_accounts
        .quota_snapshots()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|snapshot| {
            (
                snapshot.account_id,
                quota_data(&snapshot.quota_json, snapshot.quota_fetched_at),
            )
        })
        .collect()
}

pub(super) async fn list_all_account_metadata(state: &AppState) -> Vec<ManagedAccount> {
    let mut page = 1;
    let mut accounts = Vec::new();
    loop {
        let Ok(result) = state
            .services
            .admin_accounts
            .list_page(page, ACCOUNT_STATS_PAGE_LIMIT, None, None, None)
            .await
        else {
            return Vec::new();
        };
        let total = result.total;
        accounts.extend(result.items);
        if accounts.len() as u64 >= total || total == 0 {
            return accounts;
        }
        page = page.saturating_add(1);
    }
}

pub(super) fn account_summary_needs_attention(status: AccountStatus) -> bool {
    matches!(
        status,
        AccountStatus::Expired | AccountStatus::Disabled | AccountStatus::Banned
    )
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

pub(super) async fn list_current_window_model_usage(
    state: &AppState,
    usage_records: &[AccountUsageRecord],
) -> HashMap<String, Vec<AdminAccountModelUsageData>> {
    let now = Utc::now();
    let windows = usage_records
        .iter()
        .filter_map(|usage| {
            current_usage_window(usage, now)
                .map(|(start, end)| (usage.account_id.clone(), start, end))
        })
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return HashMap::new();
    }

    let windows = windows
        .into_iter()
        .map(|(account_id, start, end)| ModelUsageWindow {
            account_id,
            start,
            end,
        })
        .collect::<Vec<_>>();
    let rows = state
        .services
        .usage
        .model_usage_by_windows(&windows)
        .await
        .unwrap_or_default();
    let mut records_by_model = HashMap::<(String, String), AccountModelUsageRecord>::new();
    for row in rows {
        let account_id = row.account_id;
        let model = row.model;
        let request_count = row.request_count;
        let error_count = row.error_count;
        let input_tokens = row.input_tokens;
        let output_tokens = row.output_tokens;
        let cached_tokens = row.cached_tokens;
        let cache_write_tokens = row.cache_write_tokens;
        let billing_amount_usd = billing::calculate_billing_amount(
            nonnegative_i64_to_u64(input_tokens),
            nonnegative_i64_to_u64(output_tokens),
            nonnegative_i64_to_u64(cached_tokens),
            nonnegative_i64_to_u64(cache_write_tokens),
            &model,
            row.service_tier.as_deref(),
        );
        let last_used_at = row.last_used_at;

        let record = records_by_model
            .entry((account_id.clone(), model.clone()))
            .or_insert_with(|| AccountModelUsageRecord {
                account_id,
                model,
                request_count: 0,
                error_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                billing_amount_usd: 0.0,
                last_used_at: None,
            });
        record.request_count += request_count;
        record.error_count += error_count;
        record.input_tokens += input_tokens;
        record.output_tokens += output_tokens;
        record.cached_tokens += cached_tokens;
        record.billing_amount_usd += billing_amount_usd;
        record.last_used_at = record.last_used_at.max(last_used_at);
    }
    let records = records_by_model.into_values().collect::<Vec<_>>();

    models_by_account(records)
}

pub(super) fn current_usage_window(
    usage: &AccountUsageRecord,
    now: DateTime<Utc>,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = usage.window_started_at?;
    let end = usage.window_reset_at.unwrap_or(now);
    (start <= end).then_some((start, end))
}

pub(super) fn models_by_account(
    records: Vec<AccountModelUsageRecord>,
) -> HashMap<String, Vec<AdminAccountModelUsageData>> {
    let mut by_account = HashMap::<String, Vec<AccountModelUsageRecord>>::new();
    for record in records {
        by_account
            .entry(record.account_id.clone())
            .or_default()
            .push(record);
    }

    by_account
        .into_iter()
        .map(|(account_id, mut records)| {
            records.sort_by(|a, b| {
                b.request_count
                    .cmp(&a.request_count)
                    .then_with(|| b.last_used_at.cmp(&a.last_used_at))
                    .then_with(|| a.model.cmp(&b.model))
            });
            (
                account_id,
                records.into_iter().map(model_usage_data).collect(),
            )
        })
        .collect()
}

pub(super) fn model_usage_data(usage: AccountModelUsageRecord) -> AdminAccountModelUsageData {
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
    let billing_amount_usd = usage.billing_amount_usd;

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
        billing_amount_usd,
        billing_amount_usd_display: format_billing_amount(billing_amount_usd),
        last_used_at: usage.last_used_at.map(|value| china_rfc3339(&value)),
        last_used_at_display: china_relative_time(usage.last_used_at, Utc::now()),
    }
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
        AccountManageError::OAuthCodeExchange(_) => AdminError::bad_gateway(error.to_string()),
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
