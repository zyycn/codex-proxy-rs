//! Admin 领域结果到安全 HTTP wire 的展示投影。

use super::*;

pub(super) fn account_page_data(
    result: AccountDirectoryPage,
    page: u32,
    page_size: u16,
    now: DateTime<Utc>,
) -> AccountPageData {
    let total_pages = if result.total == 0 {
        0
    } else {
        u32::try_from(result.total.div_ceil(u64::from(page_size))).unwrap_or(u32::MAX)
    };
    AccountPageData {
        items: result
            .items
            .into_iter()
            .map(|item| account_view(item, now))
            .collect(),
        page: PageMeta::new(page, u32::from(page_size), result.total, total_pages),
        summary: AccountSummaryView {
            total: result.summary.total,
            normal: result.summary.normal,
            quota_exhausted: result.summary.quota_exhausted,
            rate_limited: result.summary.rate_limited,
            disabled: result.summary.disabled,
            error: result.summary.error,
        },
    }
}

pub(super) fn account_refresh_data(
    result: AccountRefreshResult,
    now: DateTime<Utc>,
) -> AccountRefreshData {
    AccountRefreshData {
        account: account_view(result.account, now),
    }
}

pub(super) fn account_models_data(result: ProviderModels) -> AccountModelsData {
    AccountModelsData {
        models: result
            .models
            .into_iter()
            .map(|model| {
                let id = model.id.to_string();
                AccountModelView {
                    label: id.clone(),
                    id,
                }
            })
            .collect(),
    }
}

pub(super) fn account_view(item: AccountDirectoryItem, now: DateTime<Utc>) -> AccountView {
    let AccountDirectoryItem {
        account,
        projection,
        usage,
        quota,
    } = item;
    let status = projection.status.as_str().to_owned();
    let rate_limited_until = projection.rate_limited_until.map(DateTime::<Utc>::from);
    let expires_at = account.access_token_expires_at.as_ref().map(china_rfc3339);
    let added_at = china_rfc3339(&account.created_at);
    let updated_at = china_rfc3339(&account.updated_at);
    let (quota, refresh_token_expires_at) = account_quota_view(quota, rate_limited_until, now);
    AccountView {
        id: account.id.clone(),
        name: account.name,
        provider: account.provider_kind.to_string(),
        groups: account
            .groups
            .into_iter()
            .map(|group| AccountGroupRefView {
                id: group.id.to_string(),
                name: group.name,
                color: group.color.as_str().to_owned(),
                enabled: group.enabled,
            })
            .collect(),
        resource_ref: account.id,
        email: account.email,
        account_id: account.upstream_account_id,
        user_id: account.upstream_user_id,
        label: None,
        plan_type: account.plan_type,
        authentication_kind: account.authentication_kind,
        has_refresh_token: account.has_refresh_token,
        status,
        error_reason: projection
            .error_reason
            .map(|reason| reason.as_str().to_owned()),
        error_message: projection.error_message,
        enabled: account.enabled,
        concurrency_limit: account.concurrency_limit.map(|limit| limit.get()),
        weight: account.weight.get(),
        access_token_expires_at: expires_at,
        access_token_expires_at_display: account
            .access_token_expires_at
            .as_ref()
            .map(china_datetime),
        refresh_token_expires_at,
        next_refresh_at: account.next_refresh_at.map(|value| china_rfc3339(&value)),
        added_at,
        added_at_display: china_datetime(&account.created_at),
        updated_at,
        updated_at_display: china_datetime(&account.updated_at),
        quota,
        usage: account_usage_view(usage, now),
    }
}

pub(super) fn account_quota_view(
    mut quota: ProviderQuota,
    rate_limited_until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (AccountQuotaView, Option<String>) {
    quota.apply_limit_reached_display();
    let refresh_token_expires_at = quota
        .refresh_token_expires_at
        .map(|value| china_rfc3339(&value));
    let refreshed_at_display = quota
        .observed_at
        .map_or_else(|| "—".to_owned(), |value| relative_time(value, now));
    let windows = quota.windows.into_iter().map(quota_window_view).collect();
    let rate_limited_until = rate_limited_until.map(|until| china_datetime(&until));
    (
        AccountQuotaView {
            refreshed_at_display,
            limit_reached: quota.limit_reached,
            rate_limited_until,
            windows,
        },
        refresh_token_expires_at,
    )
}

pub(crate) fn quota_window_view(window: ProviderQuotaWindow) -> AccountQuotaWindowView {
    let ProviderQuotaWindow {
        key,
        group,
        label,
        limit_id,
        limit_name,
        role,
        local_usage_attribution: _,
        window_seconds,
        used_percent,
        reset_at,
        limit_reached,
        local_usage,
        provider_data: _,
    } = window;
    let label_display = limit_name
        .as_ref()
        .map_or_else(|| label.clone(), |name| format!("{name} · {label}"));
    AccountQuotaWindowView {
        label_display,
        window_label_display: label,
        key,
        group,
        limit_id,
        limit_name,
        role: role.map(|value| value.as_str().to_owned()),
        window_seconds,
        used_percent,
        used_percent_display: used_percent
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}%")),
        limit_reached,
        local_usage: local_usage.as_ref().map(quota_local_usage),
        reset_at_display: reset_at.map_or_else(|| "—".to_owned(), |value| china_datetime(&value)),
    }
}

pub(super) fn quota_local_usage(usage: &AccountUsage) -> Value {
    let total_tokens = usage.total_tokens.unwrap_or_default();
    serde_json::json!({
        "requestCount": usage.request_count,
        "requestCountDisplay": format_number(usage.request_count),
        "inputTokens": usage.input_tokens.unwrap_or_default(),
        "inputTokensDisplay": display_optional_tokens(usage.input_tokens),
        "outputTokens": usage.output_tokens.unwrap_or_default(),
        "outputTokensDisplay": display_optional_tokens(usage.output_tokens),
        "cachedTokens": usage.cached_tokens.unwrap_or_default(),
        "cachedTokensDisplay": display_optional_tokens(usage.cached_tokens),
        "imageInputTokens": usage.image_input_tokens.unwrap_or_default(),
        "imageInputTokensDisplay": display_optional_tokens(usage.image_input_tokens),
        "imageOutputTokens": usage.image_output_tokens.unwrap_or_default(),
        "imageOutputTokensDisplay": display_optional_tokens(usage.image_output_tokens),
        "imageRequestCount": usage.image_request_count,
        "imageRequestFailedCount": usage.image_request_failed_count,
        "totalTokens": total_tokens,
        "totalTokensDisplay": format_compact_number(total_tokens),
        "requestBuckets": usage.request_buckets.iter().map(|bucket| serde_json::json!({
            "bucketStart": bucket.bucket_start,
            "requestCount": bucket.request_count,
        })).collect::<Vec<_>>(),
    })
}

pub(super) fn account_usage_view(
    usage: Option<AccountUsage>,
    now: DateTime<Utc>,
) -> AccountUsageView {
    let Some(usage) = usage else {
        return empty_account_usage();
    };
    let known_count = usage
        .cost_coverage
        .provider_reported_count
        .saturating_add(usage.cost_coverage.calculated_count);
    let cost_estimate_status = if known_count == 0 {
        "unknown"
    } else if usage.cost_coverage.unavailable_count > 0 {
        "partial"
    } else {
        "known"
    };
    AccountUsageView {
        request_count: Some(usage.request_count),
        request_count_display: format_number(usage.request_count),
        input_tokens: usage.input_tokens,
        input_tokens_display: display_optional_tokens(usage.input_tokens),
        output_tokens: usage.output_tokens,
        output_tokens_display: display_optional_tokens(usage.output_tokens),
        cached_tokens: usage.cached_tokens,
        cached_tokens_display: display_optional_tokens(usage.cached_tokens),
        reasoning_tokens: usage.reasoning_tokens,
        reasoning_tokens_display: display_optional_tokens(usage.reasoning_tokens),
        image_input_tokens: usage.image_input_tokens,
        image_input_tokens_display: display_optional_tokens(usage.image_input_tokens),
        image_output_tokens: usage.image_output_tokens,
        image_output_tokens_display: display_optional_tokens(usage.image_output_tokens),
        image_request_count: Some(usage.image_request_count),
        image_request_count_display: format_number(usage.image_request_count),
        image_request_failed_count: Some(usage.image_request_failed_count),
        image_request_failed_count_display: format_number(usage.image_request_failed_count),
        total_tokens: usage.total_tokens,
        total_tokens_display: display_optional_tokens(usage.total_tokens),
        created_tokens: usage.cache_write_tokens,
        created_tokens_display: display_optional_tokens(usage.cache_write_tokens),
        read_tokens: usage.cached_tokens,
        read_tokens_display: display_optional_tokens(usage.cached_tokens),
        last_used_at: usage.last_used_at.map(|value| china_rfc3339(&value)),
        last_used_at_display: usage
            .last_used_at
            .map_or_else(|| "—".to_owned(), |value| relative_time(value, now)),
        cost_estimate_status: cost_estimate_status.to_owned(),
        known_cost_count: Some(known_count),
        partial_cost_count: Some(u64::from(cost_estimate_status == "partial")),
        unknown_cost_count: Some(usage.cost_coverage.unavailable_count),
        costs: usage.costs.iter().map(account_currency_cost_view).collect(),
        models: usage
            .models
            .into_iter()
            .map(|model| account_model_usage_view(model, now))
            .collect(),
    }
}

pub(super) fn account_model_usage_view(
    usage: AccountModelUsage,
    now: DateTime<Utc>,
) -> ModelUsageView {
    let known_count = usage
        .cost_coverage
        .provider_reported_count
        .saturating_add(usage.cost_coverage.calculated_count);
    let cost_estimate_status = if known_count == 0 {
        "unknown"
    } else if usage.cost_coverage.unavailable_count > 0 {
        "partial"
    } else {
        "known"
    };
    let usd = usage
        .costs
        .iter()
        .find(|cost| cost.currency.eq_ignore_ascii_case("USD"));
    ModelUsageView {
        model: usage.model,
        request_count: usage.request_count,
        request_count_display: format_number(usage.request_count),
        success_rate: (usage.request_count > 0)
            .then(|| usage.success_count as f64 * 100.0 / usage.request_count as f64),
        success_rate_display: if usage.request_count == 0 {
            "—".to_owned()
        } else {
            format!(
                "{:.1}%",
                usage.success_count as f64 * 100.0 / usage.request_count as f64
            )
        },
        input_tokens: usage.input_tokens,
        input_tokens_display: display_optional_tokens(usage.input_tokens),
        output_tokens: usage.output_tokens,
        output_tokens_display: display_optional_tokens(usage.output_tokens),
        cached_tokens: usage.cached_tokens,
        cached_tokens_display: display_optional_tokens(usage.cached_tokens),
        image_input_tokens: usage.image_input_tokens,
        image_input_tokens_display: display_optional_tokens(usage.image_input_tokens),
        image_output_tokens: usage.image_output_tokens,
        image_output_tokens_display: display_optional_tokens(usage.image_output_tokens),
        image_request_count: usage.image_request_count,
        image_request_count_display: format_number(usage.image_request_count),
        image_request_failed_count: usage.image_request_failed_count,
        image_request_failed_count_display: format_number(usage.image_request_failed_count),
        total_tokens: usage.total_tokens,
        total_tokens_display: display_optional_tokens(usage.total_tokens),
        billing_amount_usd: usd.map(|cost| cost.amount.as_str().to_owned()),
        billing_amount_usd_display: usd.map_or_else(
            || "—".to_owned(),
            |cost| format_decimal_currency(cost.amount.as_str(), "USD"),
        ),
        cost_estimate_status: cost_estimate_status.to_owned(),
        known_cost_count: known_count,
        partial_cost_count: u64::from(cost_estimate_status == "partial"),
        unknown_cost_count: usage.cost_coverage.unavailable_count,
        costs: usage.costs.iter().map(account_currency_cost_view).collect(),
        last_used_at: china_rfc3339(&usage.last_used_at),
        last_used_at_display: relative_time(usage.last_used_at, now),
    }
}

pub(super) fn account_currency_cost_view(cost: &AccountCost) -> CurrencyCostView {
    CurrencyCostView {
        currency: cost.currency.clone(),
        estimated_amount: cost.amount.as_str().to_owned(),
        estimated_amount_display: format_decimal_currency(cost.amount.as_str(), &cost.currency),
    }
}

pub(super) fn display_optional_tokens(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_owned(), format_compact_number)
}

pub(super) fn empty_account_usage() -> AccountUsageView {
    AccountUsageView {
        request_count: None,
        request_count_display: "—".to_owned(),
        input_tokens: None,
        input_tokens_display: "—".to_owned(),
        output_tokens: None,
        output_tokens_display: "—".to_owned(),
        cached_tokens: None,
        cached_tokens_display: "—".to_owned(),
        reasoning_tokens: None,
        reasoning_tokens_display: "—".to_owned(),
        image_input_tokens: None,
        image_input_tokens_display: "—".to_owned(),
        image_output_tokens: None,
        image_output_tokens_display: "—".to_owned(),
        image_request_count: None,
        image_request_count_display: "—".to_owned(),
        image_request_failed_count: None,
        image_request_failed_count_display: "—".to_owned(),
        total_tokens: None,
        total_tokens_display: "—".to_owned(),
        created_tokens: None,
        created_tokens_display: "—".to_owned(),
        read_tokens: None,
        read_tokens_display: "—".to_owned(),
        last_used_at: None,
        last_used_at_display: "—".to_owned(),
        cost_estimate_status: "unavailable".to_owned(),
        known_cost_count: None,
        partial_cost_count: None,
        unknown_cost_count: None,
        costs: Vec::new(),
        models: Vec::new(),
    }
}

pub(super) fn relative_time(value: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let elapsed = now.signed_duration_since(value);
    if elapsed.num_seconds() < 0 {
        return china_datetime(&value);
    }
    if elapsed.num_seconds() < 60 {
        return "刚刚".to_owned();
    }
    if elapsed.num_minutes() < 60 {
        return format!("{} 分钟前", elapsed.num_minutes());
    }
    if elapsed.num_hours() < 24 {
        return format!("{} 小时前", elapsed.num_hours());
    }
    format!("{} 天前", elapsed.num_days())
}

pub(super) fn china_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8 is a valid fixed offset")
}

pub(super) fn china_rfc3339(value: &DateTime<Utc>) -> String {
    value.with_timezone(&china_offset()).to_rfc3339()
}

pub(super) fn china_datetime(value: &DateTime<Utc>) -> String {
    value
        .with_timezone(&china_offset())
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

pub(super) fn map_wire_error(error: WireValidationError) -> AdminError {
    AdminError::bad_request(format!("{} 字段不合法", error.field()))
}

pub(super) fn map_service_error(error: AdminServiceError) -> AdminError {
    super::super::wire::map_admin_service_error(error)
}
