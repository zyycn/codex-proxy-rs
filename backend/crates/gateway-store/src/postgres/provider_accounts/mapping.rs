//! Admin 账号列表、用量与成本的领域映射。

use super::*;

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

pub(crate) fn admin_account_usage_window_model(
    row: &sqlx::postgres::PgRow,
) -> AdminStoreResult<((String, String, String), AccountModelUsage)> {
    let account_id: String = window_usage_value(row, "account_id")?;
    let window_key: String = window_usage_value(row, "window_key")?;
    let model: String = window_usage_value(row, "model")?;
    let provider_reported_count = window_usage_count(row, "provider_reported_count")?;
    let calculated_count = window_usage_count(row, "calculated_count")?;
    let unavailable_count = window_usage_count(row, "unavailable_count")?;
    Ok((
        (account_id, window_key, model.clone()),
        AccountModelUsage {
            model,
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
        },
    ))
}

pub(crate) fn admin_account_usage_window_model_cost(
    row: &sqlx::postgres::PgRow,
) -> AdminStoreResult<((String, String, String), AccountCost)> {
    let key = (
        window_usage_value(row, "account_id")?,
        window_usage_value(row, "window_key")?,
        window_usage_value(row, "model")?,
    );
    let amount = window_usage_value::<String>(row, "amount")?;
    let amount = AdminDecimalAmount::from_str(&amount).map_err(|_| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "persisted account model cost is invalid",
        )
    })?;
    Ok((
        key,
        AccountCost {
            currency: window_usage_value(row, "cost_currency")?,
            amount,
        },
    ))
}

pub(crate) fn admin_account_usage_window_cost(
    row: &sqlx::postgres::PgRow,
) -> AdminStoreResult<((String, String), AccountCost)> {
    let key = (
        window_usage_value(row, "account_id")?,
        window_usage_value(row, "window_key")?,
    );
    let amount = window_usage_value::<String>(row, "amount")?;
    let amount = AdminDecimalAmount::from_str(&amount).map_err(|_| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "persisted account cost is invalid",
        )
    })?;
    Ok((
        key,
        AccountCost {
            currency: window_usage_value(row, "cost_currency")?,
            amount,
        },
    ))
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
