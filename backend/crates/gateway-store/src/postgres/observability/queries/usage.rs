//! Usage 明细、诊断与过滤查询族

use super::super::*;
use serde_json::Value;

const MODEL_DIAGNOSTIC_DIMENSION_SQL: &str =
    "coalesce(mr.upstream_model_id, mr.requested_model_id)";

pub(crate) fn push_usage_filter(
    query: &mut QueryBuilder<Postgres>,
    filter: &UsageRecordFilter,
    alias: &str,
) {
    if !filter.routing_group_refs.is_empty() {
        query
            .push(format!(" and {alias}.routing_group_refs && "))
            .push_bind(filter.routing_group_refs.clone())
            .push("::text[]");
    }
    if let Some(value) = &filter.client_api_key_ref {
        query.push(format!(" and {alias}.client_api_key_ref = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.request_id {
        query.push(format!(" and {alias}.id = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.provider_account_ref {
        query.push(format!(" and {alias}.provider_account_ref = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.operation {
        query.push(format!(" and {alias}.operation = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.provider_kind {
        query.push(format!(" and {alias}.provider_kind = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.model {
        query.push(format!(" and ({alias}.requested_model_id = "));
        query.push_bind(value.clone());
        query.push(format!(" or {alias}.upstream_model_id = "));
        query.push_bind(value.clone());
        query.push(")");
    }
    if let Some(value) = &filter.outcome {
        query.push(format!(" and {alias}.outcome = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = filter.status_code {
        query.push(format!(" and ({alias}.client_status_code = "));
        query.push_bind(i32::from(value));
        query.push(format!(" or {alias}.upstream_status_code = "));
        query.push_bind(i32::from(value));
        query.push(")");
    }
    if let Some(value) = &filter.transport {
        query.push(format!(" and ({alias}.client_transport = "));
        query.push_bind(value.clone());
        query.push(format!(" or {alias}.upstream_transport = "));
        query.push_bind(value.clone());
        query.push(")");
    }
    if let Some(value) = filter.attempt_index {
        query.push(format!(" and ({alias}.attempt_count = "));
        query.push_bind(i32::try_from(value).unwrap_or(i32::MAX));
        query.push(format!(
            " or exists (select 1 from ops_events attempt_event
                          where attempt_event.model_request_id = {alias}.id
                            and attempt_event.attempt_index = "
        ));
        query.push_bind(i32::try_from(value).unwrap_or(i32::MAX));
        query.push("))");
    }
    if let Some(value) = &filter.response_id {
        query.push(format!(" and ({alias}.client_response_id = "));
        query.push_bind(value.as_bytes().to_vec());
        query.push(format!(" or {alias}.upstream_response_id = "));
        query.push_bind(value.as_bytes().to_vec());
        query.push(")");
    }
    if let Some(value) = &filter.upstream_request_id {
        query.push(format!(" and {alias}.upstream_request_id = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.search {
        let pattern = literal_prefix_pattern(value);
        query.push(format!(" and ({alias}.id like "));
        query.push_bind(pattern.clone());
        for column in [
            "client_api_key_ref",
            "provider_account_ref",
            "provider_account_email_snapshot",
            "provider_account_name_snapshot",
            "requested_model_id",
            "upstream_model_id",
            "upstream_request_id",
        ] {
            query.push(format!(" escape '\\' or {alias}.{column} like "));
            query.push_bind(pattern.clone());
        }
        query.push(" escape '\\'");

        query
            .push(format!(" or {alias}.client_api_key_name_snapshot ilike "))
            .push_bind(pattern)
            .push(" escape '\\'");
        query.push(")");
    }
}

pub(crate) fn literal_prefix_pattern(value: &str) -> String {
    format!(
        "{}%",
        value
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

pub(crate) const USAGE_LIST_RECORD_SELECT: &str =
    "select mr.id, mr.client_api_key_name_snapshot as client_api_key_name, mr.endpoint, mr.client_transport, mr.requested_model_id,
            mr.provider_kind, mr.provider_account_ref,
            mr.provider_account_name_snapshot as provider_account_name,
            mr.provider_account_email_snapshot as provider_account_email,
            mr.provider_account_notes_snapshot as provider_account_notes,
            mr.provider_account_plan_type_snapshot as provider_account_plan_type,
            mr.provider_account_authentication_kind_snapshot
              as provider_account_authentication_kind,
            mr.upstream_model_id, mr.upstream_transport, mr.upstream_response_model, mr.service_tier,
            mr.input_tokens, mr.output_tokens, mr.cached_input_tokens, mr.cache_write_input_tokens,
            mr.reasoning_output_tokens, mr.image_input_tokens, mr.image_output_tokens,
            mr.total_tokens, mr.billing_snapshot_json, mr.cost_source, mr.cost_amount::text, mr.cost_currency,
            mr.transport_decision_wait_ms, mr.connect_ms, mr.headers_ms,
            mr.first_event_ms, mr.first_reasoning_ms, mr.first_text_ms, mr.first_token_ms,
            mr.provider_processing_ms, mr.upstream_response_ms,
            mr.upstream_api_overhead_ms, mr.upstream_engine_ms, mr.upstream_engine_iapi_ttft_ms, mr.upstream_engine_service_ttft_ms, mr.upstream_engine_iapi_tbt_ms, mr.upstream_engine_service_tbt_ms, mr.latency_ms, mr.admission_decision_ms,
            mr.account_selection_wait_ms, mr.capacity_used_slots, mr.capacity_total_slots,
            host(mr.client_ip) as client_ip, mr.user_agent,
            mr.reasoning_effort, mr.reasoning_preset, mr.subagent_kind, mr.compact,
            mr.started_at
     from model_request_observations mr";

pub(crate) const USAGE_RECORD_DETAIL_SELECT: &str =
    "select mr.id, mr.client_api_key_ref, mr.config_revision,
            mr.routing_scope, mr.routing_group_refs, mr.routing_group_names_snapshot,
            mr.protocol, mr.operation,
            mr.endpoint, mr.client_transport, mr.requested_model_id,
            mr.provider_kind, mr.provider_account_ref,
            mr.provider_account_name_snapshot as provider_account_name,
            mr.provider_account_email_snapshot as provider_account_email,
            mr.provider_account_authentication_kind_snapshot
              as provider_account_authentication_kind,
            mr.upstream_model_id, mr.upstream_transport, mr.http_version, mr.websocket_pool,
            mr.service_tier, mr.upstream_response_model, mr.provider_observation_json, mr.diagnostic_trace_json,
            mr.attempt_count, mr.upstream_send_state, mr.downstream_committed_at,
            mr.outcome, mr.client_status_code, mr.upstream_status_code,
            mr.client_response_id, mr.upstream_request_id, mr.upstream_response_id,
            mr.error_kind, mr.provider_error_code, mr.error_message, mr.retry_after_ms,
            mr.input_tokens, mr.output_tokens, mr.cached_input_tokens, mr.cache_write_input_tokens,
            mr.reasoning_output_tokens, mr.image_input_tokens, mr.image_output_tokens,
            mr.total_tokens, mr.billing_snapshot_json, mr.cost_source, mr.cost_amount::text,
            mr.cost_currency, mr.transport_decision_wait_ms, mr.connect_ms, mr.headers_ms,
            mr.first_event_ms, mr.first_reasoning_ms, mr.first_text_ms, mr.first_token_ms,
            mr.provider_processing_ms, mr.upstream_response_ms,
            mr.upstream_api_overhead_ms, mr.upstream_engine_ms, mr.upstream_engine_iapi_ttft_ms, mr.upstream_engine_service_ttft_ms, mr.upstream_engine_iapi_tbt_ms, mr.upstream_engine_service_tbt_ms, mr.latency_ms, mr.admission_decision_ms,
            mr.account_selection_wait_ms, mr.capacity_used_slots, mr.capacity_total_slots,
            host(mr.client_ip) as client_ip,
            mr.user_agent, mr.reasoning_effort, mr.reasoning_preset, mr.request_kind,
            mr.subagent_kind, mr.compact, mr.image_generation_requested,
            mr.image_generation_succeeded, mr.started_at, mr.effective_recovery_lease_expires_at as deadline_at, mr.completed_at
     from model_request_observations mr";

pub(crate) async fn list_usage_records(
    pool: &PgPool,
    query: UsageRecordQuery,
) -> StoreResult<UsageRecordPage> {
    query.filter.validate()?;
    let mut transaction = analytics::begin_snapshot(pool).await?;
    let total = count_usage_records(&mut *transaction, query.range, &query.filter).await?;
    let items = list_usage_record_items(&mut *transaction, &query).await?;
    transaction
        .commit()
        .await
        .map_err(|error| postgres_unavailable("complete usage page snapshot", error))?;
    Ok(UsageRecordPage {
        items,
        current_page: query.current_page,
        page_size: query.page_size.get(),
        total,
    })
}

pub(crate) async fn list_usage_record_items<'e, E: sqlx::Executor<'e, Database = Postgres>>(
    executor: E,
    query: &UsageRecordQuery,
) -> StoreResult<Vec<UsageListRecord>> {
    query.filter.validate()?;
    let offset = observability_page_offset(query.current_page, query.page_size)?;
    let mut statement = QueryBuilder::<Postgres>::new(
        "with candidate_page as materialized (select mr.id, mr.started_at from model_request_observations mr",
    );
    statement.push(" where mr.started_at >= ");
    statement.push_bind(query.range.start);
    statement.push(" and mr.started_at < ");
    statement.push_bind(query.range.end);
    push_completed_usage_fact_filter(&mut statement, "mr");
    push_usage_filter(&mut statement, &query.filter, "mr");
    statement.push(" order by mr.started_at desc, mr.id desc limit ");
    statement.push_bind(i64::from(query.page_size.get()));
    statement.push(" offset ");
    statement.push_bind(offset);
    statement.push(") ");
    statement.push(USAGE_LIST_RECORD_SELECT);
    statement.push(
        " join candidate_page page on page.id = mr.id order by page.started_at desc, page.id desc",
    );
    let rows = statement
        .build()
        .fetch_all(executor)
        .await
        .map_err(|source| postgres_unavailable("list usage records", source))?;
    let items = rows
        .iter()
        .map(usage_list_record_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(items)
}

pub(crate) async fn count_usage_records<'e, E: sqlx::Executor<'e, Database = Postgres>>(
    executor: E,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<u64> {
    let mut statement = QueryBuilder::<Postgres>::new(
        "select count(*)::bigint from model_request_observations mr where mr.started_at >= ",
    );
    statement.push_bind(range.start);
    statement.push(" and mr.started_at < ");
    statement.push_bind(range.end);
    push_completed_usage_fact_filter(&mut statement, "mr");
    push_usage_filter(&mut statement, filter, "mr");
    let total = statement
        .build_query_scalar::<i64>()
        .fetch_one(executor)
        .await
        .map_err(|source| postgres_unavailable("count usage records", source))?;
    to_u64(total)
}

pub(crate) async fn usage_record_detail(
    pool: &PgPool,
    request_id: &str,
) -> StoreResult<UsageRecordDetail> {
    require_nonempty("model request", "id", request_id)?;
    validate_text(request_id, MAX_FILTER_BYTES, "request ID")?;
    let mut statement = QueryBuilder::<Postgres>::new(USAGE_RECORD_DETAIL_SELECT);
    statement.push(" where mr.id = ");
    statement.push_bind(request_id.to_owned());
    let row = statement
        .build()
        .fetch_optional(pool)
        .await
        .map_err(|source| postgres_unavailable("load usage record detail", source))?
        .ok_or_else(|| StoreError::NotFound {
            source: None,
            entity: "model request",
            id: request_id.to_owned(),
        })?;
    let request = usage_record_from_row(&row)?;
    let rows = sqlx::query(
        "select id, attempt_index, component, operation,
                provider_kind, provider_account_ref,
                provider_account_name_snapshot as provider_account_name,
                provider_account_email_snapshot as provider_account_email,
                provider_account_authentication_kind_snapshot
                  as provider_account_authentication_kind,
                upstream_model_id,
                failure_kind, status_code, provider_error_code, retry_after_ms,
                upstream_request_id, latency_ms, message, created_at
         from ops_events where model_request_id = $1
         order by attempt_index, created_at, id",
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    .map_err(|source| postgres_unavailable("load usage attempt observations", source))?;
    let mut attempts = rows
        .iter()
        .map(intermediate_attempt_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    if request.attempt_count > 0 {
        attempts.push(final_attempt_from_request(&request));
    }
    let trace: Option<Value> = row
        .try_get("diagnostic_trace_json")
        .map_err(|source| postgres_unavailable("decode request trace", source))?;
    let related_requests = sqlx::query_scalar::<_, Value>(
        "select jsonb_build_object('requestId', related.id, 'outcome', related.outcome,
             'relation', case when related.id = current.recovery_request_id then 'recovered_by' else 'recovers' end,
             'completedAt', related.completed_at)
         from model_request_observations current join model_request_observations related
           on related.id = current.recovery_request_id or related.recovery_request_id = current.id
         where current.id = $1 order by related.started_at limit 20"
    ).bind(request_id).fetch_all(pool).await
        .map_err(|source| postgres_unavailable("load related recovery requests", source))?;
    Ok(UsageRecordDetail {
        request,
        attempts,
        trace,
        related_requests,
    })
}

pub(crate) fn intermediate_attempt_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<UsageAttemptObservation> {
    Ok(UsageAttemptObservation {
        source: "ops_event".to_owned(),
        id: get(row, "id")?,
        attempt_index: to_u32(get(row, "attempt_index")?)?,
        component: get(row, "component")?,
        operation: get(row, "operation")?,
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        provider_account_name: get(row, "provider_account_name")?,
        provider_account_email: get(row, "provider_account_email")?,
        provider_account_authentication_kind: get(row, "provider_account_authentication_kind")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: None,
        upstream_send_state: None,
        outcome: admin_observability::RequestOutcome::Failed,
        downstream_committed: false,
        status_code: optional_status(row, "status_code")?,
        provider_error_code: get(row, "provider_error_code")?,
        failure_kind: get(row, "failure_kind")?,
        retry_after_ms: optional_unsigned(row, "retry_after_ms")?,
        upstream_request_id: get(row, "upstream_request_id")?,
        latency_ms: optional_unsigned(row, "latency_ms")?,
        message: get(row, "message")?,
        input_tokens: None,
        output_tokens: None,
        cached_input_tokens: None,
        cache_write_input_tokens: None,
        reasoning_output_tokens: None,
        total_tokens: None,
        cost_source: None,
        cost_amount: None,
        cost_currency: None,
        occurred_at: get(row, "created_at")?,
    })
}

pub(crate) fn final_attempt_from_request(request: &UsageRecord) -> UsageAttemptObservation {
    UsageAttemptObservation {
        source: "model_request".to_owned(),
        id: format!("{}:final", request.id),
        attempt_index: request.attempt_count,
        component: "model_request".to_owned(),
        operation: request.operation.clone(),
        provider_kind: request.provider_kind.clone(),
        provider_account_ref: request.provider_account_ref.clone(),
        provider_account_name: request.provider_account_name.clone(),
        provider_account_email: request.provider_account_email.clone(),
        provider_account_authentication_kind: request.provider_account_authentication_kind.clone(),
        upstream_model_id: request.upstream_model_id.clone(),
        upstream_transport: request.upstream_transport.clone(),
        upstream_send_state: Some(request.upstream_send_state.clone()),
        outcome: request.outcome.clone(),
        downstream_committed: request.downstream_committed_at.is_some(),
        status_code: request.upstream_status_code.or(request.client_status_code),
        provider_error_code: request.provider_error_code.clone(),
        failure_kind: request.error_kind.clone(),
        retry_after_ms: request.retry_after_ms,
        upstream_request_id: request.upstream_request_id.clone(),
        latency_ms: request.latency_ms,
        message: request.error_message.clone(),
        input_tokens: request.input_tokens,
        output_tokens: request.output_tokens,
        cached_input_tokens: request.cached_input_tokens,
        cache_write_input_tokens: request.cache_write_input_tokens,
        reasoning_output_tokens: request.reasoning_output_tokens,
        total_tokens: request.total_tokens,
        cost_source: Some(request.cost_source.clone()),
        cost_amount: request.cost_amount.clone(),
        cost_currency: request.cost_currency.clone(),
        occurred_at: request.completed_at.unwrap_or(request.started_at),
    }
}

pub(crate) fn push_diagnostic_dimension_filter(
    statement: &mut QueryBuilder<Postgres>,
    dimension: DiagnosticDimension,
) {
    match dimension {
        DiagnosticDimension::Failure => {
            statement.push(" and mr.error_kind is not null");
        }
        DiagnosticDimension::Model => {
            statement.push(" and ");
            statement.push(MODEL_DIAGNOSTIC_DIMENSION_SQL);
            statement.push(" is not null");
        }
        DiagnosticDimension::Status => {
            statement
                .push(" and coalesce(mr.upstream_status_code, mr.client_status_code) is not null");
        }
        DiagnosticDimension::Provider
        | DiagnosticDimension::Account
        | DiagnosticDimension::ApiKey
        | DiagnosticDimension::Transport => {}
    }
}
