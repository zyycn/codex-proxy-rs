//! 账号用量查询族。

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
        "select pa.id, pa.provider_kind, pa.authentication_kind, pa.name, pa.email,
                pa.plan_type, pa.enabled, pa.availability,
                count(mr.id)::bigint as request_count,
                count(mr.id) filter (where mr.outcome = 'succeeded')::bigint as success_count,
                sum(mr.input_tokens)::bigint as input_tokens,
                sum(mr.output_tokens)::bigint as output_tokens,
                sum(mr.cached_tokens)::bigint as cached_tokens,
                sum(mr.cache_write_tokens)::bigint as cache_write_tokens,
                sum(mr.reasoning_tokens)::bigint as reasoning_tokens,
                sum(mr.image_input_tokens)::bigint as image_input_tokens,
                sum(mr.image_output_tokens)::bigint as image_output_tokens,
                count(mr.id) filter (where mr.image_generation_succeeded is true)::bigint
                  as image_request_count,
                count(mr.id) filter (where mr.image_generation_succeeded is false)::bigint
                  as image_request_failed_count,
                sum(mr.total_tokens)::bigint as total_tokens,
                count(mr.id) filter (where mr.cost_source = 'provider_reported')::bigint
                  as provider_reported_count,
                count(mr.id) filter (where mr.cost_source = 'calculated')::bigint
                  as calculated_count,
                count(mr.id) filter (where mr.cost_source = 'unavailable')::bigint
                  as unavailable_count,
                max(mr.started_at) as last_used_at
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
        " group by pa.id, pa.provider_kind, pa.authentication_kind, pa.name, pa.email,
                   pa.plan_type, pa.enabled, pa.availability
          order by max(mr.started_at) desc nulls last, pa.name, pa.id limit ",
    );
    statement.push_bind(i64::from(query.limit));
    let rows = statement
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load provider account usage"))?;

    let mut observations = rows
        .iter()
        .map(provider_account_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
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
    let account_costs = account_costs(pool, query.range, &account_ids).await?;
    let model_rows = account_model_rows(pool, query.range, &account_ids).await?;
    let model_costs = account_model_costs(pool, query.range, &account_ids).await?;
    let account_positions = observations
        .iter()
        .enumerate()
        .map(|(index, item)| (item.account_id.clone(), index))
        .collect::<HashMap<_, _>>();
    for observation in &mut observations {
        observation.costs = account_costs
            .get(&observation.account_id)
            .cloned()
            .unwrap_or_default();
        observation.request_buckets = request_buckets
            .remove(&observation.account_id)
            .unwrap_or_default();
    }
    for row in model_rows {
        let account_id: String = get(&row, "provider_account_ref")?;
        let Some(position) = account_positions.get(&account_id).copied() else {
            continue;
        };
        let model: String = get(&row, "model")?;
        observations[position]
            .models
            .push(ProviderAccountModelUsageObservation {
                costs: model_costs
                    .get(&(account_id, model.clone()))
                    .cloned()
                    .unwrap_or_default(),
                model,
                request_count: unsigned(&row, "request_count")?,
                success_count: unsigned(&row, "success_count")?,
                input_tokens: optional_unsigned(&row, "input_tokens")?,
                output_tokens: optional_unsigned(&row, "output_tokens")?,
                cached_tokens: optional_unsigned(&row, "cached_tokens")?,
                cache_write_tokens: optional_unsigned(&row, "cache_write_tokens")?,
                reasoning_tokens: optional_unsigned(&row, "reasoning_tokens")?,
                image_input_tokens: optional_unsigned(&row, "image_input_tokens")?,
                image_output_tokens: optional_unsigned(&row, "image_output_tokens")?,
                image_request_count: unsigned(&row, "image_request_count")?,
                image_request_failed_count: unsigned(&row, "image_request_failed_count")?,
                total_tokens: optional_unsigned(&row, "total_tokens")?,
                cost_coverage: coverage_from_row(&row)?,
                last_used_at: get(&row, "last_used_at")?,
            });
    }
    Ok(observations)
}

pub(crate) async fn provider_account_request_buckets(
    pool: &PgPool,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<HashMap<String, Vec<ProviderAccountRequestBucket>>> {
    let rows = sqlx::query(
        "select provider_account_ref,
                floor(extract(epoch from (started_at - $1)) / 3600)::bigint as bucket_index,
                count(*)::bigint as request_count
         from model_requests mr
         where mr.provider_account_ref = any($2::text[])
           and mr.started_at >= $1 and mr.started_at < $3
           and mr.outcome = 'succeeded'
           and mr.downstream_committed_at is not null
           and mr.client_status_code between 200 and 399
         group by mr.provider_account_ref, bucket_index
         order by mr.provider_account_ref, bucket_index",
    )
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

pub(crate) async fn account_costs(
    pool: &PgPool,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<HashMap<String, Vec<CurrencyCostTotal>>> {
    let rows = sqlx::query(
        "select mr.provider_account_ref, mr.cost_currency, sum(mr.cost_amount)::text as amount
         from model_requests mr
         where mr.provider_account_ref = any($1::text[])
           and mr.started_at >= $2 and mr.started_at < $3
           and mr.outcome = 'succeeded'
           and mr.downstream_committed_at is not null
           and mr.client_status_code between 200 and 399
           and mr.cost_amount is not null and mr.cost_currency is not null
         group by mr.provider_account_ref, mr.cost_currency
         order by mr.provider_account_ref, mr.cost_currency",
    )
    .bind(account_ids)
    .bind(range.start)
    .bind(range.end)
    .fetch_all(pool)
    .await
    .map_err(|_| postgres_unavailable("load provider account costs"))?;
    let mut costs = HashMap::<String, Vec<CurrencyCostTotal>>::new();
    for row in rows {
        costs
            .entry(get(&row, "provider_account_ref")?)
            .or_default()
            .push(cost_from_row(&row)?);
    }
    Ok(costs)
}

pub(crate) async fn account_model_rows(
    pool: &PgPool,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<Vec<sqlx::postgres::PgRow>> {
    sqlx::query(
        "select mr.provider_account_ref,
                coalesce(mr.upstream_model_id, mr.requested_model_id) as model,
                count(*)::bigint as request_count,
                count(*) filter (where mr.outcome = 'succeeded')::bigint as success_count,
                sum(mr.input_tokens)::bigint as input_tokens,
                sum(mr.output_tokens)::bigint as output_tokens,
                sum(mr.cached_tokens)::bigint as cached_tokens,
                sum(mr.cache_write_tokens)::bigint as cache_write_tokens,
                sum(mr.reasoning_tokens)::bigint as reasoning_tokens,
                sum(mr.image_input_tokens)::bigint as image_input_tokens,
                sum(mr.image_output_tokens)::bigint as image_output_tokens,
                count(*) filter (where mr.image_generation_succeeded is true)::bigint
                  as image_request_count,
                count(*) filter (where mr.image_generation_succeeded is false)::bigint
                  as image_request_failed_count,
                sum(mr.total_tokens)::bigint as total_tokens,
                count(*) filter (where mr.cost_source = 'provider_reported')::bigint
                  as provider_reported_count,
                count(*) filter (where mr.cost_source = 'calculated')::bigint
                  as calculated_count,
                count(*) filter (where mr.cost_source = 'unavailable')::bigint
                  as unavailable_count,
                max(mr.started_at) as last_used_at
         from model_requests mr
         where mr.provider_account_ref = any($1::text[])
           and mr.started_at >= $2 and mr.started_at < $3
           and mr.outcome = 'succeeded'
           and mr.downstream_committed_at is not null
           and mr.client_status_code between 200 and 399
         group by mr.provider_account_ref, coalesce(mr.upstream_model_id, mr.requested_model_id)
         order by mr.provider_account_ref, request_count desc, model",
    )
    .bind(account_ids)
    .bind(range.start)
    .bind(range.end)
    .fetch_all(pool)
    .await
    .map_err(|_| postgres_unavailable("load provider account model usage"))
}

pub(crate) async fn account_model_costs(
    pool: &PgPool,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<HashMap<(String, String), Vec<CurrencyCostTotal>>> {
    let rows = sqlx::query(
        "select mr.provider_account_ref,
                coalesce(mr.upstream_model_id, mr.requested_model_id) as model,
                mr.cost_currency, sum(mr.cost_amount)::text as amount
         from model_requests mr
         where mr.provider_account_ref = any($1::text[])
           and mr.started_at >= $2 and mr.started_at < $3
           and mr.outcome = 'succeeded'
           and mr.downstream_committed_at is not null
           and mr.client_status_code between 200 and 399
           and mr.cost_amount is not null and mr.cost_currency is not null
         group by mr.provider_account_ref, coalesce(mr.upstream_model_id, mr.requested_model_id),
                  mr.cost_currency
         order by mr.provider_account_ref, model, mr.cost_currency",
    )
    .bind(account_ids)
    .bind(range.start)
    .bind(range.end)
    .fetch_all(pool)
    .await
    .map_err(|_| postgres_unavailable("load provider account model costs"))?;
    let mut costs = HashMap::<(String, String), Vec<CurrencyCostTotal>>::new();
    for row in rows {
        costs
            .entry((get(&row, "provider_account_ref")?, get(&row, "model")?))
            .or_default()
            .push(cost_from_row(&row)?);
    }
    Ok(costs)
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
        enabled: get(row, "enabled")?,
        availability: get(row, "availability")?,
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
