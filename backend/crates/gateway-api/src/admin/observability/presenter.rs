//! 过渡期展示格式化；目标迁移到 Vue presenter。

use super::*;

pub(crate) fn display_duration(value: Option<u64>) -> String {
    let Some(value) = value.and_then(|value| i64::try_from(value).ok()) else {
        return "—".to_owned();
    };
    if value < 1_000 {
        format!("{value} ms")
    } else if value < 60_000 {
        format!("{:.2} s", value as f64 / 1_000.0)
    } else if value < 3_600_000 {
        format!("{:.1} min", value as f64 / 60_000.0)
    } else {
        format!("{:.1} h", value as f64 / 3_600_000.0)
    }
}

pub(crate) fn display_rate(value: f64) -> String {
    if value.is_finite() {
        format!("{:.1}%", value * 100.0)
    } else {
        "—".to_owned()
    }
}

pub(crate) fn china_datetime(value: &DateTime<Utc>) -> String {
    (*value + Duration::hours(8))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

pub(crate) fn china_label(value: DateTime<Utc>, format: &str) -> String {
    (value + Duration::hours(8)).format(format).to_string()
}

pub(crate) fn outcome_name(outcome: &domain::RequestOutcome) -> &str {
    outcome.as_str()
}

pub(crate) fn request_metrics_view(metrics: &domain::RequestMetrics) -> RequestMetricsView {
    RequestMetricsView {
        request_count: metrics.request_count,
        success_count: metrics.success_count,
        failure_count: metrics.failure_count,
        cancelled_count: metrics.cancelled_count,
        incomplete_count: metrics.incomplete_count,
        caller_error_count: metrics.caller_error_count,
        input_tokens: metrics.input_tokens,
        output_tokens: metrics.output_tokens,
        cached_tokens: metrics.cached_tokens,
        cache_write_tokens: metrics.cache_write_tokens,
        reasoning_tokens: metrics.reasoning_tokens,
        total_tokens: metrics.total_tokens,
    }
}

pub(crate) fn cost_coverage_view(coverage: &domain::CostCoverage) -> CostCoverageView {
    CostCoverageView {
        known: coverage.known_count(),
        partial: coverage.partial_count,
        unknown: coverage.unavailable_count,
        not_billable: coverage.not_billable_count,
    }
}

pub(crate) fn cost_views(costs: &[domain::CurrencyCost]) -> Vec<CostView> {
    costs
        .iter()
        .map(|cost| CostView {
            currency: cost.currency.clone(),
            estimated_amount: cost.amount.as_str().to_owned(),
        })
        .collect()
}

pub(crate) fn attempt_metrics_view(metrics: &domain::AttemptMetrics) -> AttemptMetricsView {
    AttemptMetricsView {
        attempt_count: metrics.attempt_count,
        success_count: metrics.success_count,
        failure_count: metrics.failure_count,
        cancelled_count: metrics.cancelled_count,
        incomplete_count: metrics.incomplete_count,
        rate_limited_count: metrics.rate_limited_count,
        auth_failure_count: metrics.auth_failure_count,
        provider5xx_count: metrics.provider_5xx_count,
        cost_coverage: cost_coverage_view(&metrics.cost_coverage),
        costs: cost_views(&metrics.costs),
    }
}

pub(crate) fn token_details(record: &domain::UsageRecord) -> TokenDetailsView {
    TokenDetailsView {
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        cache_write_tokens: record.cache_write_tokens,
        reasoning_tokens: record.reasoning_tokens,
        image_input_tokens: record.image_input_tokens,
        image_output_tokens: record.image_output_tokens,
        total_tokens: record.total_tokens,
        input_tokens_display: record
            .input_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        output_tokens_display: record
            .output_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        cached_tokens_display: record
            .cached_tokens
            .map_or_else(|| "-".to_owned(), format_compact_number),
        cache_write_tokens_display: record
            .cache_write_tokens
            .map_or_else(|| "-".to_owned(), format_compact_number),
        reasoning_tokens_display: record
            .reasoning_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        image_input_tokens_display: record
            .image_input_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        image_output_tokens_display: record
            .image_output_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        total_tokens_display: record
            .total_tokens
            .map_or_else(|| "-".to_owned(), format_number),
    }
}

pub(crate) fn format_money(cost: &domain::CurrencyCost) -> String {
    format_decimal_currency(cost.amount.as_str(), &cost.currency)
}

pub(crate) fn format_token_price(cost: &domain::CurrencyCost) -> String {
    if cost.currency != "USD" {
        return format!("{} {} / 1M Token", cost.currency, cost.amount.as_str());
    }
    format!("${} / 1M Token", cost.amount.as_str())
}

pub(crate) fn format_service_tier(service_tier: Option<&str>) -> String {
    match service_tier {
        Some("priority" | "fast") => "Fast".to_owned(),
        Some("flex") => "Flex".to_owned(),
        Some("default") | None => "Default".to_owned(),
        Some(other) => other.to_owned(),
    }
}

pub(crate) fn billing_view(billing: Option<&domain::UsageBilling>) -> Option<BillingView> {
    match billing? {
        domain::UsageBilling::Total { total, .. } => Some(BillingView {
            input_amount_display: "—".to_owned(),
            output_amount_display: "—".to_owned(),
            cache_read_amount_display: "—".to_owned(),
            cache_write_amount_display: "—".to_owned(),
            standard_amount_display: "—".to_owned(),
            total_amount_display: format_money(total),
            input_price_display: "—".to_owned(),
            output_price_display: "—".to_owned(),
            cache_read_price_display: "—".to_owned(),
            cache_write_price_display: "—".to_owned(),
            service_tier_display: "—".to_owned(),
            multiplier_display: "—".to_owned(),
        }),
        domain::UsageBilling::Calculated(value) => Some(BillingView {
            input_amount_display: format_money(&value.input_amount),
            output_amount_display: format_money(&value.output_amount),
            cache_read_amount_display: format_money(&value.cache_read_amount),
            cache_write_amount_display: format_money(&value.cache_write_amount),
            standard_amount_display: format_money(&value.standard_amount),
            total_amount_display: format_money(&value.total_amount),
            input_price_display: format_token_price(&value.input_price_per_million),
            output_price_display: format_token_price(&value.output_price_per_million),
            cache_read_price_display: format_token_price(&value.cache_read_price_per_million),
            cache_write_price_display: format_token_price(&value.cache_write_price_per_million),
            service_tier_display: format_service_tier(value.service_tier.as_deref()),
            multiplier_display: format!("{:.2}x", f64::from(value.multiplier_percent) / 100.0),
        }),
    }
}

pub(crate) fn usage_record_view(record: domain::UsageRecord) -> UsageRecordView {
    let tokens = token_details(&record);
    let billing = billing_view(record.billing.as_ref());
    let costs = record
        .cost_amount
        .as_ref()
        .zip(record.cost_currency.as_ref())
        .map(|(amount, currency)| {
            vec![CostView {
                currency: currency.clone(),
                estimated_amount: amount.as_str().to_owned(),
            }]
        })
        .unwrap_or_default();
    let status_code = record
        .client_status_code
        .or(record.upstream_status_code)
        .map(i64::from);
    let outcome = outcome_name(&record.outcome).to_owned();
    let message = record
        .error_message
        .clone()
        .unwrap_or_else(|| outcome.clone());
    let first_token_display = display_duration(record.first_token_ms);
    let latency_display = display_duration(record.latency_ms);
    let cost_coverage = match record.cost_source.as_str() {
        "provider_reported" | "calculated" => CostCoverageView {
            known: 1,
            partial: 0,
            unknown: 0,
            not_billable: 0,
        },
        _ => CostCoverageView {
            known: 0,
            partial: 0,
            unknown: 1,
            not_billable: 0,
        },
    };
    let model = record
        .upstream_model_id
        .clone()
        .unwrap_or_else(|| record.requested_model_id.clone());
    let transport = record
        .upstream_transport
        .clone()
        .or_else(|| Some(record.client_transport.clone()));
    let metadata = provider_metadata_fields(record.provider_metadata_json.as_deref());
    UsageRecordView {
        id: record.id.clone(),
        request_id: record.id,
        client_api_key_id: Some(record.client_api_key_ref),
        kind: record.operation,
        provider: record.provider_kind,
        authentication_kind: record.provider_account_authentication_kind,
        account_id: record.provider_account_ref,
        account_email: record.provider_account_email,
        account_name: record.provider_account_name,
        route: record.endpoint,
        model,
        requested_model: Some(record.requested_model_id),
        upstream_model: record.upstream_model_id,
        service_tier: record.service_tier,
        status_code,
        transport,
        protocol: record.protocol.clone(),
        http_version: record.http_version.clone(),
        client_status_code: record.client_status_code.map(i64::from),
        upstream_status_code: record.upstream_status_code.map(i64::from),
        websocket_pool: record
            .websocket_pool
            .clone()
            .map(|kind| WebSocketPoolMetadataView { kind }),
        image_generation_requested: record.image_generation_requested,
        image_generation_succeeded: record.image_generation_succeeded,
        latency_details: UsageLatencyDetailsView {
            transport_decision_wait_ms: record.transport_decision_wait_ms,
            ws_connect_ms: record.connect_ms,
            upstream_headers_ms: record.headers_ms,
            first_event_ms: record.first_event_ms,
            first_reasoning_ms: record.first_reasoning_ms,
            first_text_ms: record.first_text_ms,
            first_token_ms: record.first_token_ms,
            openai_processing_ms: record.provider_processing_ms,
        },
        attempt_index: None,
        attempt_count: u64::from(record.attempt_count),
        response_id: record.client_response_id,
        upstream_request_id: record.upstream_request_id,
        latency_ms: record.latency_ms,
        first_token_ms: record.first_token_ms,
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        cache_write_tokens: record.cache_write_tokens,
        reasoning_tokens: record.reasoning_tokens,
        image_input_tokens: record.image_input_tokens,
        image_output_tokens: record.image_output_tokens,
        message,
        metadata,
        created_at: record.started_at,
        created_at_display: china_datetime(&record.started_at),
        client_ip: record.client_ip,
        user_agent: record.user_agent,
        reasoning_effort: record.reasoning_effort,
        reasoning_preset: record.reasoning_preset,
        compact: Some(record.compact),
        request_kind: record.request_kind,
        subagent_kind: record.subagent_kind,
        token_details: tokens,
        billing,
        costs,
        cost_coverage,
        first_token_latency_ms: record.first_token_ms,
        first_token_latency_ms_display: first_token_display,
        latency_ms_display: latency_display,
        logical_outcome: outcome,
    }
}

pub(crate) fn provider_metadata_fields(value: Option<&str>) -> BTreeMap<String, Value> {
    const CORE_METADATA_FIELDS: &[&str] = &[
        "protocol",
        "logicalOutcome",
        "attemptCount",
        "requestedModel",
        "upstreamModel",
        "clientIp",
        "userAgent",
        "reasoningEffort",
        "reasoningPreset",
        "compact",
        "requestKind",
        "subagentKind",
        "transport",
        "httpVersion",
        "clientStatusCode",
        "upstreamStatusCode",
        "responseId",
        "upstreamRequestId",
        "websocketPool",
        "imageGenerationRequested",
        "imageGenerationSucceeded",
        "latencyDetails",
    ];
    let Some(Ok(Value::Object(fields))) = value.map(serde_json::from_str::<Value>) else {
        return BTreeMap::new();
    };
    fields
        .into_iter()
        .filter(|(field, _)| !CORE_METADATA_FIELDS.contains(&field.as_str()))
        .collect()
}

pub(crate) fn usage_attempt_view(attempt: domain::UsageAttempt) -> UsageAttemptView {
    let occurred_at = attempt.occurred_at;
    let credential_name = attempt
        .provider_account_name
        .as_ref()
        .or(attempt.provider_account_ref.as_ref())
        .cloned();
    UsageAttemptView {
        id: attempt.id,
        attempt_index: attempt.attempt_index,
        trigger: attempt.source,
        provider: attempt
            .provider_kind
            .unwrap_or_else(|| "unknown".to_owned()),
        model: attempt
            .upstream_model_id
            .unwrap_or_else(|| "unknown".to_owned()),
        transport: attempt
            .upstream_transport
            .unwrap_or_else(|| "unknown".to_owned()),
        send_state: attempt
            .upstream_send_state
            .unwrap_or_else(|| "unknown".to_owned()),
        outcome: outcome_name(&attempt.outcome).to_owned(),
        downstream_committed: attempt.downstream_committed,
        status_code: attempt.status_code,
        provider_error_code: attempt.provider_error_code,
        failure_class: attempt.failure_kind,
        cost_estimate_status: attempt
            .cost_source
            .clone()
            .unwrap_or_else(|| "unavailable".to_owned()),
        estimated_cost_amount: attempt.cost_amount.map(|amount| amount.as_str().to_owned()),
        estimated_cost_currency: attempt.cost_currency,
        input_tokens: attempt.input_tokens,
        output_tokens: attempt.output_tokens,
        cached_tokens: attempt.cached_tokens,
        total_tokens: attempt.total_tokens,
        first_token_ms: None,
        latency_ms: attempt.latency_ms,
        credential_name,
        account_id: attempt.provider_account_ref,
        account_name: attempt.provider_account_name,
        account_email: attempt.provider_account_email,
        authentication_kind: attempt.provider_account_authentication_kind,
        started_at: occurred_at,
        completed_at: Some(occurred_at),
    }
}

pub(crate) fn usage_detail_view(detail: domain::UsageDetail) -> UsageRecordDetailView {
    UsageRecordDetailView {
        request: usage_record_view(detail.request),
        attempts: detail
            .attempts
            .into_iter()
            .map(usage_attempt_view)
            .collect(),
        attempts_complete: false,
    }
}

pub(crate) fn page_meta(page: u32, page_size: u16, total: u64) -> PageMeta {
    let page_size_u64 = u64::from(page_size);
    let total_pages = total.saturating_add(page_size_u64 - 1) / page_size_u64;
    PageMeta::new(
        page,
        u32::from(page_size),
        total,
        u32::try_from(total_pages).unwrap_or(u32::MAX),
    )
}

pub(crate) fn usage_page_view(
    page: domain::UsagePage,
    page_number: u32,
    page_size: u16,
) -> Result<PageData<UsageRecordView>, WireValidationError> {
    Ok(PageData {
        items: page.items.into_iter().map(usage_record_view).collect(),
        page: page_meta(page_number, page_size, page.total),
        next_cursor: page
            .next_cursor
            .as_ref()
            .map(encode_observability_cursor)
            .transpose()?,
    })
}

pub(crate) fn ops_error_view(error: domain::OpsError) -> OpsErrorView {
    let account_label = error
        .provider_account_email
        .as_ref()
        .or(error.provider_account_name.as_ref())
        .or(error.provider_account_ref.as_ref())
        .cloned();
    OpsErrorView {
        id: error.event_id,
        request_id: error.request_id,
        client_api_key_id: error.client_api_key_ref,
        kind: error.component.clone(),
        provider: error.provider_kind,
        authentication_kind: error.provider_account_authentication_kind,
        account_id: error.provider_account_ref,
        route: error.endpoint.unwrap_or_else(|| "—".to_owned()),
        model: error.upstream_model_id,
        client_status_code: error.client_status_code.map(i64::from),
        upstream_status_code: error.upstream_status_code.map(i64::from),
        transport: error.upstream_transport,
        attempt_index: error.attempt_index,
        failure_class: error.failure_kind,
        response_id: error.client_response_id,
        upstream_request_id: error.upstream_request_id,
        latency_ms: error.latency_ms,
        message: error.message,
        metadata: OpsErrorMetadataView {
            source: error.source,
            component: error.component,
            attempt_id: None,
            account_label,
        },
        created_at: error.occurred_at,
        created_at_display: china_datetime(&error.occurred_at),
    }
}

pub(crate) fn ops_page_view(
    page: domain::OpsErrorPage,
    page_number: u32,
    page_size: u16,
) -> Result<PageData<OpsErrorView>, WireValidationError> {
    Ok(PageData {
        items: page.items.into_iter().map(ops_error_view).collect(),
        page: page_meta(page_number, page_size, page.total),
        next_cursor: page
            .next_cursor
            .as_ref()
            .map(encode_observability_cursor)
            .transpose()?,
    })
}

pub(crate) fn trend_point_view(point: domain::TrendPoint) -> TrendPointView {
    let local_time = china_label(point.bucket_start, "%H:%M");
    let label = china_label(point.bucket_start, "%m-%d %H:%M");
    let success_rate_value = point.success_rate.map(|value| value * 100.0);
    TrendPointView {
        time: local_time,
        bucket: point.bucket_start,
        label,
        requests: format_compact_number(point.metrics.request_count),
        requests_value: point.metrics.request_count,
        input_tokens: format_compact_number(point.metrics.input_tokens),
        input_tokens_value: point.metrics.input_tokens,
        output_tokens: format_compact_number(point.metrics.output_tokens),
        output_tokens_value: point.metrics.output_tokens,
        cached_tokens: format_compact_number(point.metrics.cached_tokens),
        cached_tokens_value: point.metrics.cached_tokens,
        cache_hit_rate_value: point.cached_token_rate,
        tokens_value: point.metrics.total_tokens,
        errors: format_compact_number(point.service_failure_count),
        errors_value: point.service_failure_count,
        latency: display_duration(point.average_latency_ms),
        latency_value: point.average_latency_ms,
        max_latency: display_duration(point.metrics.max_latency_ms),
        max_latency_value: point.metrics.max_latency_ms,
        min_latency: display_duration(point.metrics.min_latency_ms),
        min_latency_value: point.metrics.min_latency_ms,
        success_rate: success_rate_value
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}%")),
        success_rate_value,
    }
}

pub(crate) fn trend_summary_view(
    kind: TrendKind,
    summary: &domain::TrendSummary,
) -> Vec<TrendSummaryView> {
    match kind {
        TrendKind::Usage => vec![
            TrendSummaryView {
                label: "输入".to_owned(),
                value: format_compact_number(summary.input_tokens),
                ratio: None,
            },
            TrendSummaryView {
                label: "输出".to_owned(),
                value: format_compact_number(summary.output_tokens),
                ratio: None,
            },
            TrendSummaryView {
                label: "缓存".to_owned(),
                value: format_compact_number(summary.cached_tokens),
                ratio: None,
            },
        ],
        TrendKind::Latency => vec![
            TrendSummaryView {
                label: "平均".to_owned(),
                value: display_duration(summary.average_latency_ms),
                ratio: None,
            },
            TrendSummaryView {
                label: "最高".to_owned(),
                value: display_duration(summary.max_latency_ms),
                ratio: None,
            },
            TrendSummaryView {
                label: "最低".to_owned(),
                value: display_duration(summary.min_latency_ms),
                ratio: None,
            },
        ],
        TrendKind::Errors => vec![
            TrendSummaryView {
                label: "错误数".to_owned(),
                value: format_compact_number(summary.service_failure_count),
                ratio: None,
            },
            TrendSummaryView {
                label: "成功率".to_owned(),
                value: "—".to_owned(),
                ratio: summary
                    .success_rate
                    .map(|value| format!("{:.1}%", value * 100.0)),
            },
            TrendSummaryView {
                label: "总请求".to_owned(),
                value: format_compact_number(summary.request_count),
                ratio: None,
            },
        ],
    }
}

pub(crate) fn trend_view(trend: domain::Trend, kind: TrendKind) -> TrendData {
    TrendData {
        kind,
        summary: trend_summary_view(kind, &trend.summary),
        points: trend.points.into_iter().map(trend_point_view).collect(),
    }
}

pub(crate) fn health_status_name(status: domain::HealthStatus) -> &'static str {
    match status {
        domain::HealthStatus::Future => "future",
        domain::HealthStatus::NoData => "no_data",
        domain::HealthStatus::Unavailable => "unavailable",
        domain::HealthStatus::LowSample => "low_sample",
        domain::HealthStatus::Unstable => "unstable",
        domain::HealthStatus::Stable => "stable",
    }
}

pub(crate) fn reliability_display(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{value:.1}%"))
}

pub(crate) fn health_timeline_view(timeline: domain::HealthTimeline) -> HealthTimelineView {
    HealthTimelineView {
        title: "请求健康时间线".to_owned(),
        description: "有效请求可用性".to_owned(),
        reliability_display: reliability_display(timeline.reliability_percent),
        status: health_status_name(timeline.status).to_owned(),
        success_requests: timeline.success_requests,
        failed_requests: timeline.failed_requests,
        cancelled_requests: timeline.cancelled_requests,
        incomplete_requests: timeline.incomplete_requests,
        caller_error_requests: timeline.caller_error_requests,
        points: timeline
            .points
            .into_iter()
            .enumerate()
            .map(|(index, point)| {
                let elapsed_minutes = i64::try_from(index).unwrap_or(i64::MAX).saturating_mul(15);
                HealthTimelinePointView {
                    time: format!("{:02}:{:02}", elapsed_minutes / 60, elapsed_minutes % 60),
                    status: health_status_name(point.status).to_owned(),
                    reliability_display: reliability_display(point.reliability_percent),
                    success_requests: point.success_requests,
                    failed_requests: point.failed_requests,
                    cancelled_requests: point.cancelled_requests,
                    incomplete_requests: point.incomplete_requests,
                    caller_error_requests: point.caller_error_requests,
                }
            })
            .collect(),
    }
}

pub(crate) fn wire_profile_view(profile: domain::DashboardWireProfile) -> DashboardWireProfileView {
    DashboardWireProfileView {
        provider: profile.provider,
        product: profile.product,
        version: profile.version,
        build: profile.build,
        target: DashboardWireTargetView {
            os_type: profile.target.os_type,
            os_version: profile.target.os_version,
            arch: profile.target.arch,
            terminal: profile.target.terminal,
        },
        user_agent: profile.user_agent,
        attributes: profile
            .attributes
            .into_iter()
            .map(|attribute| DashboardWireAttributeView {
                label: attribute.label,
                value: attribute.value,
            })
            .collect(),
        verified_at: profile.verified_at,
        release: profile.release.map(|release| DashboardDesktopReleaseView {
            status: release.status.into(),
            checked_at: release.checked_at,
            latest_version: release.latest_version,
            latest_build: release.latest_build,
            error: release.error,
        }),
    }
}

pub(crate) fn relative_time(value: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(value) = value else {
        return "从未使用".to_owned();
    };
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

pub(crate) fn dashboard_view(
    result: domain::DashboardResult,
    kind: TrendKind,
) -> DashboardDataView {
    let domain::DashboardResult {
        observation,
        today,
        yesterday,
        total_billing_usd,
        total_cached_token_rate,
        average_first_token_latency_ms,
        trend,
        health_timeline,
        wire_profiles,
        capacity,
        rotation_strategy,
    } = result;
    let domain::DashboardObservation {
        range,
        totals,
        provider_accounts,
        account_usage,
        recent_requests,
        ..
    } = observation;
    let mut account_usage_views = Vec::with_capacity(account_usage.len());
    for credential in account_usage {
        account_usage_views.push(DashboardAccountUsageView {
            id: credential.account_id.clone(),
            provider: credential.provider_kind.clone(),
            authentication_kind: credential.authentication_kind.clone(),
            email: credential
                .email
                .clone()
                .unwrap_or_else(|| credential.name.clone()),
            plan_type: credential.plan_type.clone(),
            tokens: credential
                .total_tokens
                .map_or_else(|| "—".to_owned(), format_compact_number),
            request_count: credential.request_count,
            request_buckets: credential
                .request_buckets
                .iter()
                .map(|bucket| DashboardAccountRequestBucketView {
                    bucket_start: bucket.bucket_start,
                    request_count: bucket.request_count,
                })
                .collect(),
            quota_used_percent: credential.quota_used_percent,
            last_used: relative_time(credential.last_used_at, range.end),
        });
    }
    DashboardDataView {
        cards: DashboardCardsView {
            credentials: DashboardCredentialsCardView {
                total: format_compact_number(provider_accounts.total),
                total_value: provider_accounts.total,
                available: format_compact_number(provider_accounts.normal),
                available_value: provider_accounts.normal,
                unavailable: format_compact_number(
                    provider_accounts
                        .total
                        .saturating_sub(provider_accounts.normal),
                ),
                unavailable_value: provider_accounts
                    .total
                    .saturating_sub(provider_accounts.normal),
            },
            traffic: DashboardTrafficCardView {
                today_requests: format_compact_number(today.request_count),
                today_requests_value: today.request_count,
                yesterday_requests_value: yesterday.request_count,
                total_requests: format_compact_number(totals.request_count),
            },
            tokens: DashboardTokensCardView {
                today_tokens: format_compact_number(today.total_tokens),
                today_tokens_value: today.total_tokens,
                yesterday_tokens_value: yesterday.total_tokens,
                total_tokens: format_compact_number(totals.total_tokens),
                total_billing_amount_usd: total_billing_usd
                    .as_ref()
                    .map_or_else(|| "—".to_owned(), |amount| format!("${}", amount.as_str())),
            },
            cache: DashboardCacheCardView {
                today_hit_rate: display_rate(today.cached_token_rate),
                today_hit_rate_value: today.observed_cached_token_rate,
                yesterday_hit_rate_value: yesterday.observed_cached_token_rate,
                total_hit_rate: display_rate(total_cached_token_rate),
                total_cached_tokens: format_compact_number(totals.cached_tokens),
                average_first_token_latency_ms: display_duration(average_first_token_latency_ms),
            },
        },
        trend: trend_view(trend, kind),
        health_timeline: health_timeline_view(health_timeline),
        wire_profiles: wire_profiles.into_iter().map(wire_profile_view).collect(),
        account_usage: account_usage_views,
        usage_records: recent_requests.into_iter().map(usage_record_view).collect(),
        pool_summary: DashboardPoolSummaryView {
            total: provider_accounts.total,
            normal: provider_accounts.normal,
            quota_exhausted: provider_accounts.quota_exhausted,
            rate_limited: provider_accounts.rate_limited,
            disabled: provider_accounts.disabled,
            error: provider_accounts.error,
        },
        capacity_info: DashboardCapacityInfoView {
            max_concurrent_per_account: capacity.max_concurrent_per_account,
            total_slots: capacity.total_slots,
            used_slots: capacity.used_slots,
            available_slots: capacity.available_slots,
        },
        rotation_strategy: rotation_strategy.as_str().to_owned(),
    }
}

pub(crate) fn usage_summary_view(summary: domain::UsageSummary) -> UsageSummaryView {
    let overview = summary.overview;
    UsageSummaryView {
        total_requests: format_compact_number(overview.requests.request_count),
        input_tokens: format_compact_number(overview.requests.input_tokens),
        output_tokens: format_compact_number(overview.requests.output_tokens),
        cached_tokens: format_compact_number(overview.requests.cached_tokens),
        cache_write_tokens: format_compact_number(overview.requests.cache_write_tokens),
        total_tokens: format_compact_number(overview.requests.total_tokens),
        average_latency_ms: display_duration(summary.average_latency_ms),
        logical_requests: request_metrics_view(&overview.requests),
        attempts: attempt_metrics_view(&overview.attempts),
    }
}

pub(crate) fn usage_insights_view(insights: domain::UsageInsights) -> UsageInsightsOverviewView {
    let health_points = insights
        .health
        .points
        .iter()
        .map(|point| OverviewHealthPointView {
            bucket: point.bucket_start,
            label: china_label(point.bucket_start, "%m-%d %H:%M"),
            success_requests: point.success_requests,
            failed_requests: point.failed_requests,
            cancelled_requests: point.cancelled_requests,
            incomplete_requests: point.incomplete_requests,
            caller_error_requests: point.caller_error_requests,
            error_rate: point.error_rate,
        })
        .collect();
    let performance_points = insights
        .performance
        .points
        .iter()
        .map(|point| OverviewPerformancePointView {
            bucket: point.bucket_start,
            label: china_label(point.bucket_start, "%m-%d %H:%M"),
            latency_p50_ms: point.latency_percentiles.p50_ms.map(|value| value.as_f64()),
            latency_p95_ms: point.latency_percentiles.p95_ms.map(|value| value.as_f64()),
            latency_p99_ms: point.latency_percentiles.p99_ms.map(|value| value.as_f64()),
            first_token_p50_ms: point
                .first_token_latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            first_token_p95_ms: point
                .first_token_latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            first_token_p99_ms: point
                .first_token_latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
        })
        .collect();
    let cost_points = insights
        .cost
        .points
        .iter()
        .map(|point| OverviewCostPointView {
            bucket: point.bucket_start,
            label: china_label(point.bucket_start, "%m-%d %H:%M"),
            input_tokens: point.input_tokens,
            output_tokens: point.output_tokens,
            cached_tokens: point.cached_tokens,
            total_tokens: point.total_tokens,
            estimated_cost: point.estimated_cost.as_ref().map(ToString::to_string),
            standard_cost: point.standard_cost.as_ref().map(ToString::to_string),
            cached_token_rate: point.cached_token_rate,
            cache_hit_request_rate: point.cache_hit_request_rate,
        })
        .collect();
    UsageInsightsOverviewView {
        granularity: granularity_name(insights.granularity).to_owned(),
        health: OverviewHealthView {
            total_requests: insights.health.total_requests,
            success_requests: insights.health.success_requests,
            failed_requests: insights.health.failed_requests,
            cancelled_requests: insights.health.cancelled_requests,
            incomplete_requests: insights.health.incomplete_requests,
            caller_error_requests: insights.health.caller_error_requests,
            success_rate: insights.health.success_rate,
            request_change_rate: None,
            success_rate_change: None,
            points: health_points,
        },
        performance: OverviewPerformanceView {
            latency_p50_ms: insights
                .performance
                .latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            latency_p95_ms: insights
                .performance
                .latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            latency_p99_ms: insights
                .performance
                .latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
            first_token_p50_ms: insights
                .performance
                .first_token_latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            first_token_p95_ms: insights
                .performance
                .first_token_latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            first_token_p99_ms: insights
                .performance
                .first_token_latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
            latency_coverage: insights.performance.latency_coverage,
            first_token_coverage: insights.performance.first_token_coverage,
            points: performance_points,
        },
        cost: OverviewCostView {
            estimated_cost: insights
                .cost
                .estimated_cost
                .as_ref()
                .map(ToString::to_string),
            standard_cost: insights
                .cost
                .standard_cost
                .as_ref()
                .map(ToString::to_string),
            cost_per_request: insights
                .cost
                .cost_per_request
                .as_ref()
                .map(ToString::to_string),
            tokens_per_request: insights.cost.tokens_per_request,
            cached_token_rate: insights.cost.cached_token_rate,
            cache_hit_request_rate: insights.cost.cache_hit_request_rate,
            input_tokens: insights.cost.input_tokens,
            output_tokens: insights.cost.output_tokens,
            cached_tokens: insights.cost.cached_tokens,
            total_tokens: insights.cost.total_tokens,
            points: cost_points,
            costs: cost_views(&insights.cost.costs),
            coverage: cost_coverage_view(&insights.cost.coverage),
        },
        attempts: attempt_metrics_view(&insights.attempts),
        providers: insights
            .providers
            .into_iter()
            .map(|provider| ProviderOverviewView {
                provider: provider.provider_kind,
                request_count: provider.request_count,
                attempt_count: provider.attempt_count,
                failure_count: provider.failure_count,
                total_tokens: provider.total_tokens,
            })
            .collect(),
    }
}

pub(crate) fn granularity_name(granularity: domain::Granularity) -> &'static str {
    match granularity {
        domain::Granularity::FifteenMinutes => "15m",
        domain::Granularity::Hour => "1h",
        domain::Granularity::Day => "1d",
    }
}

pub(crate) fn diagnostics_view(
    result: domain::DiagnosticsResult,
    dimension: DiagnosticDimension,
) -> DiagnosticsView {
    DiagnosticsView {
        dimension: dimension.display_name().to_owned(),
        items: result
            .items
            .into_iter()
            .map(|item| DiagnosticItemView {
                key: item.key,
                name: if item.name == "__none__" {
                    "未知".to_owned()
                } else {
                    item.name
                },
                request_count: item.request_count,
                success_count: item.success_count,
                error_count: item.error_count,
                error_rate: item.error_rate,
                request_share: item.request_share,
                average_latency_ms: item.average_latency_ms,
                latency_p95_ms: item.latency_p95_ms,
                estimated_cost: item.estimated_cost.as_ref().map(ToString::to_string),
                attempt_count: item.attempt_count,
                total_tokens: item.total_tokens,
            })
            .collect(),
    }
}

pub(crate) fn map_wire_error(error: WireValidationError) -> AdminError {
    let message = match error.field() {
        "timeRange" => "Invalid time range",
        "statusCode" => "Status code must be between 100 and 599",
        "attemptIndex" => "Attempt index is out of range",
        "kind" => "Invalid dashboard trend kind",
        "dimension" => "Invalid diagnostics dimension",
        "outcome" => "Invalid Observability query",
        "id" => "Usage record ID is required",
        "page" | "pageSize" => "Invalid Observability query",
        "cursor" => "Invalid observability cursor",
        _ => "Invalid observability query",
    };
    AdminError::bad_request(message)
}

pub(crate) fn map_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    map_admin_service_error(error, "Observability repository unavailable")
}
