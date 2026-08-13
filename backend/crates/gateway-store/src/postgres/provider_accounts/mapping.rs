//! Admin 账号列表、用量与成本的领域映射。

use std::time::SystemTime;

use super::*;

pub(crate) struct AdminAccountListItem {
    pub(crate) account: ProviderAccountSummary,
    pub(crate) projection: gateway_core::engine::credential::AccountStatusProjection,
    pub(crate) usage: Option<ProviderAccountUsageObservation>,
}

pub(crate) fn retained_usage_range(
    retention_days: u32,
    now: DateTime<Utc>,
) -> AdminStoreResult<ObservabilityRange> {
    let duration = TimeDelta::try_days(i64::from(retention_days)).ok_or_else(|| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "usage retention is outside the supported range",
        )
    })?;
    let start = now.checked_sub_signed(duration).ok_or_else(|| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "usage retention start is outside the supported range",
        )
    })?;
    ObservabilityRange::new(start, now).map_err(|error| admin_store_error(ENTITY, error))
}

pub(crate) fn account_matches_admin_query(
    account: &ProviderAccountSummary,
    status: AdminAccountStatus,
    query: &AdminAccountListQuery,
) -> bool {
    let provider_matches = query
        .provider_kind
        .as_ref()
        .is_none_or(|provider| account.provider_kind == provider.as_str());
    let search_matches = query.search.as_ref().is_none_or(|search| {
        let search = search.to_lowercase();
        [
            Some(account.id.as_str()),
            Some(account.name.as_str()),
            account.email.as_deref(),
            account.upstream_account_id.as_deref(),
            account.upstream_user_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(&search))
    });
    let status_matches = query.status.is_none_or(|expected| expected == status);
    provider_matches && search_matches && status_matches
}

pub(crate) fn admin_account_summary(
    accounts: &[ProviderAccountSummary],
    now: DateTime<Utc>,
    rate_limited_until: &BTreeMap<String, SystemTime>,
) -> AccountSummary {
    let mut summary = AccountSummary {
        total: u64::try_from(accounts.len()).unwrap_or(u64::MAX),
        normal: 0,
        quota_exhausted: 0,
        rate_limited: 0,
        disabled: 0,
        error: 0,
    };
    for account in accounts {
        let until = rate_limited_until.get(&account.id).copied();
        match account_status_projection(account, now.into(), until).status {
            AdminAccountStatus::Normal => summary.normal = summary.normal.saturating_add(1),
            AdminAccountStatus::QuotaExhausted => {
                summary.quota_exhausted = summary.quota_exhausted.saturating_add(1);
            }
            AdminAccountStatus::RateLimited => {
                summary.rate_limited = summary.rate_limited.saturating_add(1);
            }
            AdminAccountStatus::Disabled => {
                summary.disabled = summary.disabled.saturating_add(1);
            }
            AdminAccountStatus::Error => summary.error = summary.error.saturating_add(1),
        }
    }
    summary
}

pub(crate) fn sort_admin_account_items(
    items: &mut [AdminAccountListItem],
    sort: Option<AdminAccountSort>,
) {
    let Some(sort) = sort else {
        items.sort_by(|left, right| left.account.id.cmp(&right.account.id));
        return;
    };
    items.sort_by(|left, right| {
        let ordering = match sort.field {
            AdminAccountSortField::Email => left.account.email.cmp(&right.account.email),
            AdminAccountSortField::Status => left
                .projection
                .status
                .sort_rank()
                .cmp(&right.projection.status.sort_rank()),
            AdminAccountSortField::PlanType => left.account.plan_type.cmp(&right.account.plan_type),
            AdminAccountSortField::Usage => left
                .usage
                .as_ref()
                .and_then(|usage| usage.total_tokens)
                .cmp(&right.usage.as_ref().and_then(|usage| usage.total_tokens)),
            AdminAccountSortField::LastUsedAt => left
                .usage
                .as_ref()
                .and_then(|usage| usage.last_used_at.as_ref())
                .cmp(
                    &right
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.last_used_at.as_ref()),
                ),
            AdminAccountSortField::ExpiresAt => left
                .account
                .access_token_expires_at
                .cmp(&right.account.access_token_expires_at),
        }
        .then_with(|| left.account.id.cmp(&right.account.id));
        match sort.direction {
            AdminSortDirection::Asc => ordering,
            AdminSortDirection::Desc => ordering.reverse(),
        }
    });
}

pub(crate) fn admin_account_record(
    summary: ProviderAccountSummary,
) -> AdminStoreResult<AccountRecord> {
    Ok(AccountRecord {
        id: summary.id,
        provider_kind: ProviderKind::new(summary.provider_kind).map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "persisted Provider kind is invalid",
            )
        })?,
        groups: Vec::new(),
        name: summary.name,
        email: summary.email,
        upstream_user_id: summary.upstream_user_id,
        upstream_account_id: summary.upstream_account_id,
        plan_type: summary.plan_type,
        authentication_kind: summary.authentication_kind,
        credential_revision: admin_revision(summary.credential_revision)?,
        has_refresh_token: summary.has_refresh_token,
        access_token_expires_at: summary.access_token_expires_at,
        next_refresh_at: summary.next_refresh_at,
        enabled: summary.enabled,
        concurrency_limit: summary.concurrency_limit,
        weight: summary.weight,
        credential_state: summary.credential_state,
        credential_observed_at: summary.credential_observed_at,
        quota: summary.quota,
        last_error_reason: summary.last_error_reason,
        last_error_message: summary.last_error_message,
        created_at: summary.created_at,
        updated_at: summary.updated_at,
    })
}

pub(crate) fn prepared_account(
    credential: PreparedCredentialCreate,
) -> StoreResult<NewProviderAccount> {
    Ok(NewProviderAccount {
        id: credential.account_id.as_str().to_owned(),
        provider_kind: credential.provider_kind.as_str().to_owned(),
        name: credential.name,
        email: credential.email,
        upstream_user_id: credential.upstream_user_id,
        upstream_account_id: credential.upstream_account_id,
        plan_type: credential.plan_type,
        authentication_kind: credential.authentication_kind,
        provider_credentials_json: provider_document_json(credential.provider_material)?,
        has_refresh_token: credential.has_refresh_token,
        access_token_expires_at: credential.access_token_expires_at,
        next_refresh_at: credential.next_refresh_at,
        enabled: credential.enabled,
        concurrency_limit: None,
        weight: AccountWeight::DEFAULT,
        credential_state: credential.credential_state,
        credential_observed_at: credential.credential_observed_at,
    })
}

pub(crate) fn provider_document_json(document: ProviderDocument) -> StoreResult<JsonObject> {
    JsonObject::try_from_value(
        ENTITY,
        serde_json::Value::Object(document.into_provider_data().into_inner()),
        CREDENTIALS_MAX_BYTES,
    )
}

pub(crate) fn admin_account_usage(
    usage: ProviderAccountUsageObservation,
) -> AdminStoreResult<AccountUsage> {
    Ok(AccountUsage {
        account_id: usage.account_id,
        request_count: usage.request_count,
        success_count: usage.success_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
        image_request_count: usage.image_request_count,
        image_request_failed_count: usage.image_request_failed_count,
        total_tokens: usage.total_tokens,
        cost_coverage: admin_account_cost_coverage(usage.cost_coverage),
        costs: admin_account_costs(usage.costs)?,
        last_used_at: usage.last_used_at,
        request_buckets: usage
            .request_buckets
            .into_iter()
            .map(|bucket| AccountRequestBucket {
                bucket_start: bucket.bucket_start,
                request_count: bucket.request_count,
            })
            .collect(),
        models: usage
            .models
            .into_iter()
            .map(admin_account_model_usage)
            .collect::<AdminStoreResult<Vec<_>>>()?,
    })
}

pub(crate) fn admin_account_usage_window(
    row: &sqlx::postgres::PgRow,
) -> AdminStoreResult<AccountUsageWindowResult> {
    let provider_reported_count = window_usage_count(row, "provider_reported_count")?;
    let calculated_count = window_usage_count(row, "calculated_count")?;
    let unavailable_count = window_usage_count(row, "unavailable_count")?;
    Ok(AccountUsageWindowResult {
        account_id: window_usage_value(row, "account_id")?,
        key: window_usage_value(row, "window_key")?,
        usage: AccountUsage {
            account_id: window_usage_value(row, "account_id")?,
            request_count: window_usage_count(row, "request_count")?,
            success_count: window_usage_count(row, "success_count")?,
            input_tokens: optional_window_usage_count(row, "input_tokens")?,
            output_tokens: optional_window_usage_count(row, "output_tokens")?,
            cached_tokens: optional_window_usage_count(row, "cached_tokens")?,
            cache_write_tokens: optional_window_usage_count(row, "cache_write_tokens")?,
            reasoning_tokens: optional_window_usage_count(row, "reasoning_tokens")?,
            image_input_tokens: optional_window_usage_count(row, "image_input_tokens")?,
            image_output_tokens: optional_window_usage_count(row, "image_output_tokens")?,
            image_request_count: window_usage_count(row, "image_request_count")?,
            image_request_failed_count: window_usage_count(row, "image_request_failed_count")?,
            total_tokens: optional_window_usage_count(row, "total_tokens")?,
            cost_coverage: AdminCostCoverage {
                provider_reported_count,
                calculated_count,
                partial_count: 0,
                unavailable_count,
                not_billable_count: 0,
            },
            costs: Vec::new(),
            last_used_at: window_usage_value(row, "last_used_at")?,
            request_buckets: Vec::new(),
            models: Vec::new(),
        },
    })
}

pub(crate) fn window_usage_count(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> AdminStoreResult<u64> {
    let value = window_usage_value::<i64>(row, column)?;
    u64::try_from(value).map_err(|_| invalid_window_usage(column))
}

pub(crate) fn optional_window_usage_count(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> AdminStoreResult<Option<u64>> {
    window_usage_value::<Option<i64>>(row, column)?
        .map(|value| u64::try_from(value).map_err(|_| invalid_window_usage(column)))
        .transpose()
}

pub(crate) fn window_usage_value<T>(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> AdminStoreResult<T>
where
    for<'row> T: sqlx::Decode<'row, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get(column)
        .map_err(|_| invalid_window_usage(column))
}

pub(crate) fn invalid_window_usage(column: &'static str) -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Invalid,
        ENTITY,
        format!("invalid quota window usage column: {column}"),
    )
}

pub(crate) fn admin_account_model_usage(
    usage: ProviderAccountModelUsageObservation,
) -> AdminStoreResult<AccountModelUsage> {
    Ok(AccountModelUsage {
        model: usage.model,
        request_count: usage.request_count,
        success_count: usage.success_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
        image_request_count: usage.image_request_count,
        image_request_failed_count: usage.image_request_failed_count,
        total_tokens: usage.total_tokens,
        cost_coverage: admin_account_cost_coverage(usage.cost_coverage),
        costs: admin_account_costs(usage.costs)?,
        last_used_at: usage.last_used_at,
    })
}

pub(crate) const fn admin_account_cost_coverage(
    coverage: crate::postgres::CostCoverage,
) -> AdminCostCoverage {
    AdminCostCoverage {
        provider_reported_count: coverage.provider_reported_count,
        calculated_count: coverage.calculated_count,
        partial_count: 0,
        unavailable_count: coverage.unavailable_count,
        not_billable_count: 0,
    }
}

pub(crate) fn admin_account_costs(
    costs: Vec<CurrencyCostTotal>,
) -> AdminStoreResult<Vec<AccountCost>> {
    costs
        .into_iter()
        .map(|cost| {
            let amount = AdminDecimalAmount::from_str(cost.amount.as_str()).map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    ENTITY,
                    "persisted account cost is invalid",
                )
            })?;
            Ok(AccountCost {
                currency: cost.currency,
                amount,
            })
        })
        .collect()
}
