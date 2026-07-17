use super::*;

use crate::admin_queries::accounts::{AccountListItem, AccountModelUsage};

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
