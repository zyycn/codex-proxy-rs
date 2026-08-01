//! Ops 错误查询族。

use super::super::*;

pub(crate) const OPS_ERRORS_CTE: &str = "with errors as (
       select 'model_request'::text as source,
              mr.id as event_id, mr.id as request_id,
              nullif(mr.attempt_count, 0) as attempt_index,
              mr.client_api_key_ref, 'model_request'::text as component, mr.operation,
              mr.endpoint,
              mr.provider_kind,
              mr.provider_account_ref,
              mr.provider_account_name_snapshot as provider_account_name,
              mr.provider_account_email_snapshot as provider_account_email,
              mr.provider_account_authentication_kind_snapshot
                as provider_account_authentication_kind,
              mr.upstream_model_id, mr.upstream_transport,
              coalesce(mr.error_kind, 'failed') as failure_kind,
              mr.client_status_code, mr.upstream_status_code,
              mr.provider_error_code, mr.client_response_id, mr.upstream_request_id,
              mr.latency_ms, coalesce(mr.error_message, mr.error_kind, 'request failed') as message,
              1::integer as occurrence_count,
              coalesce(mr.completed_at, mr.started_at) as occurred_at,
              'model_request:' || mr.id as stable_sort_id
       from model_requests mr where mr.outcome = 'failed'
       union all
       select 'ops_event'::text, oe.id, oe.model_request_id, oe.attempt_index,
              mr.client_api_key_ref, oe.component, oe.operation,
              null::text as endpoint,
              oe.provider_kind, oe.provider_account_ref,
              oe.provider_account_name_snapshot as provider_account_name,
              oe.provider_account_email_snapshot as provider_account_email,
              oe.provider_account_authentication_kind_snapshot
                as provider_account_authentication_kind,
              oe.upstream_model_id, null::text, oe.failure_kind,
              null::integer as client_status_code, oe.status_code as upstream_status_code,
              oe.provider_error_code, mr.client_response_id, oe.upstream_request_id,
              oe.latency_ms, oe.message, oe.occurrence_count, oe.created_at,
              'ops_event:' || oe.id
       from ops_events oe left join model_requests mr on mr.id = oe.model_request_id
     )";

pub(crate) async fn list_ops_errors(
    pool: &PgPool,
    query: OpsErrorQuery,
) -> StoreResult<OpsErrorPage> {
    query.filter.validate()?;
    let total = count_ops_errors(pool, query.range, &query.filter).await?;
    let mut statement = QueryBuilder::<Postgres>::new(OPS_ERRORS_CTE);
    statement.push(
        " select source, event_id, request_id, attempt_index, client_api_key_ref,
                 component, operation, endpoint, provider_kind, provider_account_ref,
                 provider_account_name, provider_account_email,
                 provider_account_authentication_kind,
                 upstream_model_id, upstream_transport,
                 failure_kind, client_status_code, upstream_status_code,
                 provider_error_code, client_response_id,
                 upstream_request_id, latency_ms, message, occurrence_count, occurred_at,
                 stable_sort_id
          from errors e where e.occurred_at >= ",
    );
    statement.push_bind(query.range.start);
    statement.push(" and e.occurred_at < ");
    statement.push_bind(query.range.end);
    push_ops_filter(&mut statement, &query.filter);
    if let Some(cursor) = &query.cursor {
        statement.push(" and (e.occurred_at, e.stable_sort_id) < (");
        statement.push_bind(cursor.observed_at);
        statement.push(", ");
        statement.push_bind(cursor.stable_id.clone());
        statement.push(")");
    }
    statement.push(" order by e.occurred_at desc, e.stable_sort_id desc limit ");
    statement.push_bind(i64::from(query.page_size.get()) + 1);
    if query.cursor.is_none() {
        let offset = u64::from(query.page.get() - 1)
            .checked_mul(u64::from(query.page_size.get()))
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| invalid("ops error page offset is too large"))?;
        statement.push(" offset ");
        statement.push_bind(offset);
    }
    let rows = statement
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("list ops errors"))?;
    let mut items = rows
        .iter()
        .map(ops_error_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    let has_more = items.len() > usize::from(query.page_size.get());
    if has_more {
        items.pop();
    }
    let next_cursor = if has_more {
        items
            .last()
            .map(|item| ObservabilityCursor::new(item.occurred_at, item.stable_sort_id.clone()))
            .transpose()?
    } else {
        None
    };
    Ok(OpsErrorPage {
        items,
        total,
        next_cursor,
    })
}

pub(crate) async fn count_ops_errors(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &OpsErrorFilter,
) -> StoreResult<u64> {
    let mut statement = QueryBuilder::<Postgres>::new(OPS_ERRORS_CTE);
    statement.push(" select count(*)::bigint from errors e where e.occurred_at >= ");
    statement.push_bind(range.start);
    statement.push(" and e.occurred_at < ");
    statement.push_bind(range.end);
    push_ops_filter(&mut statement, filter);
    let total = statement
        .build_query_scalar::<i64>()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("count ops errors"))?;
    to_u64(total)
}

pub(crate) fn push_ops_filter(statement: &mut QueryBuilder<Postgres>, filter: &OpsErrorFilter) {
    for (column, value) in [
        ("client_api_key_ref", &filter.client_api_key_ref),
        ("request_id", &filter.request_id),
        ("provider_account_ref", &filter.provider_account_ref),
        ("provider_kind", &filter.provider_kind),
        ("operation", &filter.operation),
        ("upstream_transport", &filter.transport),
        ("upstream_request_id", &filter.upstream_request_id),
        ("failure_kind", &filter.failure_kind),
    ] {
        if let Some(value) = value {
            statement.push(format!(" and e.{column} = "));
            statement.push_bind(value.clone());
        }
    }
    if let Some(value) = &filter.response_id {
        statement.push(" and e.client_response_id = ");
        statement.push_bind(value.as_bytes().to_vec());
    }
    if let Some(model) = &filter.model {
        statement.push(" and e.upstream_model_id = ");
        statement.push_bind(model.clone());
    }
    if let Some(index) = filter.attempt_index {
        statement.push(" and e.attempt_index = ");
        statement.push_bind(i32::try_from(index).unwrap_or(i32::MAX));
    }
    if let Some(status) = filter.status_code {
        statement.push(" and e.upstream_status_code = ");
        statement.push_bind(i32::from(status));
    }
    if let Some(search) = &filter.search {
        statement.push(
            " and lower(concat_ws(' ', e.event_id, e.request_id, e.client_api_key_ref,
                    e.component, e.operation, e.provider_kind, e.provider_account_ref,
                    e.upstream_model_id, e.failure_kind, e.provider_error_code,
                    e.upstream_request_id, e.message)) like ",
        );
        statement.push_bind(format!("%{}%", search.to_lowercase()));
    }
}
