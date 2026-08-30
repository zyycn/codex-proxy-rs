//! Provider 账号用量查询族。

use super::super::*;

pub(crate) async fn provider_account_usage(
    pool: &PgPool,
    query: ProviderAccountUsageQuery,
) -> StoreResult<Vec<ProviderAccountUsageObservation>> {
    if let Some(account_ids) = &query.account_ids {
        validate_account_ids(account_ids)?;
    }
    if query.limit == 0 || usize::from(query.limit) > MAX_ACCOUNT_IDS {
        return Err(invalid("account usage limit must be between 1 and 200"));
    }

    let mut statement = QueryBuilder::<Postgres>::new(
        "with matched as (
           select pa.id, pa.provider_kind, pa.authentication_kind, pa.name, pa.email,
                  pa.plan_type, mr.id as request_id,
                  coalesce(mr.upstream_model_id, mr.requested_model_id) as model,
                  mr.cost_currency, mr.outcome, mr.input_tokens, mr.output_tokens,
                  mr.cached_tokens, mr.cache_write_tokens, mr.reasoning_tokens,
                  mr.image_input_tokens, mr.image_output_tokens,
                  mr.image_generation_succeeded, mr.total_tokens, mr.cost_source,
                  mr.cost_amount, mr.started_at
             from provider_accounts pa
             left join model_requests mr
               on mr.provider_account_ref = pa.id and mr.started_at >= ",
    );
    statement.push_bind(query.range.start);
    statement.push(" and mr.started_at < ");
    statement.push_bind(query.range.end);
    push_completed_usage_fact_filter(&mut statement, "mr");
    if let Some(account_ids) = &query.account_ids {
        statement.push(" where pa.id = any(");
        statement.push_bind(account_ids.clone());
        statement.push("::text[])");
    }
    statement.push(
        "), aggregated as (
           select id, provider_kind, authentication_kind, name, email, plan_type,
                  model, cost_currency,
                  grouping(model)::integer as model_grouping,
                  grouping(cost_currency)::integer as currency_grouping,
                  count(request_id)::bigint as request_count,
                  count(request_id) filter (where outcome = 'succeeded')::bigint
                    as success_count,
                  sum(input_tokens)::bigint as input_tokens,
                  sum(output_tokens)::bigint as output_tokens,
                  sum(cached_tokens)::bigint as cached_tokens,
                  sum(cache_write_tokens)::bigint as cache_write_tokens,
                  sum(reasoning_tokens)::bigint as reasoning_tokens,
                  sum(image_input_tokens)::bigint as image_input_tokens,
                  sum(image_output_tokens)::bigint as image_output_tokens,
                  count(request_id) filter (where image_generation_succeeded is true)::bigint
                    as image_request_count,
                  count(request_id) filter (where image_generation_succeeded is false)::bigint
                    as image_request_failed_count,
                  sum(total_tokens)::bigint as total_tokens,
                  count(request_id) filter (where cost_source = 'provider_reported')::bigint
                    as provider_reported_count,
                  count(request_id) filter (where cost_source = 'calculated')::bigint
                    as calculated_count,
                  count(request_id) filter (where cost_source = 'unavailable')::bigint
                    as unavailable_count,
                  max(started_at) as last_used_at,
                  sum(cost_amount)::text as amount
             from matched
            group by id, provider_kind, authentication_kind, name, email, plan_type,
                     grouping sets ((), (cost_currency), (model), (model, cost_currency))
         ), selected_accounts as (
           select id,
                  row_number() over (order by last_used_at desc nulls last, name, id)
                    as sort_position
             from aggregated
            where model_grouping = 1 and currency_grouping = 1",
    );
    if query.account_ids.is_none() {
        statement.push(" and request_count > 0");
    }
    statement.push(" order by last_used_at desc nulls last, name, id limit ");
    statement.push_bind(i64::from(query.limit));
    statement.push(
        ")
         select aggregated.*
           from aggregated
           join selected_accounts selected using (id)
          order by selected.sort_position, model_grouping desc, currency_grouping desc,
                   model nulls last, cost_currency nulls last",
    );
    let rows = statement
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load provider account usage"))?;

    let mut observations = Vec::with_capacity(usize::from(query.limit));
    let mut costs = HashMap::<String, Vec<CurrencyCostTotal>>::new();
    let mut models = HashMap::<String, Vec<ProviderAccountModelUsageObservation>>::new();
    let mut model_costs = HashMap::<(String, String), Vec<CurrencyCostTotal>>::new();
    for row in &rows {
        let model_grouping: i32 = get(row, "model_grouping")?;
        let currency_grouping: i32 = get(row, "currency_grouping")?;
        match (model_grouping, currency_grouping) {
            (1, 1) => observations.push(provider_account_from_row(row)?),
            (1, 0) if get::<Option<String>>(row, "cost_currency")?.is_some() => {
                costs
                    .entry(get(row, "id")?)
                    .or_default()
                    .push(cost_from_row(row)?);
            }
            (0, 1) if get::<Option<String>>(row, "model")?.is_some() => {
                models
                    .entry(get(row, "id")?)
                    .or_default()
                    .push(provider_account_model_from_row(row)?);
            }
            (0, 0)
                if get::<Option<String>>(row, "model")?.is_some()
                    && get::<Option<String>>(row, "cost_currency")?.is_some() =>
            {
                model_costs
                    .entry((get(row, "id")?, get(row, "model")?))
                    .or_default()
                    .push(cost_from_row(row)?);
            }
            (0 | 1, 0 | 1) => {}
            _ => {
                return Err(postgres_unavailable(
                    "decode provider account usage grouping",
                ));
            }
        }
    }
    if observations.is_empty() {
        return Ok(observations);
    }
    let account_ids = observations
        .iter()
        .map(|item| item.account_id.clone())
        .collect::<Vec<_>>();
    let mut request_buckets = if query.include_hourly_request_buckets {
        provider_account_request_buckets(pool, query.range, &account_ids).await?
    } else {
        HashMap::new()
    };
    for observation in &mut observations {
        observation.costs = costs.remove(&observation.account_id).unwrap_or_default();
        observation.request_buckets = request_buckets
            .remove(&observation.account_id)
            .unwrap_or_default();
        observation.models = models.remove(&observation.account_id).unwrap_or_default();
        for model in &mut observation.models {
            model.costs = model_costs
                .remove(&(observation.account_id.clone(), model.model.clone()))
                .unwrap_or_default();
        }
        observation.models.sort_by(|left, right| {
            right
                .request_count
                .cmp(&left.request_count)
                .then_with(|| left.model.cmp(&right.model))
        });
    }
    Ok(observations)
}

pub(crate) async fn provider_account_request_buckets(
    pool: &PgPool,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<HashMap<String, Vec<ProviderAccountRequestBucket>>> {
    let completed_usage = completed_usage_fact_predicate("mr");
    let statement = format!(
        "select provider_account_ref,
                floor(extract(epoch from (started_at - $1)) / 3600)::bigint as bucket_index,
                count(*)::bigint as request_count
         from model_requests mr
         where mr.provider_account_ref = any($2::text[])
           and mr.started_at >= $1 and mr.started_at < $3
           and {completed_usage}
         group by mr.provider_account_ref, bucket_index
         order by mr.provider_account_ref, bucket_index"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
        .bind(range.start)
        .bind(account_ids)
        .bind(range.end)
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load provider account request timeline"))?;

    let mut observed = HashMap::<String, BTreeMap<u64, u64>>::new();
    for row in rows {
        observed
            .entry(get(&row, "provider_account_ref")?)
            .or_default()
            .insert(
                unsigned(&row, "bucket_index")?,
                unsigned(&row, "request_count")?,
            );
    }

    let step = TimeDelta::hours(1);
    let mut timelines = HashMap::with_capacity(account_ids.len());
    for account_id in account_ids {
        let mut account_observed = observed.remove(account_id).unwrap_or_default();
        let mut bucket_start = range.start;
        let mut bucket_index = 0_u64;
        let mut buckets = Vec::new();
        while bucket_start < range.end {
            buckets.push(ProviderAccountRequestBucket {
                bucket_start,
                request_count: account_observed.remove(&bucket_index).unwrap_or_default(),
            });
            bucket_start = bucket_start
                .checked_add_signed(step)
                .ok_or_else(|| invalid("account request timeline exceeds supported timestamps"))?;
            bucket_index = bucket_index
                .checked_add(1)
                .ok_or_else(|| invalid("account request timeline exceeds supported buckets"))?;
        }
        timelines.insert(account_id.clone(), buckets);
    }
    Ok(timelines)
}

fn provider_account_model_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<ProviderAccountModelUsageObservation> {
    Ok(ProviderAccountModelUsageObservation {
        model: get(row, "model")?,
        request_count: unsigned(row, "request_count")?,
        success_count: unsigned(row, "success_count")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_tokens: optional_unsigned(row, "cached_tokens")?,
        cache_write_tokens: optional_unsigned(row, "cache_write_tokens")?,
        reasoning_tokens: optional_unsigned(row, "reasoning_tokens")?,
        image_input_tokens: optional_unsigned(row, "image_input_tokens")?,
        image_output_tokens: optional_unsigned(row, "image_output_tokens")?,
        image_request_count: unsigned(row, "image_request_count")?,
        image_request_failed_count: unsigned(row, "image_request_failed_count")?,
        total_tokens: optional_unsigned(row, "total_tokens")?,
        cost_coverage: coverage_from_row(row)?,
        costs: Vec::new(),
        last_used_at: get(row, "last_used_at")?,
    })
}

pub(crate) fn provider_account_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<ProviderAccountUsageObservation> {
    Ok(ProviderAccountUsageObservation {
        account_id: get(row, "id")?,
        provider_kind: get(row, "provider_kind")?,
        authentication_kind: get(row, "authentication_kind")?,
        name: get(row, "name")?,
        email: get(row, "email")?,
        plan_type: get(row, "plan_type")?,
        request_count: unsigned(row, "request_count")?,
        success_count: unsigned(row, "success_count")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_tokens: optional_unsigned(row, "cached_tokens")?,
        cache_write_tokens: optional_unsigned(row, "cache_write_tokens")?,
        reasoning_tokens: optional_unsigned(row, "reasoning_tokens")?,
        image_input_tokens: optional_unsigned(row, "image_input_tokens")?,
        image_output_tokens: optional_unsigned(row, "image_output_tokens")?,
        image_request_count: unsigned(row, "image_request_count")?,
        image_request_failed_count: unsigned(row, "image_request_failed_count")?,
        total_tokens: optional_unsigned(row, "total_tokens")?,
        cost_coverage: coverage_from_row(row)?,
        costs: Vec::new(),
        last_used_at: get(row, "last_used_at")?,
        request_buckets: Vec::new(),
        models: Vec::new(),
    })
}
