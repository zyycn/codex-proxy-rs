//! Ops 错误查询族。

use super::super::*;

const REQUEST_ERROR_SELECT: &str = "select 'model_request'::text as source,
       mr.id as event_id, mr.id as request_id,
       nullif(mr.attempt_count, 0) as attempt_index,
       mr.client_api_key_ref, 'model_request'::text as component, mr.operation,
       mr.protocol, mr.client_transport, mr.requested_model_id, mr.service_tier,
       mr.endpoint, mr.provider_kind, mr.provider_account_ref,
       mr.provider_account_name_snapshot as provider_account_name,
       mr.provider_account_email_snapshot as provider_account_email,
       mr.provider_account_authentication_kind_snapshot
         as provider_account_authentication_kind,
       mr.upstream_model_id, mr.upstream_transport,
       coalesce(mr.error_kind, 'failed') as failure_kind,
       mr.upstream_send_state,
       mr.client_status_code, mr.upstream_status_code,
       mr.provider_error_code, mr.client_response_id, mr.upstream_request_id,
       mr.latency_ms,
       coalesce(mr.error_message, mr.error_kind, 'request failed') as message,
       mr.raw_upstream_error,
       1::integer as occurrence_count,
       host(mr.client_ip) as client_ip, mr.user_agent,
       mr.reasoning_effort, mr.reasoning_preset, mr.request_kind,
       mr.subagent_kind, mr.compact,
       mr.continuation_affinity_hash, mr.continuation_previous_response_id_hash,
       mr.continuation_unavailable_reason,
       mr.upstream_connection_id, mr.upstream_connection_exit_reason,
       mr.upstream_connection_age_ms, mr.upstream_connection_idle_ms,
       mr.recovery_request_id, mr.recovered_at, mr.recovery_attempt_count,
       mr.recovery_retry_delay_ms, mr.recovery_total_latency_ms,
       mr.completed_at as occurred_at,
       'model_request:' || mr.id as stable_sort_id
from model_requests mr
where mr.outcome = 'failed'";

const OPS_EVENT_SELECT: &str = "select 'ops_event'::text as source,
       oe.id as event_id, oe.model_request_id as request_id, oe.attempt_index,
       mr.client_api_key_ref, oe.component, oe.operation,
       mr.protocol, mr.client_transport, mr.requested_model_id, mr.service_tier,
       mr.endpoint, oe.provider_kind, oe.provider_account_ref,
       oe.provider_account_name_snapshot as provider_account_name,
       oe.provider_account_email_snapshot as provider_account_email,
       oe.provider_account_authentication_kind_snapshot
         as provider_account_authentication_kind,
       oe.upstream_model_id, null::text as upstream_transport, oe.failure_kind,
       oe.upstream_send_state,
       null::integer as client_status_code, oe.status_code as upstream_status_code,
       oe.provider_error_code, mr.client_response_id, oe.upstream_request_id,
       oe.latency_ms, oe.message, oe.raw_upstream_error, oe.occurrence_count,
       host(mr.client_ip) as client_ip, mr.user_agent,
       mr.reasoning_effort, mr.reasoning_preset, mr.request_kind,
       mr.subagent_kind, mr.compact,
       mr.continuation_affinity_hash, mr.continuation_previous_response_id_hash,
       null::text as continuation_unavailable_reason,
       null::text as upstream_connection_id,
       null::text as upstream_connection_exit_reason,
       null::bigint as upstream_connection_age_ms,
       null::bigint as upstream_connection_idle_ms,
       mr.recovery_request_id, mr.recovered_at,
       coalesce(mr.recovery_attempt_count, 0) as recovery_attempt_count,
       mr.recovery_retry_delay_ms, mr.recovery_total_latency_ms,
       oe.created_at as occurred_at,
       'ops_event:' || oe.id as stable_sort_id
from ops_events oe
left join model_requests mr on mr.id = oe.model_request_id
where true";

pub(crate) async fn list_ops_errors(
    pool: &PgPool,
    query: OpsErrorQuery,
) -> StoreResult<OpsErrorPage> {
    query.filter.validate()?;
    let total = count_ops_errors(pool, query.range, &query.filter).await?;
    let offset = observability_page_offset(query.current_page, query.page_size)?;
    let mut statement = QueryBuilder::<Postgres>::new("select * from (");
    statement.push(REQUEST_ERROR_SELECT);
    push_request_error_predicates(&mut statement, query.range, &query.filter);
    statement.push(" union all ");
    statement.push(OPS_EVENT_SELECT);
    push_ops_event_predicates(&mut statement, query.range, &query.filter);
    statement.push(") e order by occurred_at desc, stable_sort_id desc limit ");
    statement.push_bind(i64::from(query.page_size.get()));
    statement.push(" offset ");
    statement.push_bind(offset);
    let rows = statement
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("list ops errors"))?;
    let items = rows
        .iter()
        .map(ops_error_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(OpsErrorPage {
        items,
        current_page: query.current_page,
        page_size: query.page_size.get(),
        total,
    })
}

pub(crate) async fn count_ops_errors(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &OpsErrorFilter,
) -> StoreResult<u64> {
    let mut statement = QueryBuilder::<Postgres>::new(
        "select coalesce(sum(source_count), 0)::bigint from (select count(*)::bigint as source_count from model_requests mr where mr.outcome = 'failed'",
    );
    push_request_error_predicates(&mut statement, range, filter);
    statement.push(
        " union all select count(*)::bigint as source_count from ops_events oe left join model_requests mr on mr.id = oe.model_request_id where true",
    );
    push_ops_event_predicates(&mut statement, range, filter);
    statement.push(") counts");
    let total = statement
        .build_query_scalar::<i64>()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("count ops errors"))?;
    to_u64(total)
}

fn push_request_error_predicates(
    statement: &mut QueryBuilder<Postgres>,
    range: ObservabilityRange,
    filter: &OpsErrorFilter,
) {
    push_range(statement, "mr.completed_at", range);
    for (column, value) in [
        ("mr.client_api_key_ref", &filter.client_api_key_ref),
        ("mr.id", &filter.request_id),
        ("mr.provider_account_ref", &filter.provider_account_ref),
        ("mr.provider_kind", &filter.provider_kind),
        ("mr.operation", &filter.operation),
        ("mr.upstream_transport", &filter.transport),
        ("mr.upstream_request_id", &filter.upstream_request_id),
    ] {
        push_text_equality(statement, column, value);
    }
    push_response_id_filter(statement, "mr.client_response_id", filter);
    push_text_equality(statement, "mr.upstream_model_id", &filter.model);
    if let Some(index) = filter.attempt_index {
        statement.push(" and nullif(mr.attempt_count, 0) = ");
        statement.push_bind(i32::try_from(index).unwrap_or(i32::MAX));
    }
    if let Some(status) = filter.status_code {
        statement.push(" and mr.upstream_status_code = ");
        statement.push_bind(i32::from(status));
    }
    if let Some(search) = &filter.search {
        push_prefix_search(
            statement,
            &[
                "mr.id",
                "mr.client_api_key_ref",
                "mr.provider_account_ref",
                "mr.upstream_request_id",
                "mr.provider_error_code",
            ],
            search,
        );
    }
}

fn push_ops_event_predicates(
    statement: &mut QueryBuilder<Postgres>,
    range: ObservabilityRange,
    filter: &OpsErrorFilter,
) {
    push_range(statement, "oe.created_at", range);
    for (column, value) in [
        ("mr.client_api_key_ref", &filter.client_api_key_ref),
        ("oe.model_request_id", &filter.request_id),
        ("oe.provider_account_ref", &filter.provider_account_ref),
        ("oe.provider_kind", &filter.provider_kind),
        ("oe.operation", &filter.operation),
        ("oe.upstream_request_id", &filter.upstream_request_id),
    ] {
        push_text_equality(statement, column, value);
    }
    // Ops events do not persist upstream transport. A transport filter therefore
    // intentionally excludes this source instead of matching an unrelated request fact.
    if filter.transport.is_some() {
        statement.push(" and false");
    }
    push_response_id_filter(statement, "mr.client_response_id", filter);
    push_text_equality(statement, "oe.upstream_model_id", &filter.model);
    if let Some(index) = filter.attempt_index {
        statement.push(" and oe.attempt_index = ");
        statement.push_bind(i32::try_from(index).unwrap_or(i32::MAX));
    }
    if let Some(status) = filter.status_code {
        statement.push(" and oe.status_code = ");
        statement.push_bind(i32::from(status));
    }
    if let Some(search) = &filter.search {
        push_prefix_search(
            statement,
            &[
                "oe.id",
                "oe.model_request_id",
                "mr.client_api_key_ref",
                "oe.provider_account_ref",
                "oe.upstream_request_id",
                "oe.provider_error_code",
            ],
            search,
        );
    }
}

fn push_range(statement: &mut QueryBuilder<Postgres>, column: &str, range: ObservabilityRange) {
    statement.push(format!(" and {column} >= "));
    statement.push_bind(range.start);
    statement.push(format!(" and {column} < "));
    statement.push_bind(range.end);
}

fn push_text_equality(
    statement: &mut QueryBuilder<Postgres>,
    column: &str,
    value: &Option<String>,
) {
    if let Some(value) = value {
        statement.push(format!(" and {column} = "));
        statement.push_bind(value.clone());
    }
}

fn push_response_id_filter(
    statement: &mut QueryBuilder<Postgres>,
    column: &str,
    filter: &OpsErrorFilter,
) {
    if let Some(value) = &filter.response_id {
        statement.push(format!(" and {column} = "));
        statement.push_bind(value.as_bytes().to_vec());
    }
}

fn push_prefix_search(statement: &mut QueryBuilder<Postgres>, columns: &[&str], value: &str) {
    let pattern = literal_prefix_pattern(value);
    statement.push(" and (");
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            statement.push(" or ");
        }
        statement.push(format!("{column} like "));
        statement.push_bind(pattern.clone());
        statement.push(" escape '\\'");
    }
    statement.push(")");
}
