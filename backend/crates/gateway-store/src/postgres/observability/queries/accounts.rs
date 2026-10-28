//! Provider 账号用量查询族

use super::super::*;

pub(crate) async fn provider_account_usage(
    snapshot: &mut analytics::AnalyticsSnapshot<'_>,
    query: ProviderAccountUsageQuery,
) -> StoreResult<Vec<ProviderAccountUsageObservation>> {
    if let Some(account_ids) = &query.account_ids {
        validate_account_ids(account_ids)?;
    }
    if query.limit == 0 || usize::from(query.limit) > MAX_ACCOUNT_IDS {
        return Err(invalid("account usage limit must be between 1 and 200"));
    }

    // 账号卡片先确定有限候选，避免为少量账号聚合整个历史范围
    let mut statement = QueryBuilder::<Postgres>::new("select pa.* from provider_accounts pa");
    if let Some(account_ids) = &query.account_ids {
        statement
            .push(" where pa.id=any(")
            .push_bind(account_ids.clone())
            .push("::text[])");
    } else {
        statement.push(" join lateral (select mr.started_at from model_requests mr where mr.provider_account_ref=pa.id and mr.started_at >= ").push_bind(query.range.start)
            .push(" and mr.started_at < ").push_bind(query.range.end);
        push_completed_usage_fact_filter(&mut statement, "mr");
        statement.push(" order by mr.started_at desc,mr.id desc limit 1) latest on true order by latest.started_at desc,pa.name,pa.id limit ").push_bind(i64::from(query.limit));
    }
    let rows = statement
        .build()
        .fetch_all(&mut *snapshot.tx)
        .await
        .map_err(|error| postgres_unavailable("select usage accounts", error))?;
    let account_ids = rows
        .iter()
        .map(|row| get(row, "id"))
        .collect::<StoreResult<Vec<String>>>()?;
    let mut models_by_account = HashMap::<String, Vec<ProviderAccountModelUsageObservation>>::new();
    let mut totals = BTreeMap::<String, analytics::MetricState>::new();
    for ((account, model), state) in snapshot
        .account_usage_states(query.range, &account_ids)
        .await?
    {
        totals.entry(account.clone()).or_default().merge(&state)?;
        if !model.is_empty()
            && let Some(last_used_at) = state.last_used_at()
        {
            models_by_account.entry(account).or_default().push(
                ProviderAccountModelUsageObservation {
                    model,
                    request_count: state.count("completed_request_count"),
                    success_count: state.count("completed_request_count"),
                    input_tokens: state.optional_count("input_tokens"),
                    output_tokens: state.optional_count("output_tokens"),
                    cached_input_tokens: state.optional_count("cached_input_tokens"),
                    cache_write_input_tokens: state.optional_count("cache_write_input_tokens"),
                    reasoning_output_tokens: state.optional_count("reasoning_output_tokens"),
                    image_input_tokens: state.optional_count("image_input_tokens"),
                    image_output_tokens: state.optional_count("image_output_tokens"),
                    total_tokens: state.optional_count("total_tokens"),
                    image_request_count: state.count("image_request_count"),
                    image_request_failed_count: state.count("image_request_failed_count"),
                    cost_coverage: state.coverage(),
                    costs: state.costs()?,
                    last_used_at,
                },
            );
        }
    }
    let mut request_buckets = if let Some(range) = query.request_bucket_range {
        provider_account_request_buckets(snapshot, range, &account_ids).await?
    } else {
        HashMap::new()
    };
    let mut observations = Vec::with_capacity(rows.len());
    for row in rows {
        let account_id: String = get(&row, "id")?;
        let state = totals.remove(&account_id).unwrap_or_default();
        let mut models = models_by_account.remove(&account_id).unwrap_or_default();
        models.sort_by(|a, b| {
            b.request_count
                .cmp(&a.request_count)
                .then(a.model.cmp(&b.model))
        });
        observations.push(ProviderAccountUsageObservation {
            provider_kind: get(&row, "provider_kind")?,
            authentication_kind: get(&row, "authentication_kind")?,
            name: get(&row, "name")?,
            email: get(&row, "email")?,
            notes: get(&row, "notes")?,
            plan_type: get(&row, "plan_type")?,
            request_count: state.count("completed_request_count"),
            success_count: state.count("completed_request_count"),
            input_tokens: state.optional_count("input_tokens"),
            output_tokens: state.optional_count("output_tokens"),
            cached_input_tokens: state.optional_count("cached_input_tokens"),
            cache_write_input_tokens: state.optional_count("cache_write_input_tokens"),
            reasoning_output_tokens: state.optional_count("reasoning_output_tokens"),
            image_input_tokens: state.optional_count("image_input_tokens"),
            image_output_tokens: state.optional_count("image_output_tokens"),
            total_tokens: state.optional_count("total_tokens"),
            image_request_count: state.count("image_request_count"),
            image_request_failed_count: state.count("image_request_failed_count"),
            cost_coverage: state.coverage(),
            costs: state.costs()?,
            last_used_at: state.last_used_at(),
            request_buckets: request_buckets.remove(&account_id).unwrap_or_default(),
            models,
            account_id,
        });
    }
    observations.sort_by(|a, b| {
        b.last_used_at
            .cmp(&a.last_used_at)
            .then(a.name.cmp(&b.name))
            .then(a.account_id.cmp(&b.account_id))
    });
    observations.truncate(usize::from(query.limit));
    Ok(observations)
}

pub(crate) async fn provider_account_request_buckets(
    snapshot: &mut analytics::AnalyticsSnapshot<'_>,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<HashMap<String, Vec<ProviderAccountRequestBucket>>> {
    let mut observed = snapshot.account_request_counts(range, account_ids).await?;
    let mut timelines = HashMap::with_capacity(account_ids.len());
    for account in account_ids {
        let mut buckets = Vec::with_capacity(ACCOUNT_USAGE_TIMELINE_HOURS as usize);
        for index in 0..ACCOUNT_USAGE_TIMELINE_HOURS {
            let start = range
                .start
                .checked_add_signed(TimeDelta::hours(index))
                .ok_or_else(|| invalid("account request timeline exceeds supported timestamps"))?;
            buckets.push(ProviderAccountRequestBucket {
                bucket_start: start,
                request_count: observed
                    .remove(&(account.clone(), index))
                    .unwrap_or_default(),
            });
        }
        timelines.insert(account.clone(), buckets);
    }
    Ok(timelines)
}
