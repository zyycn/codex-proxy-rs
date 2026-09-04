//! Dashboard 与趋势查询族。

use super::super::*;

pub(crate) async fn request_metrics(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<RequestMetrics> {
    filter.validate()?;
    // 结果计数覆盖范围内全部请求（成功率/失败率的分母分子）；用量、缓存、
    // 延迟与成本聚合仅统计用量事实（已完整交付客户端的成功响应）。
    let fact = completed_usage_fact_predicate("mr");
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "select count(*)::bigint as request_count,
                count(*) filter (where outcome = 'succeeded')::bigint as success_count,
                count(*) filter (where outcome = 'failed')::bigint as failure_count,
                count(*) filter (where outcome = 'cancelled')::bigint as cancelled_count,
                count(*) filter (where outcome = 'incomplete')::bigint as incomplete_count,
                count(*) filter (where client_status_code between 400 and 499)::bigint
                  as caller_error_count,
                coalesce(sum(input_tokens) filter (where {fact}), 0)::bigint as input_tokens,
                coalesce(sum(output_tokens) filter (where {fact}), 0)::bigint as output_tokens,
                coalesce(sum(cached_tokens) filter (where {fact}), 0)::bigint as cached_tokens,
                coalesce(sum(cache_write_tokens) filter (where {fact}), 0)::bigint
                  as cache_write_tokens,
                coalesce(sum(reasoning_tokens) filter (where {fact}), 0)::bigint
                  as reasoning_tokens,
                coalesce(sum(total_tokens) filter (where {fact}), 0)::bigint as total_tokens,
                coalesce(sum(first_token_ms) filter (where {fact}), 0)::bigint
                  as first_token_latency_sum,
                count(first_token_ms) filter (where {fact})::bigint as first_token_latency_count,
                coalesce(sum(latency_ms) filter (where {fact}), 0)::bigint as latency_sum,
                count(latency_ms) filter (where {fact})::bigint as latency_count,
                max(latency_ms) filter (where {fact})::bigint as max_latency_ms,
                min(latency_ms) filter (where {fact})::bigint as min_latency_ms,
                count(*) filter (where {fact} and input_tokens is not null)::bigint
                  as cache_eligible_request_count,
                count(*) filter (where {fact} and input_tokens is not null
                                   and cached_tokens > 0)::bigint
                  as cache_hit_request_count,
                percentile_cont(0.50) within group (order by latency_ms)
                  filter (where {fact}) as latency_p50_ms,
                percentile_cont(0.95) within group (order by latency_ms)
                  filter (where {fact}) as latency_p95_ms,
                percentile_cont(0.99) within group (order by latency_ms)
                  filter (where {fact}) as latency_p99_ms,
                percentile_cont(0.50) within group (order by first_token_ms)
                  filter (where {fact}) as first_token_p50_ms,
                percentile_cont(0.95) within group (order by first_token_ms)
                  filter (where {fact}) as first_token_p95_ms,
                percentile_cont(0.99) within group (order by first_token_ms)
                  filter (where {fact}) as first_token_p99_ms,
                count(admission_decision_ms)::bigint as admission_decision_count,
                percentile_cont(0.50) within group (order by admission_decision_ms)
                  as admission_decision_p50_ms,
                percentile_cont(0.95) within group (order by admission_decision_ms)
                  as admission_decision_p95_ms,
                percentile_cont(0.99) within group (order by admission_decision_ms)
                  as admission_decision_p99_ms,
                count(account_selection_wait_ms)::bigint as account_selection_wait_count,
                percentile_cont(0.50) within group (order by account_selection_wait_ms)
                  as account_selection_wait_p50_ms,
                percentile_cont(0.95) within group (order by account_selection_wait_ms)
                  as account_selection_wait_p95_ms,
                percentile_cont(0.99) within group (order by account_selection_wait_ms)
                  as account_selection_wait_p99_ms,
                round((percentile_cont(0.10) within group (
                  order by output_tokens::double precision * 1000.0
                    / greatest(latency_ms - first_token_ms, 1)
                ) filter (where {fact} and output_tokens > 0
                          and latency_ms > first_token_ms)))::bigint as output_throughput_p10,
                round((percentile_cont(0.50) within group (
                  order by output_tokens::double precision * 1000.0
                    / greatest(latency_ms - first_token_ms, 1)
                ) filter (where {fact} and output_tokens > 0
                          and latency_ms > first_token_ms)))::bigint as output_throughput_p50,
                round((percentile_cont(0.90) within group (
                  order by output_tokens::double precision * 1000.0
                    / greatest(latency_ms - first_token_ms, 1)
                ) filter (where {fact} and output_tokens > 0
                          and latency_ms > first_token_ms)))::bigint as output_throughput_p90,
                count(capacity_total_slots)::bigint as capacity_sample_count,
                round(avg(capacity_used_slots::double precision
                          / nullif(capacity_total_slots, 0)) * 10000)::bigint
                  as capacity_utilization_avg_basis_points,
                round(percentile_cont(0.95) within group (
                  order by capacity_used_slots::double precision
                    / nullif(capacity_total_slots, 0)
                ) * 10000)::bigint as capacity_utilization_p95_basis_points
         from model_requests mr where mr.started_at >= "
    ));
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    push_unrecovered_request_filter(&mut query, "mr");
    push_usage_filter(&mut query, filter, "mr");
    let row = query
        .build()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("load request metrics"))?;
    request_metrics_from_row(&row)
}

pub(crate) async fn dashboard_totals(pool: &PgPool) -> StoreResult<DashboardTotals> {
    // 卡片脚注的“总计”覆盖全部历史；只聚合页面实际展示的字段，避免重复计算
    // 当前区间指标所需的延迟分位数。
    let fact = completed_usage_fact_predicate("mr");
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "select count(*)::bigint as request_count,
                coalesce(sum(input_tokens) filter (where {fact}), 0)::bigint as input_tokens,
                coalesce(sum(cached_tokens) filter (where {fact}), 0)::bigint as cached_tokens,
                coalesce(sum(total_tokens) filter (where {fact}), 0)::bigint as total_tokens,
                sum(cost_amount) filter (where {fact} and cost_currency = 'USD')::text
                  as billing_usd
           from model_requests mr
          where mr.recovered_at is null"
    ));
    let row = query
        .build()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("load dashboard totals"))?;
    Ok(DashboardTotals {
        request_count: unsigned(&row, "request_count")?,
        input_tokens: unsigned(&row, "input_tokens")?,
        cached_tokens: unsigned(&row, "cached_tokens")?,
        total_tokens: unsigned(&row, "total_tokens")?,
        billing_usd: optional_decimal(&row, "billing_usd")?,
    })
}

pub(crate) async fn request_metric_series(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<Vec<RequestMetricPoint>> {
    request_metric_series_inner(pool, range, filter, true).await
}

pub(crate) async fn dashboard_request_metric_series(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<Vec<RequestMetricPoint>> {
    request_metric_series_inner(pool, range, filter, false).await
}

async fn request_metric_series_inner(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
    load_costs: bool,
) -> StoreResult<Vec<RequestMetricPoint>> {
    filter.validate()?;
    let granularity = granularity_for(range);
    // 与 request_metrics 同一契约：结果计数覆盖全部请求，用量/延迟/成本
    // 聚合仅统计用量事实。
    let fact = completed_usage_fact_predicate("mr");
    let mut query = QueryBuilder::<Postgres>::new("select date_bin(");
    query.push_bind(granularity.sql_interval());
    query.push(format!(
        "::interval, mr.started_at, timestamptz '1970-01-01 00:00:00+00') as bucket_start,
                count(*)::bigint as request_count,
                count(*) filter (where outcome = 'succeeded')::bigint as success_count,
                count(*) filter (where outcome = 'failed')::bigint as failure_count,
                count(*) filter (where outcome = 'cancelled')::bigint as cancelled_count,
                count(*) filter (where outcome = 'incomplete')::bigint as incomplete_count,
                count(*) filter (where client_status_code between 400 and 499)::bigint
                  as caller_error_count,
                coalesce(sum(input_tokens) filter (where {fact}), 0)::bigint as input_tokens,
                coalesce(sum(output_tokens) filter (where {fact}), 0)::bigint as output_tokens,
                coalesce(sum(cached_tokens) filter (where {fact}), 0)::bigint as cached_tokens,
                coalesce(sum(cache_write_tokens) filter (where {fact}), 0)::bigint
                  as cache_write_tokens,
                coalesce(sum(reasoning_tokens) filter (where {fact}), 0)::bigint
                  as reasoning_tokens,
                coalesce(sum(total_tokens) filter (where {fact}), 0)::bigint as total_tokens,
                coalesce(sum(first_token_ms) filter (where {fact}), 0)::bigint
                  as first_token_latency_sum,
                count(first_token_ms) filter (where {fact})::bigint as first_token_latency_count,
                coalesce(sum(latency_ms) filter (where {fact}), 0)::bigint as latency_sum,
                count(latency_ms) filter (where {fact})::bigint as latency_count,
                max(latency_ms) filter (where {fact})::bigint as max_latency_ms,
                min(latency_ms) filter (where {fact})::bigint as min_latency_ms,
                count(*) filter (where {fact} and input_tokens is not null)::bigint
                  as cache_eligible_request_count,
                count(*) filter (where {fact} and input_tokens is not null
                                   and cached_tokens > 0)::bigint
                  as cache_hit_request_count,
                percentile_cont(0.50) within group (order by latency_ms)
                  filter (where {fact}) as latency_p50_ms,
                percentile_cont(0.95) within group (order by latency_ms)
                  filter (where {fact}) as latency_p95_ms,
                percentile_cont(0.99) within group (order by latency_ms)
                  filter (where {fact}) as latency_p99_ms,
                percentile_cont(0.50) within group (order by first_token_ms)
                  filter (where {fact}) as first_token_p50_ms,
                percentile_cont(0.95) within group (order by first_token_ms)
                  filter (where {fact}) as first_token_p95_ms,
                percentile_cont(0.99) within group (order by first_token_ms)
                  filter (where {fact}) as first_token_p99_ms,
                count(admission_decision_ms)::bigint as admission_decision_count,
                percentile_cont(0.50) within group (order by admission_decision_ms)
                  as admission_decision_p50_ms,
                percentile_cont(0.95) within group (order by admission_decision_ms)
                  as admission_decision_p95_ms,
                percentile_cont(0.99) within group (order by admission_decision_ms)
                  as admission_decision_p99_ms,
                count(account_selection_wait_ms)::bigint as account_selection_wait_count,
                percentile_cont(0.50) within group (order by account_selection_wait_ms)
                  as account_selection_wait_p50_ms,
                percentile_cont(0.95) within group (order by account_selection_wait_ms)
                  as account_selection_wait_p95_ms,
                percentile_cont(0.99) within group (order by account_selection_wait_ms)
                  as account_selection_wait_p99_ms,
                round((percentile_cont(0.10) within group (
                  order by output_tokens::double precision * 1000.0
                    / greatest(latency_ms - first_token_ms, 1)
                ) filter (where {fact} and output_tokens > 0
                          and latency_ms > first_token_ms)))::bigint as output_throughput_p10,
                round((percentile_cont(0.50) within group (
                  order by output_tokens::double precision * 1000.0
                    / greatest(latency_ms - first_token_ms, 1)
                ) filter (where {fact} and output_tokens > 0
                          and latency_ms > first_token_ms)))::bigint as output_throughput_p50,
                round((percentile_cont(0.90) within group (
                  order by output_tokens::double precision * 1000.0
                    / greatest(latency_ms - first_token_ms, 1)
                ) filter (where {fact} and output_tokens > 0
                          and latency_ms > first_token_ms)))::bigint as output_throughput_p90,
                count(capacity_total_slots)::bigint as capacity_sample_count,
                round(avg(capacity_used_slots::double precision
                          / nullif(capacity_total_slots, 0)) * 10000)::bigint
                  as capacity_utilization_avg_basis_points,
                round(percentile_cont(0.95) within group (
                  order by capacity_used_slots::double precision
                    / nullif(capacity_total_slots, 0)
                ) * 10000)::bigint as capacity_utilization_p95_basis_points,
                count(*) filter (where {fact} and cost_source = 'provider_reported')::bigint
                  as provider_reported_count,
                count(*) filter (where {fact} and cost_source = 'calculated')::bigint
                  as calculated_count,
                count(*) filter (where {fact} and cost_source = 'unavailable')::bigint
                  as unavailable_count
         from model_requests mr where mr.started_at >= "
    ));
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    push_unrecovered_request_filter(&mut query, "mr");
    push_usage_filter(&mut query, filter, "mr");
    query.push(" group by bucket_start order by bucket_start");
    let rows = query
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load request metric series"))?;

    let mut points = BTreeMap::new();
    for row in rows {
        let bucket_start = get(&row, "bucket_start")?;
        points.insert(
            bucket_start,
            RequestMetricPoint {
                bucket_start,
                granularity,
                metrics: request_metrics_from_row(&row)?,
                cost_coverage: coverage_from_row(&row)?,
                costs: Vec::new(),
            },
        );
    }
    if load_costs {
        let bucket_costs = request_costs_by_bucket(pool, range, filter, granularity).await?;
        for (bucket, costs) in bucket_costs {
            if let Some(point) = points.get_mut(&bucket) {
                point.costs = costs;
            }
        }
    }
    fill_metric_gaps(range, granularity, points)
}

pub(crate) fn calculated_usage_billing_facts(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: UsageRecordFilter,
) -> futures::stream::BoxStream<'_, StoreResult<CalculatedUsageBillingFact>> {
    use futures::TryStreamExt;

    Box::pin(async_stream::try_stream! {
        filter.validate()?;
        let granularity = granularity_for(range);
        let mut query = QueryBuilder::<Postgres>::new("select date_bin(");
        query.push_bind(granularity.sql_interval());
        query.push(
            "::interval, mr.started_at, timestamptz '1970-01-01 00:00:00+00') as bucket_start,
                    mr.provider_kind, mr.upstream_model_id, mr.service_tier,
                    mr.input_tokens, mr.output_tokens, mr.cached_tokens, mr.cache_write_tokens,
                    mr.cost_currency, mr.cost_amount::text as amount
             from model_requests mr where mr.started_at >= ",
        );
        query.push_bind(range.start);
        query.push(" and mr.started_at < ");
        query.push_bind(range.end);
        query.push(
            " and mr.cost_source = 'calculated'
               and mr.cost_amount is not null and mr.cost_currency is not null
               and nullif(mr.provider_kind, '') is not null
               and nullif(mr.upstream_model_id, '') is not null",
        );
        push_completed_usage_fact_filter(&mut query, "mr");
        push_usage_filter(&mut query, &filter, "mr");
        // 费用逐条累加与行顺序无关，避免全量物化和不必要的数据库排序。
        let mut rows = query.build().fetch(pool);
        while let Some(row) = rows.try_next().await
            .map_err(|_| postgres_unavailable("load calculated usage billing facts"))?
        {
            yield calculated_usage_billing_fact_from_row(&row)?;
        }
    })
}

pub(crate) async fn request_costs_by_bucket(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
    granularity: ObservationGranularity,
) -> StoreResult<BTreeMap<DateTime<Utc>, Vec<CurrencyCostTotal>>> {
    let mut query = QueryBuilder::<Postgres>::new("select date_bin(");
    query.push_bind(granularity.sql_interval());
    query.push(
        "::interval, mr.started_at, timestamptz '1970-01-01 00:00:00+00') as bucket_start,
                mr.cost_currency, sum(mr.cost_amount)::text as amount
         from model_requests mr
         where mr.started_at >= ",
    );
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    query.push(" and mr.cost_amount is not null and mr.cost_currency is not null");
    push_completed_usage_fact_filter(&mut query, "mr");
    push_usage_filter(&mut query, filter, "mr");
    query.push(" group by bucket_start, mr.cost_currency order by bucket_start, mr.cost_currency");
    let rows = query
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load request costs by bucket"))?;
    let mut result = BTreeMap::<DateTime<Utc>, Vec<CurrencyCostTotal>>::new();
    for row in rows {
        result
            .entry(get(&row, "bucket_start")?)
            .or_default()
            .push(cost_from_row(&row)?);
    }
    Ok(result)
}

pub(crate) async fn attempt_metrics(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<AttemptMetrics> {
    filter.validate()?;
    let fact = completed_usage_fact_predicate("sr");
    // PERF: `model_requests` 包含宽请求元数据。这里只物化两个聚合族会复用的列，
    // 避免生产基数下 `mr.*` 与多次标量子查询把 CTE 反复写入临时文件。
    let mut query = QueryBuilder::<Postgres>::new(
        "with selected_requests as materialized (
           select mr.id, mr.attempt_count, mr.outcome, mr.error_kind,
                  mr.upstream_status_code, mr.client_status_code,
                  mr.downstream_committed_at, mr.client_transport, mr.cost_source
           from model_requests mr where mr.started_at >= ",
    );
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    push_unrecovered_request_filter(&mut query, "mr");
    push_usage_filter(&mut query, filter, "mr");
    query.push(format!(
        "), request_aggregate as (
           select coalesce(sum(sr.attempt_count), 0)::bigint as attempt_count,
                  count(*) filter (
                    where sr.outcome = 'succeeded' and sr.attempt_count > 0
                  )::bigint as success_count,
                  count(*) filter (
                    where sr.outcome = 'cancelled' and sr.attempt_count > 0
                  )::bigint as cancelled_count,
                  count(*) filter (
                    where sr.outcome = 'incomplete' and sr.attempt_count > 0
                  )::bigint as incomplete_count,
                  count(*) filter (
                    where sr.outcome = 'failed' and sr.attempt_count > 0
                  )::bigint as request_failure_count,
                  count(*) filter (
                    where sr.outcome = 'failed' and sr.attempt_count > 0
                      and (coalesce(sr.error_kind, 'failed') in
                             ('rate_limited', 'quota_exhausted')
                           or coalesce(sr.upstream_status_code,
                                       sr.client_status_code) = 429)
                  )::bigint as request_rate_limited_count,
                  count(*) filter (
                    where sr.outcome = 'failed' and sr.attempt_count > 0
                      and (coalesce(sr.error_kind, 'failed') in
                             ('authentication', 'authorization', 'invalid_credential')
                           or coalesce(sr.upstream_status_code,
                                       sr.client_status_code) in (401, 403))
                  )::bigint as request_auth_failure_count,
                  count(*) filter (
                    where sr.outcome = 'failed' and sr.attempt_count > 0
                      and coalesce(sr.upstream_status_code,
                                   sr.client_status_code) between 500 and 599
                  )::bigint as request_provider_5xx_count,
                  count(*) filter (
                    where {fact} and sr.cost_source = 'provider_reported'
                  )::bigint as provider_reported_count,
                  count(*) filter (
                    where {fact} and sr.cost_source = 'calculated'
                  )::bigint as calculated_count,
                  count(*) filter (
                    where {fact} and sr.cost_source = 'unavailable'
                  )::bigint as unavailable_count
           from selected_requests sr
         ), ops_aggregate as (
           select coalesce(sum(coalesce(oe.occurrence_count, 1)), 0)::bigint
                    as ops_failure_count,
                  coalesce(sum(coalesce(oe.occurrence_count, 1)) filter (
                    where oe.failure_kind in ('rate_limited', 'quota_exhausted')
                       or oe.status_code = 429
                  ), 0)::bigint as ops_rate_limited_count,
                  coalesce(sum(coalesce(oe.occurrence_count, 1)) filter (
                    where oe.failure_kind in
                            ('authentication', 'authorization', 'invalid_credential')
                       or oe.status_code in (401, 403)
                  ), 0)::bigint as ops_auth_failure_count,
                  coalesce(sum(coalesce(oe.occurrence_count, 1)) filter (
                    where oe.status_code between 500 and 599
                  ), 0)::bigint as ops_provider_5xx_count
           from ops_events oe
           join selected_requests sr on sr.id = oe.model_request_id
         )
         select requests.attempt_count,
                requests.success_count,
                requests.request_failure_count + ops.ops_failure_count as failure_count,
                requests.cancelled_count,
                requests.incomplete_count,
                requests.request_rate_limited_count + ops.ops_rate_limited_count
                  as rate_limited_count,
                requests.request_auth_failure_count + ops.ops_auth_failure_count
                  as auth_failure_count,
                requests.request_provider_5xx_count + ops.ops_provider_5xx_count
                  as provider_5xx_count,
                requests.provider_reported_count,
                requests.calculated_count,
                requests.unavailable_count
         from request_aggregate requests
         cross join ops_aggregate ops"
    ));
    let row = query
        .build()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("load attempt metrics"))?;
    Ok(AttemptMetrics {
        attempt_count: unsigned(&row, "attempt_count")?,
        success_count: unsigned(&row, "success_count")?,
        failure_count: unsigned(&row, "failure_count")?,
        cancelled_count: unsigned(&row, "cancelled_count")?,
        incomplete_count: unsigned(&row, "incomplete_count")?,
        rate_limited_count: unsigned(&row, "rate_limited_count")?,
        auth_failure_count: unsigned(&row, "auth_failure_count")?,
        provider_5xx_count: unsigned(&row, "provider_5xx_count")?,
        cost_coverage: coverage_from_row(&row)?,
        costs: request_costs(pool, range, filter).await?,
    })
}

pub(crate) async fn request_costs(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<Vec<CurrencyCostTotal>> {
    let mut query = QueryBuilder::<Postgres>::new(
        "select mr.cost_currency, sum(mr.cost_amount)::text as amount
         from model_requests mr where mr.started_at >= ",
    );
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    query.push(" and mr.cost_amount is not null and mr.cost_currency is not null");
    push_completed_usage_fact_filter(&mut query, "mr");
    push_usage_filter(&mut query, filter, "mr");
    query.push(" group by mr.cost_currency order by mr.cost_currency");
    query
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load request costs"))?
        .iter()
        .map(cost_from_row)
        .collect()
}

pub(crate) async fn provider_observations(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<Vec<ProviderObservation>> {
    let fact = completed_usage_fact_predicate("mr");
    // 请求、attempt 与失败覆盖执行审计；token 只累计完整交付的用量事实。
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "select coalesce(mr.provider_kind, 'unrouted') as provider_kind,
                count(*)::bigint as request_count,
                coalesce(sum(mr.attempt_count), 0)::bigint as attempt_count,
                count(*) filter (where mr.outcome = 'failed')::bigint as failure_count,
                coalesce(sum(mr.total_tokens) filter (where {fact}), 0)::bigint
                  as total_tokens
         from model_requests mr where mr.started_at >= "
    ));
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    push_unrecovered_request_filter(&mut query, "mr");
    push_usage_filter(&mut query, filter, "mr");
    query.push(" group by coalesce(mr.provider_kind, 'unrouted') order by request_count desc");
    let rows = query
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load provider observations"))?;
    rows.iter()
        .map(|row| {
            Ok(ProviderObservation {
                provider_kind: get(row, "provider_kind")?,
                request_count: unsigned(row, "request_count")?,
                attempt_count: unsigned(row, "attempt_count")?,
                failure_count: unsigned(row, "failure_count")?,
                total_tokens: unsigned(row, "total_tokens")?,
            })
        })
        .collect()
}

pub(crate) fn request_metrics_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<RequestMetrics> {
    Ok(RequestMetrics {
        request_count: unsigned(row, "request_count")?,
        success_count: unsigned(row, "success_count")?,
        failure_count: unsigned(row, "failure_count")?,
        cancelled_count: unsigned(row, "cancelled_count")?,
        incomplete_count: unsigned(row, "incomplete_count")?,
        caller_error_count: unsigned(row, "caller_error_count")?,
        input_tokens: unsigned(row, "input_tokens")?,
        output_tokens: unsigned(row, "output_tokens")?,
        cached_tokens: unsigned(row, "cached_tokens")?,
        cache_write_tokens: unsigned(row, "cache_write_tokens")?,
        reasoning_tokens: unsigned(row, "reasoning_tokens")?,
        total_tokens: unsigned(row, "total_tokens")?,
        first_token_latency_sum: unsigned(row, "first_token_latency_sum")?,
        first_token_latency_count: unsigned(row, "first_token_latency_count")?,
        latency_sum: unsigned(row, "latency_sum")?,
        latency_count: unsigned(row, "latency_count")?,
        max_latency_ms: optional_unsigned(row, "max_latency_ms")?,
        min_latency_ms: optional_unsigned(row, "min_latency_ms")?,
        cache_eligible_request_count: unsigned(row, "cache_eligible_request_count")?,
        cache_hit_request_count: unsigned(row, "cache_hit_request_count")?,
        latency_percentiles: LatencyPercentiles {
            p50_ms: optional_percentile(row, "latency_p50_ms")?,
            p95_ms: optional_percentile(row, "latency_p95_ms")?,
            p99_ms: optional_percentile(row, "latency_p99_ms")?,
        },
        first_token_latency_percentiles: LatencyPercentiles {
            p50_ms: optional_percentile(row, "first_token_p50_ms")?,
            p95_ms: optional_percentile(row, "first_token_p95_ms")?,
            p99_ms: optional_percentile(row, "first_token_p99_ms")?,
        },
        admission_decision_count: unsigned(row, "admission_decision_count")?,
        admission_decision_percentiles: LatencyPercentiles {
            p50_ms: optional_percentile(row, "admission_decision_p50_ms")?,
            p95_ms: optional_percentile(row, "admission_decision_p95_ms")?,
            p99_ms: optional_percentile(row, "admission_decision_p99_ms")?,
        },
        account_selection_wait_count: unsigned(row, "account_selection_wait_count")?,
        account_selection_wait_percentiles: LatencyPercentiles {
            p50_ms: optional_percentile(row, "account_selection_wait_p50_ms")?,
            p95_ms: optional_percentile(row, "account_selection_wait_p95_ms")?,
            p99_ms: optional_percentile(row, "account_selection_wait_p99_ms")?,
        },
        output_throughput_p10: optional_unsigned(row, "output_throughput_p10")?,
        output_throughput_p50: optional_unsigned(row, "output_throughput_p50")?,
        output_throughput_p90: optional_unsigned(row, "output_throughput_p90")?,
        capacity_sample_count: unsigned(row, "capacity_sample_count")?,
        capacity_utilization_avg_basis_points: optional_unsigned(
            row,
            "capacity_utilization_avg_basis_points",
        )?,
        capacity_utilization_p95_basis_points: optional_unsigned(
            row,
            "capacity_utilization_p95_basis_points",
        )?,
    })
}

pub(crate) fn optional_percentile(
    row: &sqlx::postgres::PgRow,
    column: &str,
) -> StoreResult<Option<PercentileMilliseconds>> {
    row.try_get::<Option<f64>, _>(column)
        .map_err(|_| postgres_unavailable("decode latency percentile"))?
        .map(PercentileMilliseconds::new)
        .transpose()
}

pub(crate) fn coverage_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<CostCoverage> {
    Ok(CostCoverage {
        provider_reported_count: unsigned(row, "provider_reported_count")?,
        calculated_count: unsigned(row, "calculated_count")?,
        unavailable_count: unsigned(row, "unavailable_count")?,
    })
}

pub(crate) fn granularity_for(range: ObservabilityRange) -> ObservationGranularity {
    let seconds = range.end.signed_duration_since(range.start).num_seconds();
    if seconds <= 2 * 24 * 60 * 60 {
        ObservationGranularity::FifteenMinutes
    } else if seconds <= 31 * 24 * 60 * 60 {
        ObservationGranularity::Hour
    } else {
        ObservationGranularity::Day
    }
}

pub(crate) fn fill_metric_gaps(
    range: ObservabilityRange,
    granularity: ObservationGranularity,
    mut points: BTreeMap<DateTime<Utc>, RequestMetricPoint>,
) -> StoreResult<Vec<RequestMetricPoint>> {
    let seconds = granularity.seconds();
    let start_epoch = range.start.timestamp().div_euclid(seconds) * seconds;
    let mut bucket = DateTime::from_timestamp(start_epoch, 0)
        .ok_or_else(|| invalid("metric range start is outside supported timestamps"))?;
    let step = TimeDelta::seconds(seconds);
    let mut result = Vec::new();
    while bucket < range.end {
        result.push(points.remove(&bucket).unwrap_or(RequestMetricPoint {
            bucket_start: bucket,
            granularity,
            metrics: RequestMetrics::default(),
            cost_coverage: CostCoverage::default(),
            costs: Vec::new(),
        }));
        bucket = bucket
            .checked_add_signed(step)
            .ok_or_else(|| invalid("metric range exceeds supported timestamps"))?;
    }
    Ok(result)
}
