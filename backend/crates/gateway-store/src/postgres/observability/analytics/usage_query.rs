//! 从互斥统计桶和窄事实生成完整分组游标，保留原生与外部贡献覆盖

use super::*;
use gateway_admin::model::usage_data::*;
use sqlx::postgres::PgRow;

const REQUEST_METRICS: &[(UsageMetric, &str)] = &[
    (UsageMetric::RequestCount, "request_count"),
    (UsageMetric::SuccessCount, "success_count"),
    (UsageMetric::FailureCount, "failure_count"),
    (UsageMetric::CancelledCount, "cancelled_count"),
    (UsageMetric::IncompleteCount, "incomplete_count"),
    (
        UsageMetric::CompletedRequestCount,
        "completed_request_count",
    ),
];
const TOKEN_FIELDS: &[(UsageMetric, &str)] = &[
    (UsageMetric::InputTokens, "input_tokens"),
    (UsageMetric::OutputTokens, "output_tokens"),
    (UsageMetric::CachedInputTokens, "cached_input_tokens"),
    (
        UsageMetric::CacheWriteInputTokens,
        "cache_write_input_tokens",
    ),
    (
        UsageMetric::ReasoningOutputTokens,
        "reasoning_output_tokens",
    ),
    (UsageMetric::ImageInputTokens, "image_input_tokens"),
    (UsageMetric::ImageOutputTokens, "image_output_tokens"),
    (UsageMetric::TotalTokens, "total_tokens"),
];

pub(crate) async fn declare_usage_statistics(
    snapshot: &mut AnalyticsSnapshot<'_>,
    request: &QueryUsageStatistics,
) -> StoreResult<()> {
    let filter = &request.filter;
    let range =
        ObservabilityRange::new(time(filter.records.start_ms)?, time(filter.records.end_ms)?)?;
    snapshot.require_visible(Some(range))?;
    let collection = collection(request);
    let mut query = QueryBuilder::<Postgres>::new(
        "declare usage_read no scroll cursor for with native_parts as (select 'null'::text as name,'{}'::jsonb as counts,'{}'::jsonb as costs",
    );
    if let Some(collection) = collection {
        let (hours, quarters) = snapshot.summary_buckets(range, true).await?;
        for (table, seconds, buckets) in [
            ("request_analytics_1h", 3600, hours),
            ("request_analytics_15m", 900, quarters),
        ] {
            if buckets.is_empty() {
                continue;
            }
            let (key, model, provider, account) = summary_columns(collection);
            let group = group_expression(request.group_by, key, model, "null::text", false);
            query.push(format!(" union all select names.name,a.counts,a.costs from {table} a cross join lateral (values ({group}),('null')) names(name) where a.collection="))
                .push_bind(collection).push(" and a.schema_version=").push_bind(SCHEMA_VERSION)
                .push(" and a.histogram_version=").push_bind(HISTOGRAM_VERSION);
            query::push_summary_buckets(&mut query, &buckets, seconds);
            for (column, value) in [
                (key, &filter.records.client_key_ref),
                (model, &filter.records.model),
                (provider, &filter.records.provider),
                (account, &filter.records.account_ref),
            ] {
                if let Some(value) = value {
                    query
                        .push(format!(" and {column}="))
                        .push_bind(value.clone());
                }
            }
        }
    }
    let spans = if collection.is_some() {
        snapshot.uncovered_spans(range).await?
    } else {
        vec![(range.start, range.end)]
    };
    if !spans.is_empty() {
        query.push(" union all select name,counts,costs from (with matching as not materialized (select mr.*,coalesce((");
        query.push(completed_usage_fact_predicate("mr"));
        query
            .push("),false) as is_completed_usage,")
            .push_bind(range.start)
            .push("::timestamptz as output_bucket,names.name from request_analytics_facts mr");
        push_fact_groups(&mut query, request.group_by);
        query.push(" where (");
        for (index, (start, end)) in spans.into_iter().enumerate() {
            if index > 0 {
                query.push(" or ");
            }
            query
                .push("(mr.started_at >= ")
                .push_bind(start)
                .push(" and mr.started_at < ")
                .push_bind(end)
                .push(")");
        }
        query.push(")");
        push_unrecovered_request_filter(&mut query, "mr");
        super::super::super::usage_facts::push_usage_read_filter(&mut query, filter);
        query.push(")");
        let counters = counter_names();
        let names = counters.iter().map(String::as_str).collect::<Vec<_>>();
        aggregate::push_fact_aggregation_selected(
            &mut query,
            false,
            true,
            DistributionRead::None,
            Some(&names),
            None,
        );
        query.push(") facts");
    }
    query.push("), inputs as (select name,'native'::text as origin,counts,costs from native_parts");
    if filter.routing_group_ref.is_none() {
        let group = group_expression(
            request.group_by,
            "nullif(a.dimensions[4],'')",
            "nullif(a.dimensions[2],'')",
            "null::text",
            true,
        );
        query.push(format!(" union all select names.name,'contribution',a.counts,a.costs from request_analytics_1h a cross join lateral (values ({group}),('null')) names(name) where a.collection='usage_contribution' and a.bucket_start >= "))
            .push_bind(range.start).push(" and a.bucket_start < ").push_bind(range.end);
        for (index, value) in [
            (1, &filter.records.provider),
            (2, &filter.records.model),
            (3, &filter.records.account_ref),
            (4, &filter.records.client_key_ref),
        ] {
            if let Some(value) = value {
                query
                    .push(format!(" and a.dimensions[{index}]="))
                    .push_bind(value.clone());
            }
        }
    }
    // 分组和全局总计在同一查询中生成；每条输入只贡献一次全局计数
    // 汇总在数据库完成，应用只接收当前批次，金额按原币种保留
    query.push(r#"), groups as (
        select name, count(*) filter(where origin='contribution')::bigint as contribution_count
        from inputs group by name
    ), metric_values as (
        select name, origin, m.key, sum(m.value::numeric)::text as value, count(*)::bigint as samples
        from inputs cross join lateral jsonb_each_text(counts) m
        group by name, origin, m.key
    ), metric_groups as (
        select name,
            jsonb_object_agg(key,value) filter(where origin='native') as native_counts,
            jsonb_object_agg(key,value) filter(where origin='contribution') as contribution_counts,
            jsonb_object_agg(key,samples) filter(where origin='contribution') as contribution_samples
        from metric_values group by name
    ), cost_values as (
        select name, origin, c.key, trim_scale(sum(c.value::numeric))::text as value
        from inputs cross join lateral jsonb_each_text(costs) c
        where c.key not in ('standard_usd','no_cache_usd')
        group by name, origin, c.key
    ), cost_groups as (
        select name,
            jsonb_object_agg(key,value) filter(where origin='native') as native_costs,
            jsonb_object_agg(key,value) filter(where origin='contribution') as contribution_costs
        from cost_values group by name
    ) select g.name, g.contribution_count, count(*) over()-1 as total_groups,
        coalesce(m.native_counts,'{}'::jsonb) as native_counts,
        coalesce(m.contribution_counts,'{}'::jsonb) as contribution_counts,
        coalesce(m.contribution_samples,'{}'::jsonb) as contribution_samples,
        coalesce(c.native_costs,'{}'::jsonb) as native_costs,
        coalesce(c.contribution_costs,'{}'::jsonb) as contribution_costs
    from groups g
    left join metric_groups m using(name)
    left join cost_groups c using(name)
    order by (g.name<>'null'), g.name collate "C""#);
    query
        .build()
        .execute(&mut *snapshot.tx)
        .await
        .map_err(|error| postgres_unavailable("open usage statistics cursor", error))?;
    Ok(())
}

fn collection(query: &QueryUsageStatistics) -> Option<&'static str> {
    let filter = &query.filter;
    if query.group_by == UsageGrouping::RoutingGroup || filter.routing_group_ref.is_some() {
        return None;
    }
    let key = matches!(query.group_by, UsageGrouping::Key | UsageGrouping::KeyModel)
        || filter.records.client_key_ref.is_some();
    if filter.records.account_ref.is_some() {
        return (!key).then_some("account_model");
    }
    if key {
        if query.group_by == UsageGrouping::Key
            && filter.records.model.is_none()
            && filter.records.provider.is_none()
        {
            Some("key")
        } else {
            Some("key_model")
        }
    } else {
        Some("provider_model")
    }
}

fn summary_columns(collection: &str) -> (&'static str, &'static str, &'static str, &'static str) {
    match collection {
        "key" => (
            "nullif(a.dimensions[1],'')",
            "null::text",
            "null::text",
            "null::text",
        ),
        "key_model" => (
            "nullif(a.dimensions[1],'')",
            "nullif(a.dimensions[4],'')",
            "nullif(a.dimensions[2],'')",
            "null::text",
        ),
        "account_model" => (
            "null::text",
            "nullif(a.dimensions[4],'')",
            "nullif(a.dimensions[2],'')",
            "nullif(a.dimensions[1],'')",
        ),
        _ => (
            "null::text",
            "nullif(a.dimensions[3],'')",
            "nullif(a.dimensions[1],'')",
            "null::text",
        ),
    }
}

fn group_expression(
    grouping: UsageGrouping,
    key: &str,
    model: &str,
    group: &str,
    contribution: bool,
) -> String {
    let (key, model, group) = match grouping {
        UsageGrouping::Model => ("null::text", model, "null::text"),
        UsageGrouping::Key => (key, "null::text", "null::text"),
        UsageGrouping::KeyModel => (key, model, "null::text"),
        UsageGrouping::RoutingGroup => ("null::text", "null::text", group),
    };
    let unassigned = if contribution {
        match grouping {
            UsageGrouping::Model => format!("({model}) is null"),
            UsageGrouping::Key => format!("({key}) is null"),
            UsageGrouping::KeyModel => format!("({key}) is null or ({model}) is null"),
            UsageGrouping::RoutingGroup => "true".to_owned(),
        }
    } else {
        "false".to_owned()
    };
    format!(
        "jsonb_build_object('client_key_ref',{key},'model',{model},'routing_group_ref',{group},'unassigned_contribution',({unassigned}))::text"
    )
}

fn push_fact_groups(query: &mut QueryBuilder<Postgres>, grouping: UsageGrouping) {
    let group = group_expression(
        grouping,
        "mr.client_api_key_ref",
        "mr.upstream_model_id",
        "r.group_ref",
        false,
    );
    if grouping == UsageGrouping::RoutingGroup {
        query.push(format!(" cross join lateral (select {group} as name from unnest(case when cardinality(mr.routing_group_refs)>0 then mr.routing_group_refs else array[null::text] end) r(group_ref) union all select 'null') names"));
    } else {
        query.push(format!(
            " cross join lateral (values ({group}),('null')) names(name)"
        ));
    }
}

fn counter_names() -> Vec<String> {
    REQUEST_METRICS
        .iter()
        .chain(TOKEN_FIELDS)
        .map(|(_, name)| (*name).to_owned())
        .chain(
            TOKEN_FIELDS
                .iter()
                .map(|(_, name)| format!("{name}_known_count")),
        )
        .chain(
            [
                "provider_reported_count",
                "calculated_count",
                "unavailable_count",
            ]
            .map(str::to_owned),
        )
        .collect()
}

fn time(milliseconds: i64) -> StoreResult<DateTime<Utc>> {
    DateTime::from_timestamp_millis(milliseconds).ok_or_else(|| invalid("invalid usage query time"))
}

pub(crate) fn decode_usage_statistics(row: PgRow) -> StoreResult<UsageStatisticsRow> {
    let name: String = get(&row, "name")?;
    let native_counts: BTreeMap<String, String> =
        get::<sqlx::types::Json<_>>(&row, "native_counts")?.0;
    let contribution_counts: BTreeMap<String, String> =
        get::<sqlx::types::Json<_>>(&row, "contribution_counts")?.0;
    let contribution_samples: BTreeMap<String, u64> =
        get::<sqlx::types::Json<_>>(&row, "contribution_samples")?.0;
    let native_costs: BTreeMap<String, String> =
        get::<sqlx::types::Json<_>>(&row, "native_costs")?.0;
    let contribution_costs: BTreeMap<String, String> =
        get::<sqlx::types::Json<_>>(&row, "contribution_costs")?.0;
    let mut state = MetricState::default();
    for (key, value) in native_counts {
        state.counts.insert(
            key,
            value
                .parse()
                .map_err(|_| invalid("native usage count exceeds result range"))?,
        );
    }
    let native_request_count = UsageCount(state.count("request_count"));
    let cost_coverage = UsageStatisticsCostCoverage {
        provider_reported: UsageCount(state.count("provider_reported_count")),
        calculated: UsageCount(state.count("calculated_count")),
        unavailable: UsageCount(state.count("unavailable_count")),
    };
    let native_token_samples = TOKEN_FIELDS
        .iter()
        .map(|(metric, name)| {
            (
                *metric,
                UsageCount(state.count(&format!("{name}_known_count"))),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (name, value) in contribution_counts {
        contributions::apply_count(
            &mut state,
            &name,
            value
                .parse()
                .map_err(|_| invalid("aggregate counter exceeds arithmetic range"))?,
        )?;
    }
    state.costs.clone_from(&native_costs);
    for (currency, amount) in &contribution_costs {
        contributions::apply_cost(&mut state, currency, amount)?;
    }
    let mut values = REQUEST_METRICS
        .iter()
        .map(|(metric, name)| (*metric, UsageCount(state.count(name))))
        .collect::<BTreeMap<_, _>>();
    let mut contribution_token_samples = BTreeMap::new();
    for (metric, name) in TOKEN_FIELDS {
        let samples = contribution_samples.get(*name).copied().unwrap_or_default();
        contribution_token_samples.insert(*metric, UsageCount(samples));
        if native_token_samples[metric].get() > 0 || samples > 0 {
            values.insert(*metric, UsageCount(state.count(name)));
        }
    }
    let costs = state
        .costs()?
        .into_iter()
        .map(|cost| UsageStatisticsCost {
            native_amount: native_costs.get(&cost.currency).cloned(),
            contribution_amount: contribution_costs.get(&cost.currency).cloned(),
            currency: cost.currency,
            amount: cost.amount.as_str().to_owned(),
        })
        .collect();
    Ok(UsageStatisticsRow {
        group: serde_json::from_str(&name).map_err(|_| invalid("invalid usage group identity"))?,
        metrics: UsageStatisticsMetrics {
            values,
            native_request_count,
            contribution_count: UsageCount(
                u64::try_from(get::<i64>(&row, "contribution_count")?)
                    .map_err(|_| invalid("negative contribution count"))?,
            ),
            native_token_samples,
            contribution_token_samples,
            costs,
            cost_coverage,
        },
    })
}
