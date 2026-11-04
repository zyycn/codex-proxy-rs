//! 统计扫描的原生请求窄投影，沿用宿主用量资格与稳定身份

use super::*;
use sqlx::{QueryBuilder, postgres::PgRow};

pub(super) async fn declare(
    tx: &mut Transaction<'_, Postgres>,
    request: &ScanUsageRecords,
) -> StoreResult<()> {
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "declare usage_read no scroll cursor for
        select mr.id, mr.client_api_key_ref, mr.provider_kind, mr.provider_account_ref,
            mr.requested_model_id, mr.upstream_model_id, mr.protocol, mr.endpoint,
            mr.operation, mr.client_transport, mr.service_tier, mr.started_at, mr.completed_at,
            mr.outcome, mr.input_tokens, mr.output_tokens, mr.cached_input_tokens,
            mr.cache_write_input_tokens, mr.reasoning_output_tokens, mr.image_input_tokens,
            mr.image_output_tokens, mr.total_tokens, mr.cost_source, mr.cost_currency,
            mr.cost_amount::text as cost_amount, mr.user_agent, mr.reasoning_effort,
            mr.subagent_kind, mr.request_kind, mr.routing_scope, mr.routing_group_refs,
            mr.recovered_at is not null as recovered,
            coalesce(({}),false) and mr.recovered_at is null as usage_counted
        from model_request_observations mr where mr.started_at >= ",
        super::super::completed_usage_fact_predicate("mr"),
    ));
    query
        .push_bind(at(request.filter.records.start_ms)?)
        .push(" and mr.started_at < ")
        .push_bind(at(request.filter.records.end_ms)?);
    super::super::usage_facts::push_usage_read_filter(&mut query, &request.filter);
    query.push(" order by mr.started_at,mr.id");
    query.build().execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}

pub(super) fn decode(row: PgRow) -> StoreResult<UsageRecordFact> {
    let user_agent = row.try_get("user_agent").map_err(db_error)?;
    let reasoning_effort = row.try_get("reasoning_effort").map_err(db_error)?;
    let subagent_kind = row.try_get("subagent_kind").map_err(db_error)?;
    let request_kind = row.try_get("request_kind").map_err(db_error)?;
    let routing_scope = row.try_get("routing_scope").map_err(db_error)?;
    let routing_group_refs = row.try_get("routing_group_refs").map_err(db_error)?;
    let recovered = row.try_get("recovered").map_err(db_error)?;
    let usage_counted = row.try_get("usage_counted").map_err(db_error)?;
    Ok(UsageRecordFact {
        id: row.try_get("id").map_err(db_error)?,
        data: records::decode_data(&row)?,
        user_agent,
        reasoning_effort,
        subagent_kind,
        request_kind,
        routing_scope,
        routing_group_refs,
        recovered,
        request_counted: !recovered,
        usage_counted,
    })
}
