//! Store 与 Admin 领域之间的无格式化转换及 row 解析。

use super::*;

pub(crate) fn store_range(
    range: admin_observability::TimeRange,
) -> AdminStoreResult<ObservabilityRange> {
    ObservabilityRange::new(range.start, range.end).map_err(observability_error)
}

pub(crate) fn store_usage_filter(filter: admin_observability::UsageFilter) -> UsageRecordFilter {
    UsageRecordFilter {
        client_api_key_ref: filter.client_api_key_ref,
        request_id: filter.request_id,
        provider_account_ref: filter.provider_account_ref,
        operation: filter.operation,
        provider_kind: filter.provider_kind,
        model: filter.model,
        outcome: filter.outcome.map(store_request_outcome),
        status_code: filter.status_code,
        transport: filter.transport,
        attempt_index: filter.attempt_index,
        response_id: filter.response_id,
        upstream_request_id: filter.upstream_request_id,
        search: filter.search,
    }
}

pub(crate) fn store_request_outcome(outcome: admin_observability::RequestOutcome) -> String {
    outcome.as_str().to_owned()
}

pub(crate) fn store_usage_query(
    query: admin_observability::UsageQuery,
) -> AdminStoreResult<UsageRecordQuery> {
    Ok(UsageRecordQuery {
        range: store_range(query.range)?,
        filter: store_usage_filter(query.filter),
        current_page: query.current_page,
        page_size: ObservabilityPageSize::new(query.page_size.get())
            .map_err(observability_error)?,
    })
}

pub(crate) fn store_ops_error_filter(
    filter: admin_observability::OpsErrorFilter,
) -> OpsErrorFilter {
    OpsErrorFilter {
        client_api_key_ref: filter.client_api_key_ref,
        request_id: filter.request_id,
        provider_account_ref: filter.provider_account_ref,
        provider_kind: filter.provider_kind,
        operation: filter.operation,
        endpoint: filter.endpoint,
        model: filter.model,
        transport: filter.transport,
        attempt_index: filter.attempt_index,
        response_id: filter.response_id,
        upstream_request_id: filter.upstream_request_id,
        failure_kind: filter.failure_kind,
        status_code: filter.status_code,
        search: filter.search,
    }
}

pub(crate) fn store_ops_error_query(
    query: admin_observability::OpsErrorQuery,
) -> AdminStoreResult<OpsErrorQuery> {
    Ok(OpsErrorQuery {
        range: store_range(query.range)?,
        filter: store_ops_error_filter(query.filter),
        current_page: query.current_page,
        page_size: ObservabilityPageSize::new(query.page_size.get())
            .map_err(observability_error)?,
    })
}

pub(crate) const fn store_diagnostic_dimension(
    dimension: admin_observability::DiagnosticDimension,
) -> DiagnosticDimension {
    match dimension {
        admin_observability::DiagnosticDimension::Provider => DiagnosticDimension::Provider,
        admin_observability::DiagnosticDimension::Model => DiagnosticDimension::Model,
        admin_observability::DiagnosticDimension::Account => DiagnosticDimension::Account,
        admin_observability::DiagnosticDimension::ApiKey => DiagnosticDimension::ApiKey,
        admin_observability::DiagnosticDimension::Transport => DiagnosticDimension::Transport,
        admin_observability::DiagnosticDimension::Failure => DiagnosticDimension::Failure,
        admin_observability::DiagnosticDimension::Status => DiagnosticDimension::Status,
    }
}

pub(crate) fn admin_dashboard_observation(
    observation: DashboardObservation,
) -> AdminStoreResult<admin_observability::DashboardObservation> {
    let DashboardObservation {
        range,
        totals,
        provider_accounts,
        trend,
        account_usage,
        recent_requests,
    } = observation;
    Ok(admin_observability::DashboardObservation {
        range: admin_range(range),
        totals: admin_dashboard_totals(totals)?,
        provider_accounts: admin_account_pool_metrics(provider_accounts),
        trend: trend
            .into_iter()
            .map(admin_request_metric_point)
            .collect::<AdminStoreResult<_>>()?,
        account_usage: account_usage
            .into_iter()
            .map(admin_dashboard_account_usage)
            .collect::<AdminStoreResult<_>>()?,
        recent_requests: recent_requests
            .into_iter()
            .map(admin_usage_list_record)
            .collect::<AdminStoreResult<_>>()?,
    })
}

pub(crate) fn admin_dashboard_totals(
    totals: DashboardTotals,
) -> AdminStoreResult<admin_observability::DashboardTotals> {
    Ok(admin_observability::DashboardTotals {
        request_count: totals.request_count,
        input_tokens: totals.input_tokens,
        cached_tokens: totals.cached_tokens,
        total_tokens: totals.total_tokens,
        billing_usd: admin_optional_decimal_amount(totals.billing_usd)?,
    })
}

pub(crate) const fn admin_range(range: ObservabilityRange) -> admin_observability::TimeRange {
    admin_observability::TimeRange {
        start: range.start,
        end: range.end,
    }
}

pub(crate) fn admin_request_metrics(
    metrics: RequestMetrics,
) -> AdminStoreResult<admin_observability::RequestMetrics> {
    Ok(admin_observability::RequestMetrics {
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
        first_token_latency_sum_ms: metrics.first_token_latency_sum,
        first_token_latency_count: metrics.first_token_latency_count,
        latency_sum_ms: metrics.latency_sum,
        latency_count: metrics.latency_count,
        min_latency_ms: metrics.min_latency_ms,
        max_latency_ms: metrics.max_latency_ms,
        latency_percentiles: admin_latency_percentiles(metrics.latency_percentiles)?,
        first_token_latency_percentiles: admin_latency_percentiles(
            metrics.first_token_latency_percentiles,
        )?,
        admission_decision_count: metrics.admission_decision_count,
        admission_decision_percentiles: admin_latency_percentiles(
            metrics.admission_decision_percentiles,
        )?,
        account_selection_wait_count: metrics.account_selection_wait_count,
        account_selection_wait_percentiles: admin_latency_percentiles(
            metrics.account_selection_wait_percentiles,
        )?,
        output_throughput_p10: metrics.output_throughput_p10,
        output_throughput_p50: metrics.output_throughput_p50,
        output_throughput_p90: metrics.output_throughput_p90,
        capacity_sample_count: metrics.capacity_sample_count,
        capacity_utilization_avg_basis_points: metrics.capacity_utilization_avg_basis_points,
        capacity_utilization_p95_basis_points: metrics.capacity_utilization_p95_basis_points,
        cache_eligible_request_count: metrics.cache_eligible_request_count,
        cache_hit_request_count: metrics.cache_hit_request_count,
    })
}

pub(crate) fn admin_latency_percentiles(
    percentiles: LatencyPercentiles,
) -> AdminStoreResult<admin_observability::LatencyPercentiles> {
    Ok(admin_observability::LatencyPercentiles {
        p50_ms: percentiles
            .p50_ms
            .map(|value| admin_observability::PercentileMilliseconds::new(value.as_f64()))
            .transpose()
            .map_err(|_| invalid_admin_percentile())?,
        p95_ms: percentiles
            .p95_ms
            .map(|value| admin_observability::PercentileMilliseconds::new(value.as_f64()))
            .transpose()
            .map_err(|_| invalid_admin_percentile())?,
        p99_ms: percentiles
            .p99_ms
            .map(|value| admin_observability::PercentileMilliseconds::new(value.as_f64()))
            .transpose()
            .map_err(|_| invalid_admin_percentile())?,
    })
}

pub(crate) fn invalid_admin_percentile() -> gateway_admin::ports::store::AdminStoreError {
    observability_error(StoreError::InvalidData {
        entity: "observability latency percentile",
        message: "latency percentile is outside the admin contract".to_owned(),
    })
}

pub(crate) fn admin_attempt_metrics(
    metrics: AttemptMetrics,
) -> AdminStoreResult<admin_observability::AttemptMetrics> {
    Ok(admin_observability::AttemptMetrics {
        attempt_count: metrics.attempt_count,
        success_count: metrics.success_count,
        failure_count: metrics.failure_count,
        cancelled_count: metrics.cancelled_count,
        incomplete_count: metrics.incomplete_count,
        rate_limited_count: metrics.rate_limited_count,
        auth_failure_count: metrics.auth_failure_count,
        provider_5xx_count: metrics.provider_5xx_count,
        cost_coverage: admin_cost_coverage(metrics.cost_coverage),
        costs: admin_currency_costs(metrics.costs)?,
    })
}

pub(crate) const fn admin_cost_coverage(
    coverage: CostCoverage,
) -> admin_observability::CostCoverage {
    admin_observability::CostCoverage {
        provider_reported_count: coverage.provider_reported_count,
        calculated_count: coverage.calculated_count,
        partial_count: 0,
        unavailable_count: coverage.unavailable_count,
        not_billable_count: 0,
    }
}

pub(crate) fn admin_currency_costs(
    costs: Vec<CurrencyCostTotal>,
) -> AdminStoreResult<Vec<admin_observability::CurrencyCost>> {
    costs
        .into_iter()
        .map(|cost| {
            Ok(admin_observability::CurrencyCost {
                currency: cost.currency,
                amount: admin_decimal_amount(cost.amount)?,
            })
        })
        .collect()
}

pub(crate) fn admin_decimal_amount(
    amount: DecimalAmount,
) -> AdminStoreResult<admin_observability::DecimalAmount> {
    admin_observability::DecimalAmount::from_str(amount.as_str()).map_err(|_| {
        observability_error(StoreError::InvalidData {
            entity: "observability decimal amount",
            message: "amount is outside the admin numeric contract".to_owned(),
        })
    })
}

pub(crate) fn admin_optional_decimal_amount(
    amount: Option<DecimalAmount>,
) -> AdminStoreResult<Option<admin_observability::DecimalAmount>> {
    amount.map(admin_decimal_amount).transpose()
}

pub(crate) const fn admin_account_pool_metrics(
    metrics: ProviderAccountMetrics,
) -> admin_observability::AccountPoolMetrics {
    admin_observability::AccountPoolMetrics {
        total: metrics.total,
        normal: metrics.normal,
        quota_exhausted: metrics.quota_exhausted,
        rate_limited: metrics.rate_limited,
        disabled: metrics.disabled,
        error: metrics.error,
    }
}

pub(crate) fn admin_dashboard_account_usage(
    usage: ProviderAccountUsageObservation,
) -> AdminStoreResult<admin_observability::DashboardAccountUsage> {
    Ok(admin_observability::DashboardAccountUsage {
        account_id: usage.account_id,
        provider_kind: usage.provider_kind,
        authentication_kind: usage.authentication_kind,
        name: usage.name,
        email: usage.email,
        plan_type: usage.plan_type,
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
        cost_coverage: admin_cost_coverage(usage.cost_coverage),
        costs: admin_currency_costs(usage.costs)?,
        last_used_at: usage.last_used_at,
        request_buckets: usage
            .request_buckets
            .into_iter()
            .map(
                |bucket| admin_observability::DashboardAccountRequestBucket {
                    bucket_start: bucket.bucket_start,
                    request_count: bucket.request_count,
                },
            )
            .collect(),
        quota_used_percent: None,
        models: usage
            .models
            .into_iter()
            .map(admin_dashboard_account_model_usage)
            .collect::<AdminStoreResult<_>>()?,
    })
}

pub(crate) fn admin_dashboard_account_model_usage(
    usage: ProviderAccountModelUsageObservation,
) -> AdminStoreResult<admin_observability::DashboardAccountModelUsage> {
    Ok(admin_observability::DashboardAccountModelUsage {
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
        cost_coverage: admin_cost_coverage(usage.cost_coverage),
        costs: admin_currency_costs(usage.costs)?,
        last_used_at: usage.last_used_at,
    })
}

pub(crate) fn admin_request_metric_point(
    point: RequestMetricPoint,
) -> AdminStoreResult<admin_observability::RequestMetricPoint> {
    Ok(admin_observability::RequestMetricPoint {
        bucket_start: point.bucket_start,
        granularity: admin_granularity(point.granularity),
        metrics: admin_request_metrics(point.metrics)?,
        cost_coverage: admin_cost_coverage(point.cost_coverage),
        costs: admin_currency_costs(point.costs)?,
    })
}

pub(crate) fn admin_calculated_usage_billing_fact(
    fact: CalculatedUsageBillingFact,
) -> AdminStoreResult<admin_observability::UsageCalculatedBillingFact> {
    Ok(admin_observability::UsageCalculatedBillingFact {
        bucket_start: fact.bucket_start,
        provider_kind: fact.provider_kind,
        upstream_model_id: fact.upstream_model_id,
        service_tier: fact.service_tier,
        input_tokens: fact.input_tokens,
        output_tokens: fact.output_tokens,
        cached_tokens: fact.cached_tokens,
        cache_write_tokens: fact.cache_write_tokens,
        total: admin_observability::CurrencyCost {
            currency: fact.total.currency,
            amount: admin_decimal_amount(fact.total.amount)?,
        },
    })
}

pub(crate) const fn admin_granularity(
    granularity: ObservationGranularity,
) -> admin_observability::Granularity {
    match granularity {
        ObservationGranularity::FifteenMinutes => admin_observability::Granularity::FifteenMinutes,
        ObservationGranularity::Hour => admin_observability::Granularity::Hour,
        ObservationGranularity::Day => admin_observability::Granularity::Day,
    }
}

pub(crate) fn admin_usage_page(
    page: UsageRecordPage,
) -> AdminStoreResult<admin_observability::UsagePage> {
    Ok(admin_observability::UsagePage {
        items: page
            .items
            .into_iter()
            .map(admin_usage_list_record)
            .collect::<AdminStoreResult<_>>()?,
        current_page: page.current_page,
        page_size: page.page_size,
        total: page.total,
    })
}

pub(crate) fn admin_usage_list_record(
    record: UsageListRecord,
) -> AdminStoreResult<admin_observability::UsageListRecord> {
    let billing = match (&record.cost_amount, &record.cost_currency) {
        (Some(amount), Some(currency)) => Some(admin_observability::UsageBilling::Total {
            source: record.cost_source.clone(),
            total: admin_observability::CurrencyCost {
                currency: currency.clone(),
                amount: admin_decimal_amount(amount.clone())?,
            },
        }),
        (None, None) => None,
        _ => {
            return Err(observability_error(StoreError::InvalidData {
                entity: "observability request billing",
                message: "cost amount and currency must be present together".to_owned(),
            }));
        }
    };
    Ok(admin_observability::UsageListRecord {
        id: record.id,
        endpoint: record.endpoint,
        client_transport: record.client_transport,
        requested_model_id: record.requested_model_id,
        provider_kind: record.provider_kind,
        provider_account_ref: record.provider_account_ref,
        provider_account_name: record.provider_account_name,
        provider_account_email: record.provider_account_email,
        provider_account_authentication_kind: record.provider_account_authentication_kind,
        upstream_model_id: record.upstream_model_id,
        upstream_transport: record.upstream_transport,
        service_tier: record.service_tier,
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        cache_write_tokens: record.cache_write_tokens,
        reasoning_tokens: record.reasoning_tokens,
        image_input_tokens: record.image_input_tokens,
        image_output_tokens: record.image_output_tokens,
        total_tokens: record.total_tokens,
        cost_source: record.cost_source,
        cost_amount: admin_optional_decimal_amount(record.cost_amount)?,
        cost_currency: record.cost_currency,
        billing,
        transport_decision_wait_ms: record.transport_decision_wait_ms,
        connect_ms: record.connect_ms,
        headers_ms: record.headers_ms,
        first_event_ms: record.first_event_ms,
        first_reasoning_ms: record.first_reasoning_ms,
        first_text_ms: record.first_text_ms,
        first_token_ms: record.first_token_ms,
        provider_processing_ms: record.provider_processing_ms,
        latency_ms: record.latency_ms,
        admission_decision_ms: record.admission_decision_ms,
        account_selection_wait_ms: record.account_selection_wait_ms,
        capacity_used_slots: record.capacity_used_slots,
        capacity_total_slots: record.capacity_total_slots,
        client_ip: record.client_ip,
        user_agent: record.user_agent,
        reasoning_effort: record.reasoning_effort,
        reasoning_preset: record.reasoning_preset,
        subagent_kind: record.subagent_kind,
        compact: record.compact,
        started_at: record.started_at,
    })
}

pub(crate) fn admin_usage_record(
    record: UsageRecord,
) -> AdminStoreResult<admin_observability::UsageRecord> {
    let billing = match (&record.cost_amount, &record.cost_currency) {
        (Some(amount), Some(currency)) => Some(admin_observability::UsageBilling::Total {
            source: record.cost_source.clone(),
            total: admin_observability::CurrencyCost {
                currency: currency.clone(),
                amount: admin_decimal_amount(amount.clone())?,
            },
        }),
        (None, None) => None,
        _ => {
            return Err(observability_error(StoreError::InvalidData {
                entity: "observability request billing",
                message: "cost amount and currency must be present together".to_owned(),
            }));
        }
    };
    Ok(admin_observability::UsageRecord {
        id: record.id,
        client_api_key_ref: record.client_api_key_ref,
        config_revision: record.config_revision,
        routing_scope: record.routing_scope,
        routing_group_refs: record.routing_group_refs,
        routing_group_names_snapshot: record.routing_group_names_snapshot,
        protocol: record.protocol,
        operation: record.operation,
        endpoint: record.endpoint,
        client_transport: record.client_transport,
        requested_model_id: record.requested_model_id,
        provider_kind: record.provider_kind,
        provider_account_ref: record.provider_account_ref,
        provider_account_name: record.provider_account_name,
        provider_account_email: record.provider_account_email,
        provider_account_authentication_kind: record.provider_account_authentication_kind,
        upstream_model_id: record.upstream_model_id,
        upstream_transport: record.upstream_transport,
        http_version: record.http_version,
        websocket_pool: record.websocket_pool,
        service_tier: record.service_tier,
        provider_metadata_json: record.provider_metadata_json,
        attempt_count: record.attempt_count,
        upstream_send_state: record.upstream_send_state,
        downstream_committed_at: record.downstream_committed_at,
        outcome: admin_request_outcome(&record.outcome)?,
        client_status_code: record.client_status_code,
        upstream_status_code: record.upstream_status_code,
        client_response_id: record.client_response_id,
        upstream_request_id: record.upstream_request_id,
        upstream_response_id: record.upstream_response_id,
        error_kind: record.error_kind,
        provider_error_code: record.provider_error_code,
        error_message: record.error_message,
        retry_after_ms: record.retry_after_ms,
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        cache_write_tokens: record.cache_write_tokens,
        reasoning_tokens: record.reasoning_tokens,
        image_input_tokens: record.image_input_tokens,
        image_output_tokens: record.image_output_tokens,
        total_tokens: record.total_tokens,
        cost_source: record.cost_source,
        cost_amount: admin_optional_decimal_amount(record.cost_amount)?,
        cost_currency: record.cost_currency,
        billing,
        transport_decision_wait_ms: record.transport_decision_wait_ms,
        connect_ms: record.connect_ms,
        headers_ms: record.headers_ms,
        first_event_ms: record.first_event_ms,
        first_reasoning_ms: record.first_reasoning_ms,
        first_text_ms: record.first_text_ms,
        first_token_ms: record.first_token_ms,
        provider_processing_ms: record.provider_processing_ms,
        latency_ms: record.latency_ms,
        admission_decision_ms: record.admission_decision_ms,
        account_selection_wait_ms: record.account_selection_wait_ms,
        capacity_used_slots: record.capacity_used_slots,
        capacity_total_slots: record.capacity_total_slots,
        client_ip: record.client_ip,
        user_agent: record.user_agent,
        reasoning_effort: record.reasoning_effort,
        reasoning_preset: record.reasoning_preset,
        request_kind: record.request_kind,
        subagent_kind: record.subagent_kind,
        compact: record.compact,
        image_generation_requested: record.image_generation_requested,
        image_generation_succeeded: record.image_generation_succeeded,
        started_at: record.started_at,
        deadline_at: record.deadline_at,
        completed_at: record.completed_at,
    })
}

pub(crate) fn admin_request_outcome(
    outcome: &str,
) -> AdminStoreResult<admin_observability::RequestOutcome> {
    admin_observability::RequestOutcome::new(outcome.to_owned()).map_err(|_| {
        observability_error(StoreError::InvalidData {
            entity: "observability request outcome",
            message: "invalid request outcome".to_owned(),
        })
    })
}

pub(crate) fn admin_usage_detail(
    detail: UsageRecordDetail,
) -> AdminStoreResult<admin_observability::UsageDetail> {
    Ok(admin_observability::UsageDetail {
        request: admin_usage_record(detail.request)?,
        attempts: detail
            .attempts
            .into_iter()
            .map(admin_usage_attempt)
            .collect::<AdminStoreResult<_>>()?,
    })
}

pub(crate) fn admin_usage_attempt(
    attempt: UsageAttemptObservation,
) -> AdminStoreResult<admin_observability::UsageAttempt> {
    Ok(admin_observability::UsageAttempt {
        source: attempt.source,
        id: attempt.id,
        attempt_index: attempt.attempt_index,
        component: attempt.component,
        operation: attempt.operation,
        provider_kind: attempt.provider_kind,
        provider_account_ref: attempt.provider_account_ref,
        provider_account_name: attempt.provider_account_name,
        provider_account_email: attempt.provider_account_email,
        provider_account_authentication_kind: attempt.provider_account_authentication_kind,
        upstream_model_id: attempt.upstream_model_id,
        upstream_transport: attempt.upstream_transport,
        upstream_send_state: attempt.upstream_send_state,
        outcome: admin_request_outcome(&attempt.outcome)?,
        downstream_committed: attempt.downstream_committed,
        status_code: attempt.status_code,
        provider_error_code: attempt.provider_error_code,
        failure_kind: attempt.failure_kind,
        retry_after_ms: attempt.retry_after_ms,
        upstream_request_id: attempt.upstream_request_id,
        latency_ms: attempt.latency_ms,
        message: attempt.message,
        input_tokens: attempt.input_tokens,
        output_tokens: attempt.output_tokens,
        cached_tokens: attempt.cached_tokens,
        cache_write_tokens: attempt.cache_write_tokens,
        reasoning_tokens: attempt.reasoning_tokens,
        total_tokens: attempt.total_tokens,
        cost_source: attempt.cost_source,
        cost_amount: admin_optional_decimal_amount(attempt.cost_amount)?,
        cost_currency: attempt.cost_currency,
        occurred_at: attempt.occurred_at,
    })
}

pub(crate) fn admin_usage_overview(
    overview: UsageOverview,
) -> AdminStoreResult<admin_observability::UsageOverview> {
    Ok(admin_observability::UsageOverview {
        range: admin_range(overview.range),
        requests: admin_request_metrics(overview.requests)?,
        attempts: admin_attempt_metrics(overview.attempts)?,
        providers: overview
            .providers
            .into_iter()
            .map(admin_provider_observation)
            .collect(),
    })
}

pub(crate) fn admin_provider_observation(
    observation: ProviderObservation,
) -> admin_observability::ProviderObservation {
    admin_observability::ProviderObservation {
        provider_kind: observation.provider_kind,
        request_count: observation.request_count,
        attempt_count: observation.attempt_count,
        failure_count: observation.failure_count,
        total_tokens: observation.total_tokens,
    }
}

pub(crate) fn admin_diagnostic_observation(
    observation: DiagnosticObservation,
) -> AdminStoreResult<admin_observability::DiagnosticObservation> {
    Ok(admin_observability::DiagnosticObservation {
        key: observation.key,
        name: observation.name,
        request_count: observation.request_count,
        success_count: observation.success_count,
        failure_count: observation.failure_count,
        attempt_count: observation.attempt_count,
        total_tokens: observation.total_tokens,
        average_latency_ms: observation.average_latency_ms,
        latency_p95_ms: observation.latency_p95_ms,
        first_token_p95_ms: observation.first_token_p95_ms,
        non_completion_count: observation.non_completion_count,
        retry_count: observation.retry_count,
        cost_coverage: admin_cost_coverage(observation.cost_coverage),
        costs: admin_currency_costs(observation.costs)?,
    })
}

pub(crate) fn admin_ops_error_page(
    page: OpsErrorPage,
) -> AdminStoreResult<admin_observability::OpsErrorPage> {
    Ok(admin_observability::OpsErrorPage {
        items: page.items.into_iter().map(admin_ops_error).collect(),
        current_page: page.current_page,
        page_size: page.page_size,
        total: page.total,
    })
}

pub(crate) fn admin_ops_error(error: OpsErrorRecord) -> admin_observability::OpsError {
    admin_observability::OpsError {
        source: error.source,
        event_id: error.event_id,
        request_id: error.request_id,
        attempt_index: error.attempt_index,
        client_api_key_ref: error.client_api_key_ref,
        component: error.component,
        operation: error.operation,
        protocol: error.protocol,
        client_transport: error.client_transport,
        requested_model_id: error.requested_model_id,
        service_tier: error.service_tier,
        endpoint: error.endpoint,
        provider_kind: error.provider_kind,
        provider_account_ref: error.provider_account_ref,
        provider_account_name: error.provider_account_name,
        provider_account_email: error.provider_account_email,
        provider_account_authentication_kind: error.provider_account_authentication_kind,
        upstream_model_id: error.upstream_model_id,
        upstream_transport: error.upstream_transport,
        failure_kind: error.failure_kind,
        client_status_code: error.client_status_code,
        upstream_status_code: error.upstream_status_code,
        provider_error_code: error.provider_error_code,
        client_response_id: error.client_response_id,
        upstream_request_id: error.upstream_request_id,
        latency_ms: error.latency_ms,
        message: error.message,
        occurrence_count: error.occurrence_count,
        client_ip: error.client_ip,
        user_agent: error.user_agent,
        reasoning_effort: error.reasoning_effort,
        reasoning_preset: error.reasoning_preset,
        request_kind: error.request_kind,
        subagent_kind: error.subagent_kind,
        compact: error.compact,
        occurred_at: error.occurred_at,
        stable_sort_id: error.stable_sort_id,
    }
}

pub(crate) fn usage_list_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<UsageListRecord> {
    Ok(UsageListRecord {
        id: get(row, "id")?,
        endpoint: get(row, "endpoint")?,
        client_transport: get(row, "client_transport")?,
        requested_model_id: get(row, "requested_model_id")?,
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        provider_account_name: get(row, "provider_account_name")?,
        provider_account_email: get(row, "provider_account_email")?,
        provider_account_authentication_kind: get(row, "provider_account_authentication_kind")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: get(row, "upstream_transport")?,
        service_tier: get(row, "service_tier")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_tokens: optional_unsigned(row, "cached_tokens")?,
        cache_write_tokens: optional_unsigned(row, "cache_write_tokens")?,
        reasoning_tokens: optional_unsigned(row, "reasoning_tokens")?,
        image_input_tokens: optional_unsigned(row, "image_input_tokens")?,
        image_output_tokens: optional_unsigned(row, "image_output_tokens")?,
        total_tokens: optional_unsigned(row, "total_tokens")?,
        cost_source: get(row, "cost_source")?,
        cost_amount: optional_decimal(row, "cost_amount")?,
        cost_currency: get(row, "cost_currency")?,
        transport_decision_wait_ms: optional_unsigned(row, "transport_decision_wait_ms")?,
        connect_ms: optional_unsigned(row, "connect_ms")?,
        headers_ms: optional_unsigned(row, "headers_ms")?,
        first_event_ms: optional_unsigned(row, "first_event_ms")?,
        first_reasoning_ms: optional_unsigned(row, "first_reasoning_ms")?,
        first_text_ms: optional_unsigned(row, "first_text_ms")?,
        first_token_ms: optional_unsigned(row, "first_token_ms")?,
        provider_processing_ms: optional_unsigned(row, "provider_processing_ms")?,
        latency_ms: optional_unsigned(row, "latency_ms")?,
        admission_decision_ms: optional_unsigned(row, "admission_decision_ms")?,
        account_selection_wait_ms: optional_unsigned(row, "account_selection_wait_ms")?,
        capacity_used_slots: optional_unsigned(row, "capacity_used_slots")?,
        capacity_total_slots: optional_unsigned(row, "capacity_total_slots")?,
        client_ip: get(row, "client_ip")?,
        user_agent: get(row, "user_agent")?,
        reasoning_effort: get(row, "reasoning_effort")?,
        reasoning_preset: get(row, "reasoning_preset")?,
        subagent_kind: get(row, "subagent_kind")?,
        compact: get(row, "compact")?,
        started_at: get(row, "started_at")?,
    })
}

pub(crate) fn usage_record_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<UsageRecord> {
    Ok(UsageRecord {
        id: get(row, "id")?,
        client_api_key_ref: get(row, "client_api_key_ref")?,
        config_revision: unsigned(row, "config_revision")?,
        routing_scope: get(row, "routing_scope")?,
        routing_group_refs: get(row, "routing_group_refs")?,
        routing_group_names_snapshot: get::<sqlx::types::Json<Vec<String>>>(
            row,
            "routing_group_names_snapshot",
        )?
        .0,
        protocol: get(row, "protocol")?,
        operation: get(row, "operation")?,
        endpoint: get(row, "endpoint")?,
        client_transport: get(row, "client_transport")?,
        requested_model_id: get(row, "requested_model_id")?,
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        provider_account_name: get(row, "provider_account_name")?,
        provider_account_email: get(row, "provider_account_email")?,
        provider_account_authentication_kind: get(row, "provider_account_authentication_kind")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: get(row, "upstream_transport")?,
        http_version: get(row, "http_version")?,
        websocket_pool: get(row, "websocket_pool")?,
        service_tier: get(row, "service_tier")?,
        provider_metadata_json: get::<Option<serde_json::Value>>(row, "provider_observation_json")?
            .map(|value| serde_json::to_string(&value))
            .transpose()
            .map_err(|_| postgres_unavailable("encode provider observation"))?,
        attempt_count: to_u32(get(row, "attempt_count")?)?,
        upstream_send_state: get(row, "upstream_send_state")?,
        downstream_committed_at: get(row, "downstream_committed_at")?,
        outcome: get(row, "outcome")?,
        client_status_code: optional_status(row, "client_status_code")?,
        upstream_status_code: optional_status(row, "upstream_status_code")?,
        client_response_id: opaque_response_id(row, "client_response_id")?,
        upstream_request_id: get(row, "upstream_request_id")?,
        upstream_response_id: opaque_response_id(row, "upstream_response_id")?,
        error_kind: get(row, "error_kind")?,
        provider_error_code: get(row, "provider_error_code")?,
        error_message: get(row, "error_message")?,
        retry_after_ms: optional_unsigned(row, "retry_after_ms")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_tokens: optional_unsigned(row, "cached_tokens")?,
        cache_write_tokens: optional_unsigned(row, "cache_write_tokens")?,
        reasoning_tokens: optional_unsigned(row, "reasoning_tokens")?,
        image_input_tokens: optional_unsigned(row, "image_input_tokens")?,
        image_output_tokens: optional_unsigned(row, "image_output_tokens")?,
        total_tokens: optional_unsigned(row, "total_tokens")?,
        cost_source: get(row, "cost_source")?,
        cost_amount: optional_decimal(row, "cost_amount")?,
        cost_currency: get(row, "cost_currency")?,
        transport_decision_wait_ms: optional_unsigned(row, "transport_decision_wait_ms")?,
        connect_ms: optional_unsigned(row, "connect_ms")?,
        headers_ms: optional_unsigned(row, "headers_ms")?,
        first_event_ms: optional_unsigned(row, "first_event_ms")?,
        first_reasoning_ms: optional_unsigned(row, "first_reasoning_ms")?,
        first_text_ms: optional_unsigned(row, "first_text_ms")?,
        first_token_ms: optional_unsigned(row, "first_token_ms")?,
        provider_processing_ms: optional_unsigned(row, "provider_processing_ms")?,
        latency_ms: optional_unsigned(row, "latency_ms")?,
        admission_decision_ms: optional_unsigned(row, "admission_decision_ms")?,
        account_selection_wait_ms: optional_unsigned(row, "account_selection_wait_ms")?,
        capacity_used_slots: optional_unsigned(row, "capacity_used_slots")?,
        capacity_total_slots: optional_unsigned(row, "capacity_total_slots")?,
        client_ip: get(row, "client_ip")?,
        user_agent: get(row, "user_agent")?,
        reasoning_effort: get(row, "reasoning_effort")?,
        reasoning_preset: get(row, "reasoning_preset")?,
        request_kind: get(row, "request_kind")?,
        subagent_kind: get(row, "subagent_kind")?,
        compact: get(row, "compact")?,
        image_generation_requested: get(row, "image_generation_requested")?,
        image_generation_succeeded: get(row, "image_generation_succeeded")?,
        started_at: get(row, "started_at")?,
        deadline_at: get(row, "deadline_at")?,
        completed_at: get(row, "completed_at")?,
    })
}

pub(crate) fn ops_error_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<OpsErrorRecord> {
    Ok(OpsErrorRecord {
        source: get(row, "source")?,
        event_id: get(row, "event_id")?,
        request_id: get(row, "request_id")?,
        attempt_index: get::<Option<i32>>(row, "attempt_index")?
            .map(to_u32)
            .transpose()?,
        client_api_key_ref: get(row, "client_api_key_ref")?,
        component: get(row, "component")?,
        operation: get(row, "operation")?,
        protocol: get(row, "protocol")?,
        client_transport: get(row, "client_transport")?,
        requested_model_id: get(row, "requested_model_id")?,
        service_tier: get(row, "service_tier")?,
        endpoint: get(row, "endpoint")?,
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        provider_account_name: get(row, "provider_account_name")?,
        provider_account_email: get(row, "provider_account_email")?,
        provider_account_authentication_kind: get(row, "provider_account_authentication_kind")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: get(row, "upstream_transport")?,
        failure_kind: get(row, "failure_kind")?,
        client_status_code: optional_status(row, "client_status_code")?,
        upstream_status_code: optional_status(row, "upstream_status_code")?,
        provider_error_code: get(row, "provider_error_code")?,
        client_response_id: opaque_response_id(row, "client_response_id")?,
        upstream_request_id: get(row, "upstream_request_id")?,
        latency_ms: optional_unsigned(row, "latency_ms")?,
        message: get(row, "message")?,
        occurrence_count: to_u32(get(row, "occurrence_count")?)?,
        client_ip: get(row, "client_ip")?,
        user_agent: get(row, "user_agent")?,
        reasoning_effort: get(row, "reasoning_effort")?,
        reasoning_preset: get(row, "reasoning_preset")?,
        request_kind: get(row, "request_kind")?,
        subagent_kind: get(row, "subagent_kind")?,
        compact: get(row, "compact")?,
        occurred_at: get(row, "occurred_at")?,
        stable_sort_id: get(row, "stable_sort_id")?,
    })
}

pub(crate) fn cost_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<CurrencyCostTotal> {
    Ok(CurrencyCostTotal {
        currency: get(row, "cost_currency")?,
        amount: DecimalAmount::from_str(&get::<String>(row, "amount")?)?,
    })
}

pub(crate) fn calculated_usage_billing_fact_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<CalculatedUsageBillingFact> {
    Ok(CalculatedUsageBillingFact {
        bucket_start: get(row, "bucket_start")?,
        provider_kind: get(row, "provider_kind")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        service_tier: get(row, "service_tier")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_tokens: optional_unsigned(row, "cached_tokens")?,
        cache_write_tokens: optional_unsigned(row, "cache_write_tokens")?,
        total: cost_from_row(row)?,
    })
}

pub(crate) fn optional_decimal(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> StoreResult<Option<DecimalAmount>> {
    get::<Option<String>>(row, column)?
        .map(|value| DecimalAmount::from_str(&value))
        .transpose()
}

pub(crate) fn opaque_response_id(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> StoreResult<Option<String>> {
    get::<Option<Vec<u8>>>(row, column)?
        .map(String::from_utf8)
        .transpose()
        .map_err(|_| invalid(column))
}

pub(crate) fn validate_account_ids(account_ids: &[String]) -> StoreResult<()> {
    let mut unique = BTreeSet::new();
    for account_id in account_ids {
        validate_text(account_id, MAX_FILTER_BYTES, "provider account ID")?;
        if !unique.insert(account_id.as_str()) {
            return Err(invalid("account usage query contains duplicate IDs"));
        }
    }
    Ok(())
}

pub(crate) fn validate_optional_text(
    value: Option<&str>,
    max_bytes: usize,
    field: &'static str,
) -> StoreResult<()> {
    value.map_or(Ok(()), |value| validate_text(value, max_bytes, field))
}

pub(crate) fn validate_text(value: &str, max_bytes: usize, field: &'static str) -> StoreResult<()> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(invalid(&format!("{field} is invalid")));
    }
    Ok(())
}

pub(crate) fn get<'r, T>(row: &'r sqlx::postgres::PgRow, column: &'static str) -> StoreResult<T>
where
    T: sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get(column).map_err(|_| invalid(column))
}

pub(crate) fn unsigned(row: &sqlx::postgres::PgRow, column: &'static str) -> StoreResult<u64> {
    to_u64(get(row, column)?)
}

pub(crate) fn optional_unsigned(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> StoreResult<Option<u64>> {
    get::<Option<i64>>(row, column)?.map(to_u64).transpose()
}

pub(crate) fn optional_status(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> StoreResult<Option<u16>> {
    get::<Option<i32>>(row, column)?
        .map(|value| {
            u16::try_from(value)
                .ok()
                .filter(|value| (100..=599).contains(value))
                .ok_or_else(|| invalid("status code is outside its supported range"))
        })
        .transpose()
}

pub(crate) fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| invalid("numeric observation is negative"))
}

pub(crate) fn to_u32(value: i32) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| invalid("integer observation is negative"))
}

pub(crate) fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "observability",
        message: message.to_owned(),
    }
}
