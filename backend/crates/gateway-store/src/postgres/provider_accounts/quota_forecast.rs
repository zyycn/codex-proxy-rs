//! 预测只读采样：先聚合数值、再读取有界的 Provider 文档，不扫描模型排行维度。

use gateway_admin::model::quota_forecast_sampling::{
    MAX_FORECAST_HISTORY_POINTS, QuotaForecastHistory, QuotaForecastHistoryPoint,
    QuotaForecastUsage,
};

use super::*;

pub(super) async fn load_history(
    pool: &PgPool,
    budget: &super::super::ObservabilityQueryBudget,
    window: &AccountUsageWindowQuery,
) -> AdminStoreResult<QuotaForecastHistory> {
    validate_admin_account_ids(std::slice::from_ref(&window.account_id))
        .map_err(|error| admin_store_error(ENTITY, error))?;
    let range = ObservabilityRange::new(window.range.start, window.range.end)
        .map_err(|error| admin_store_error(ENTITY, error))?;
    if range.end - range.start > TimeDelta::days(32) {
        return Err(AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "forecast range exceeds supported quota windows",
        ));
    }
    let rows = budget
        .run("load quota forecast history", async {
            sqlx::query(sqlx::AssertSqlSafe(history_sql()))
                .bind(&window.account_id)
                .bind(range.start)
                .bind(range.end)
                .bind(MAX_FORECAST_HISTORY_POINTS as i32)
                .fetch_all(pool)
                .await
                .map_err(|_| postgres_unavailable("load quota forecast history"))
        })
        .await
        .map_err(|error| admin_store_error(ENTITY, error))?;
    let mut history = QuotaForecastHistory::default();
    for row in &rows {
        let usage = QuotaForecastUsage {
            request_count: window_usage_count(row, "request_count")?,
            tokens: window_usage_count(row, "tokens")?,
            input_tokens: window_usage_count(row, "input_tokens")?,
            output_tokens: window_usage_count(row, "output_tokens")?,
            cached_tokens: window_usage_count(row, "cached_tokens")?,
            missing_token_count: window_usage_count(row, "missing_token_count")?,
            known_cost_count: window_usage_count(row, "known_cost_count")?,
            unavailable_cost_count: window_usage_count(row, "unavailable_cost_count")?,
            usd: window_usage_value(row, "usd")?,
            excluded_request_count: window_usage_count(row, "excluded_request_count")?,
        };
        if window_usage_value::<bool>(row, "is_total")? {
            history.usage = usage;
            history.pending_request_count = window_usage_count(row, "pending_count")?;
        } else if let Some(document) = window_usage_value::<
            Option<sqlx::types::Json<serde_json::Map<String, serde_json::Value>>>,
        >(row, "document")?
        {
            history.points.push(QuotaForecastHistoryPoint {
                started_at: window_usage_value(row, "started_at")?,
                completed_at: window_usage_value(row, "completed_at")?,
                usage,
                provider_observation: ProviderDocument::new(OpaqueProviderData::new(document.0)),
            });
        }
    }
    Ok(history)
}

fn history_sql() -> String {
    let completed_usage = completed_usage_fact_predicate("mr");
    // 一个语句共享 MVCC 快照。用 RANGE 帧让相同完成时间的点拥有相同累计值，
    // 避免并发请求的任意行顺序制造不同分子；未完成请求只进入待决计数。
    format!(
        "with scoped as materialized (
            select mr.id, mr.started_at, mr.completed_at,
                   mr.completed_at <= $3 and mr.outcome <> 'running' as settled,
                   coalesce(({completed_usage}), false) as included,
                   mr.provider_observation_json is not null as has_document,
                   mr.input_tokens, mr.output_tokens, mr.cached_tokens, mr.total_tokens,
                   mr.cost_amount, mr.cost_currency,
                   coalesce(mr.cost_source in ('calculated', 'provider_reported')
                     and mr.cost_amount is not null and mr.cost_currency = 'USD', false) as known_cost
              from model_requests mr
             where mr.provider_account_ref = $1
               and mr.started_at >= $2 and mr.started_at < $3
        ), facts as materialized (
            select *,
                (included and settled)::integer as request_count,
                case when included and settled then coalesce(total_tokens,
                  coalesce(input_tokens, 0) + coalesce(output_tokens, 0)) else 0 end as tokens,
                case when included and settled then coalesce(input_tokens, 0) else 0 end as inputs,
                case when included and settled then coalesce(output_tokens, 0) else 0 end as outputs,
                case when included and settled then coalesce(cached_tokens, 0) else 0 end as cached,
                (included and settled and total_tokens is null
                  and (input_tokens is null or output_tokens is null))::integer as missing_tokens,
                (included and settled and known_cost)::integer as known_costs,
                (included and settled and not known_cost)::integer as missing_costs,
                case when included and settled and known_cost then cost_amount else 0 end as usd,
                (not included and settled)::integer as excluded
              from scoped
        ), cumulative as (
            select id, started_at, completed_at, has_document,
                least($4 - 1, floor(extract(epoch from (completed_at - $2))
                  / greatest(1, extract(epoch from ($3::timestamptz - $2::timestamptz)) / $4))) as bucket,
                sum(request_count) over w as request_count,
                sum(tokens) over w as tokens,
                sum(inputs) over w as input_tokens,
                sum(outputs) over w as output_tokens,
                sum(cached) over w as cached_tokens,
                sum(missing_tokens) over w as missing_token_count,
                sum(known_costs) over w as known_cost_count,
                sum(missing_costs) over w as unavailable_cost_count,
                sum(usd) over w as usd,
                sum(excluded) over w as excluded_request_count
              from facts where settled
              window w as (order by completed_at range between unbounded preceding and current row)
        ), selected as (
            select distinct on (bucket) * from cumulative
             where has_document order by bucket, completed_at desc, id desc
        )
        select false as is_total, s.started_at, s.completed_at,
            s.request_count::bigint, s.tokens::bigint, s.input_tokens::bigint,
            s.output_tokens::bigint, s.cached_tokens::bigint, s.missing_token_count::bigint,
            s.known_cost_count::bigint, s.unavailable_cost_count::bigint,
            s.usd::double precision, s.excluded_request_count::bigint,
            0::bigint as pending_count, mr.provider_observation_json as document
          from selected s join model_requests mr on mr.id = s.id
        union all
        select true, $3, $3,
            coalesce(sum(request_count), 0)::bigint, coalesce(sum(tokens), 0)::bigint,
            coalesce(sum(inputs), 0)::bigint, coalesce(sum(outputs), 0)::bigint,
            coalesce(sum(cached), 0)::bigint, coalesce(sum(missing_tokens), 0)::bigint,
            coalesce(sum(known_costs), 0)::bigint, coalesce(sum(missing_costs), 0)::bigint,
            coalesce(sum(usd), 0)::double precision, coalesce(sum(excluded), 0)::bigint,
            count(*) filter (where settled is not true), null::jsonb
          from facts
        order by completed_at, is_total"
    )
}
