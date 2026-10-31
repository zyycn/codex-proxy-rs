//! Store 与 Admin 领域之间的无格式化转换及 row 解析

use super::*;

pub(crate) fn store_range(
    range: admin_observability::TimeRange,
) -> StoreResult<ObservabilityRange> {
    // 显式外部范围已经校验；自然日零点的空快照仍须返回零计数
    if range.start == range.end {
        return Ok(ObservabilityRange {
            start: range.start,
            end: range.end,
        });
    }
    ObservabilityRange::new(range.start, range.end)
}

pub(crate) fn store_usage_filter(filter: admin_observability::UsageFilter) -> UsageRecordFilter {
    UsageRecordFilter {
        model_contains: filter.model_contains,
        client_api_key_name: filter.client_api_key_name,
        provider_account_name: filter.provider_account_name,
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
        status: query.status,
        range: store_range(query.range).map_err(observability_error)?,
        filter: store_usage_filter(query.filter),
        current_page: query.current_page,
        page_size: query.page_size,
    })
}

pub(crate) fn store_ops_error_query(
    query: admin_observability::OpsErrorQuery,
) -> AdminStoreResult<OpsErrorQuery> {
    Ok(OpsErrorQuery {
        range: store_range(query.range).map_err(observability_error)?,
        filter: query.filter,
        current_page: query.current_page,
        page_size: query.page_size,
    })
}

pub(crate) fn admin_dashboard_observation(
    observation: DashboardObservation,
) -> admin_observability::DashboardObservation {
    let DashboardObservation {
        analytics,
        range,
        totals,
        yesterday,
        provider_accounts,
        runtime_slots,
        trend,
        account_usage,
        recent_requests,
    } = observation;
    admin_observability::DashboardObservation {
        analytics,
        range: admin_range(range),
        totals,
        yesterday,
        provider_accounts,
        runtime_slots: Some(runtime_slots),
        trend: trend.into_iter().map(admin_request_metric_point).collect(),
        account_usage: account_usage
            .into_iter()
            .map(admin_dashboard_account_usage)
            .collect(),
        recent_requests,
    }
}

pub(crate) const fn admin_range(range: ObservabilityRange) -> admin_observability::TimeRange {
    admin_observability::TimeRange {
        start: range.start,
        end: range.end,
    }
}

pub(crate) fn admin_attempt_metrics(
    metrics: AttemptMetrics,
) -> admin_observability::AttemptMetrics {
    admin_observability::AttemptMetrics {
        attempt_count: metrics.attempt_count,
        success_count: metrics.success_count,
        failure_count: metrics.failure_count,
        cancelled_count: metrics.cancelled_count,
        incomplete_count: metrics.incomplete_count,
        rate_limited_count: metrics.rate_limited_count,
        auth_failure_count: metrics.auth_failure_count,
        provider_5xx_count: metrics.provider_5xx_count,
        cost_coverage: admin_cost_coverage(metrics.cost_coverage),
        costs: metrics.costs,
    }
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

pub(crate) fn admin_dashboard_account_usage(
    usage: ProviderAccountUsageObservation,
) -> admin_observability::DashboardAccountUsage {
    admin_observability::DashboardAccountUsage {
        upstream: None,
        account_id: usage.account_id,
        provider_kind: usage.provider_kind,
        authentication_kind: usage.authentication_kind,
        name: usage.name,
        email: usage.email,
        notes: usage.notes,
        plan_type: usage.plan_type,
        plan_type_display: None,
        request_count: usage.request_count,
        success_count: usage.success_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_input_tokens: usage.cache_write_input_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
        image_request_count: usage.image_request_count,
        image_request_failed_count: usage.image_request_failed_count,
        total_tokens: usage.total_tokens,
        cost_coverage: admin_cost_coverage(usage.cost_coverage),
        costs: usage.costs,
        last_used_at: usage.last_used_at,
        request_buckets: usage.request_buckets,
        quota_used_percent: None,
        quota_window: None,
        models: usage
            .models
            .into_iter()
            .map(admin_account_model_usage)
            .collect(),
    }
}

pub(crate) fn admin_account_model_usage(
    usage: ProviderAccountModelUsageObservation,
) -> admin_observability::AccountModelUsage {
    admin_observability::AccountModelUsage {
        model: usage.model,
        request_count: usage.request_count,
        success_count: usage.success_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_input_tokens: usage.cache_write_input_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
        image_request_count: usage.image_request_count,
        image_request_failed_count: usage.image_request_failed_count,
        total_tokens: usage.total_tokens,
        cost_coverage: admin_cost_coverage(usage.cost_coverage),
        costs: usage.costs,
        last_used_at: usage.last_used_at,
    }
}

pub(crate) fn admin_request_metric_point(
    point: RequestMetricPoint,
) -> admin_observability::RequestMetricPoint {
    admin_observability::RequestMetricPoint {
        bucket_start: point.bucket_start,
        granularity: point.granularity,
        metrics: point.metrics,
        cost_coverage: admin_cost_coverage(point.cost_coverage),
        costs: point.costs,
    }
}

pub(crate) fn admin_calculated_usage_billing_fact(
    fact: CalculatedUsageBillingFact,
) -> admin_observability::UsageCalculatedBillingFact {
    admin_observability::UsageCalculatedBillingFact {
        breakdown: fact
            .billing_snapshot_json
            .as_ref()
            .and_then(super::super::pricing::decode_billing_snapshot),
        bucket_start: fact.bucket_start,
        provider_kind: fact.provider_kind,
        upstream_model_id: fact.upstream_model_id,
        service_tier: fact.service_tier,
        input_tokens: fact.input_tokens,
        output_tokens: fact.output_tokens,
        cached_input_tokens: fact.cached_input_tokens,
        cache_write_input_tokens: fact.cache_write_input_tokens,
        total: fact.total,
    }
}

fn restore_billing_snapshot(
    billing: Option<admin_observability::UsageBilling>,
    snapshot: Option<&serde_json::Value>,
) -> Option<admin_observability::UsageBilling> {
    if let Some(admin_observability::UsageBilling::Total { source, total }) = &billing
        && source == "calculated"
        && let Some(detail) = snapshot.and_then(super::super::pricing::decode_billing_snapshot)
        && &detail.total_amount == total
    {
        return Some(admin_observability::UsageBilling::Calculated(Box::new(
            detail,
        )));
    }
    billing
}

pub(crate) fn request_outcome(outcome: &str) -> StoreResult<admin_observability::RequestOutcome> {
    admin_observability::RequestOutcome::new(outcome.to_owned()).map_err(|_| {
        StoreError::InvalidData {
            source: None,
            entity: "observability request outcome",
            message: "invalid request outcome".to_owned(),
        }
    })
}

pub(crate) fn admin_usage_overview(overview: UsageOverview) -> admin_observability::UsageOverview {
    admin_observability::UsageOverview {
        analytics: overview.analytics,
        range: admin_range(overview.range),
        requests: overview.requests,
        attempts: admin_attempt_metrics(overview.attempts),
        providers: overview.providers,
    }
}

pub(crate) fn admin_diagnostics_observation(
    observation: DiagnosticsObservation,
) -> admin_observability::DiagnosticsObservation {
    admin_observability::DiagnosticsObservation {
        analytics: observation.analytics,
        total_request_count: observation.total_request_count,
        items: observation
            .items
            .into_iter()
            .map(admin_diagnostic_observation)
            .collect(),
    }
}

fn admin_diagnostic_observation(
    observation: DiagnosticObservation,
) -> admin_observability::DiagnosticObservation {
    admin_observability::DiagnosticObservation {
        key: observation.key,
        name: observation.name,
        account_provider_kind: observation.account_provider_kind,
        account_plan_type: observation.account_plan_type,
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
        retried_request_count: observation.retried_request_count,
        cost_coverage: admin_cost_coverage(observation.cost_coverage),
        costs: observation.costs,
    }
}

pub(crate) fn usage_list_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<UsageListRecord> {
    let cost_source: String = get(row, "cost_source")?;
    let cost_amount = optional_decimal(row, "cost_amount")?;
    let cost_currency: Option<String> = get(row, "cost_currency")?;
    let billing = billing_from_row(
        row,
        &cost_source,
        cost_amount.as_ref(),
        cost_currency.as_deref(),
    )?;
    Ok(UsageListRecord {
        error_code: get(row, "error_code")?,
        error_message: get(row, "error_message")?,
        client_status_code: optional_status(row, "client_status_code")?,
        outcome: admin_observability::RequestOutcome::new(get::<String>(row, "outcome")?)
            .map_err(|_| invalid("invalid request outcome"))?,
        client_api_key_name: get(row, "client_api_key_name")?,
        billing,
        id: get(row, "id")?,
        endpoint: get(row, "endpoint")?,
        client_transport: get(row, "client_transport")?,
        requested_model_id: get(row, "requested_model_id")?,
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        provider_account_name: get(row, "provider_account_name")?,
        provider_account_email: get(row, "provider_account_email")?,
        provider_account_notes: get(row, "provider_account_notes")?,
        provider_account_plan_type: get(row, "provider_account_plan_type")?,
        provider_account_plan_type_display: None,
        provider_account_authentication_kind: get(row, "provider_account_authentication_kind")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: get(row, "upstream_transport")?,
        service_tier: get(row, "service_tier")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_input_tokens: optional_unsigned(row, "cached_input_tokens")?,
        cache_write_input_tokens: optional_unsigned(row, "cache_write_input_tokens")?,
        reasoning_output_tokens: optional_unsigned(row, "reasoning_output_tokens")?,
        image_input_tokens: optional_unsigned(row, "image_input_tokens")?,
        image_output_tokens: optional_unsigned(row, "image_output_tokens")?,
        total_tokens: optional_unsigned(row, "total_tokens")?,
        cost_source,
        cost_amount,
        cost_currency,
        transport_decision_wait_ms: optional_unsigned(row, "transport_decision_wait_ms")?,
        connect_ms: optional_unsigned(row, "connect_ms")?,
        headers_ms: optional_unsigned(row, "headers_ms")?,
        first_event_ms: optional_unsigned(row, "first_event_ms")?,
        first_reasoning_ms: optional_unsigned(row, "first_reasoning_ms")?,
        first_text_ms: optional_unsigned(row, "first_text_ms")?,
        first_token_ms: optional_unsigned(row, "first_token_ms")?,
        provider_processing_ms: optional_unsigned(row, "provider_processing_ms")?,
        upstream_response_ms: optional_unsigned(row, "upstream_response_ms")?,
        upstream_api_overhead_ms: get(row, "upstream_api_overhead_ms")?,
        upstream_engine_ms: get(row, "upstream_engine_ms")?,
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
    let cost_source: String = get(row, "cost_source")?;
    let cost_amount = optional_decimal(row, "cost_amount")?;
    let cost_currency: Option<String> = get(row, "cost_currency")?;
    let billing = billing_from_row(
        row,
        &cost_source,
        cost_amount.as_ref(),
        cost_currency.as_deref(),
    )?;
    Ok(UsageRecord {
        billing,
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
            .map_err(|source| postgres_unavailable("encode provider observation", source))?,
        attempt_count: to_u32(get(row, "attempt_count")?)?,
        upstream_send_state: get(row, "upstream_send_state")?,
        downstream_committed_at: get(row, "downstream_committed_at")?,
        outcome: request_outcome(&get::<String>(row, "outcome")?)?,
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
        cached_input_tokens: optional_unsigned(row, "cached_input_tokens")?,
        cache_write_input_tokens: optional_unsigned(row, "cache_write_input_tokens")?,
        reasoning_output_tokens: optional_unsigned(row, "reasoning_output_tokens")?,
        image_input_tokens: optional_unsigned(row, "image_input_tokens")?,
        image_output_tokens: optional_unsigned(row, "image_output_tokens")?,
        total_tokens: optional_unsigned(row, "total_tokens")?,
        cost_source,
        cost_amount,
        cost_currency,
        transport_decision_wait_ms: optional_unsigned(row, "transport_decision_wait_ms")?,
        connect_ms: optional_unsigned(row, "connect_ms")?,
        headers_ms: optional_unsigned(row, "headers_ms")?,
        first_event_ms: optional_unsigned(row, "first_event_ms")?,
        first_reasoning_ms: optional_unsigned(row, "first_reasoning_ms")?,
        first_text_ms: optional_unsigned(row, "first_text_ms")?,
        first_token_ms: optional_unsigned(row, "first_token_ms")?,
        provider_processing_ms: optional_unsigned(row, "provider_processing_ms")?,
        upstream_response_ms: optional_unsigned(row, "upstream_response_ms")?,
        upstream_api_overhead_ms: get(row, "upstream_api_overhead_ms")?,
        upstream_engine_ms: get(row, "upstream_engine_ms")?,
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
        client_api_key_name: get(row, "client_api_key_name")?,
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
        provider_account_plan_type: get(row, "provider_account_plan_type")?,
        provider_account_plan_type_display: None,
        provider_account_authentication_kind: get(row, "provider_account_authentication_kind")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: get(row, "upstream_transport")?,
        failure_kind: get(row, "failure_kind")?,
        upstream_send_state: get(row, "upstream_send_state")?,
        client_status_code: optional_status(row, "client_status_code")?,
        upstream_status_code: optional_status(row, "upstream_status_code")?,
        provider_error_code: get(row, "provider_error_code")?,
        client_response_id: opaque_response_id(row, "client_response_id")?,
        upstream_request_id: get(row, "upstream_request_id")?,
        latency_ms: optional_unsigned(row, "latency_ms")?,
        message: get(row, "message")?,
        error_details: get(row, "error_details")?,
        client_ip: get(row, "client_ip")?,
        user_agent: get(row, "user_agent")?,
        reasoning_effort: get(row, "reasoning_effort")?,
        reasoning_preset: get(row, "reasoning_preset")?,
        request_kind: get(row, "request_kind")?,
        subagent_kind: get(row, "subagent_kind")?,
        compact: get(row, "compact")?,
        continuation_affinity_hash: get(row, "continuation_affinity_hash")?,
        continuation_previous_response_id_hash: get(row, "continuation_previous_response_id_hash")?,
        continuation_unavailable_reason: get(row, "continuation_unavailable_reason")?,
        upstream_connection_id: get(row, "upstream_connection_id")?,
        upstream_connection_exit_reason: get(row, "upstream_connection_exit_reason")?,
        upstream_connection_age_ms: optional_unsigned(row, "upstream_connection_age_ms")?,
        upstream_connection_idle_ms: optional_unsigned(row, "upstream_connection_idle_ms")?,
        recovery_request_id: get(row, "recovery_request_id")?,
        recovered_at: get(row, "recovered_at")?,
        recovery_attempt_count: to_u32(get(row, "recovery_attempt_count")?)?,
        recovery_retry_delay_ms: optional_unsigned(row, "recovery_retry_delay_ms")?,
        recovery_total_latency_ms: optional_unsigned(row, "recovery_total_latency_ms")?,
        occurred_at: get(row, "occurred_at")?,
        stable_sort_id: get(row, "stable_sort_id")?,
    })
}

pub(crate) fn cost_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<CurrencyCostTotal> {
    Ok(CurrencyCostTotal {
        currency: get(row, "cost_currency")?,
        amount: parse_decimal_amount(&get::<String>(row, "amount")?)?,
    })
}

pub(crate) fn calculated_usage_billing_fact_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<CalculatedUsageBillingFact> {
    Ok(CalculatedUsageBillingFact {
        billing_snapshot_json: get(row, "billing_snapshot_json")?,
        bucket_start: get(row, "bucket_start")?,
        provider_kind: get(row, "provider_kind")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        service_tier: get(row, "service_tier")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_input_tokens: optional_unsigned(row, "cached_input_tokens")?,
        cache_write_input_tokens: optional_unsigned(row, "cache_write_input_tokens")?,
        total: cost_from_row(row)?,
    })
}

pub(crate) fn optional_decimal(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> StoreResult<Option<DecimalAmount>> {
    get::<Option<String>>(row, column)?
        .map(|value| parse_decimal_amount(&value))
        .transpose()
}

pub(crate) fn opaque_response_id(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> StoreResult<Option<String>> {
    get::<Option<Vec<u8>>>(row, column)?
        .map(String::from_utf8)
        .transpose()
        .map_err(|source| invalid(column).with_source(source))
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
    row.try_get(column)
        .map_err(|source| invalid(column).with_source(source))
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
    u64::try_from(value)
        .map_err(|source| invalid("numeric observation is negative").with_source(source))
}

pub(crate) fn to_u32(value: i32) -> StoreResult<u32> {
    u32::try_from(value)
        .map_err(|source| invalid("integer observation is negative").with_source(source))
}

pub(crate) fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        source: None,
        entity: "observability",
        message: message.to_owned(),
    }
}

fn parse_decimal_amount(value: &str) -> StoreResult<DecimalAmount> {
    DecimalAmount::from_str(value).map_err(|source| StoreError::InvalidData {
        source: Some(source.into()),
        entity: "decimal amount",
        message: "expected a non-negative numeric(20,10) value".to_owned(),
    })
}

fn billing_from_row(
    row: &sqlx::postgres::PgRow,
    source: &str,
    amount: Option<&DecimalAmount>,
    currency: Option<&str>,
) -> StoreResult<Option<admin_observability::UsageBilling>> {
    let billing = match (amount, currency) {
        (Some(amount), Some(currency)) => Some(admin_observability::UsageBilling::Total {
            source: source.to_owned(),
            total: admin_observability::CurrencyCost {
                currency: currency.to_owned(),
                amount: amount.clone(),
            },
        }),
        (None, None) => None,
        _ => {
            return Err(StoreError::InvalidData {
                source: None,
                entity: "observability request billing",
                message: "cost amount and currency must be present together".to_owned(),
            });
        }
    };
    let snapshot: Option<serde_json::Value> = get(row, "billing_snapshot_json")?;
    Ok(restore_billing_snapshot(billing, snapshot.as_ref()))
}
