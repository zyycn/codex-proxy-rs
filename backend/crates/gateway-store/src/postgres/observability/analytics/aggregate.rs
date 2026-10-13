//! 在 PostgreSQL 内完成事实统计与汇总合并，结果规模只随输出分组增长

use super::*;

/// 输入 matching 提供 output_bucket、name 和完整事实列及 is_completed_usage
/// 精确分位数在整个结果组内计算，不把局部分位数合并成新的分位数
pub(super) fn push_fact_aggregation(
    query: &mut QueryBuilder<Postgres>,
    exact_percentiles: bool,
    read_costs: bool,
) {
    push_fact_aggregation_selected(query, exact_percentiles, read_costs, true, None, None);
}

pub(super) fn push_fact_aggregation_selected(
    query: &mut QueryBuilder<Postgres>,
    exact_percentiles: bool,
    read_costs: bool,
    read_distributions: bool,
    counter_names: Option<&[&str]>,
    distribution_names: Option<&[&str]>,
) {
    let counters = selected_counter_expressions(counter_names);
    query.push(", counted as (select output_bucket,name,max(started_at) filter(where is_completed_usage) as last_used_at,");
    push_counter_object(query, &counters);
    query.push(" as counts");
    if exact_percentiles && read_distributions {
        query.push(",");
        push_exact_distributions(query, distribution_names);
    }
    query.push(" from matching group by output_bucket,name)");
    if read_costs {
        query.push(", cost_values as (select output_bucket,name,currency,sum(amount)::text as amount from matching cross join lateral (values (cost_currency,cost_amount),('standard_usd',standard_cost_amount),('no_cache_usd',no_cache_cost_amount)) cost(currency,amount) where is_completed_usage and currency is not null and amount is not null group by output_bucket,name,currency), merged_costs as (select output_bucket,name,jsonb_object_agg(currency,amount) as costs from cost_values group by output_bucket,name)");
    }
    if !read_distributions {
        query.push(" select c.output_bucket as bucket_start,c.name,c.counts,c.last_used_at,'{}'::jsonb as distributions,'{}'::jsonb as exact_percentiles,");
        query.push(if read_costs {
            "coalesce(k.costs,'{}'::jsonb)"
        } else {
            "'{}'::jsonb"
        });
        query.push(" as costs from counted c");
        if read_costs {
            query.push(" left join merged_costs k using(output_bucket,name)");
        }
        return;
    }
    if exact_percentiles {
        query.push(" select c.output_bucket as bucket_start,c.name,c.counts,c.last_used_at,c.distributions,c.exact_percentiles,");
        query.push(if read_costs {
            "coalesce(k.costs,'{}'::jsonb)"
        } else {
            "'{}'::jsonb"
        });
        query.push(" as costs from counted c");
        if read_costs {
            query.push(" left join merged_costs k using(output_bucket,name)");
        }
        return;
    }
    query.push(", samples as (select output_bucket,name,metric,value from matching cross join lateral (values ");
    for (index, (name, expression)) in selected_sample_expressions(distribution_names)
        .iter()
        .enumerate()
    {
        if index > 0 {
            query.push(",");
        }
        query.push(format!("('{name}',{expression})"));
    }
    query.push(") sample(metric,value) where value is not null), sample_stats as (select output_bucket,name,metric,count(*) as count,sum(value) as sum,min(value) as minimum,max(value) as maximum,count(*) filter(where value=0) as zero");
    query.push(" from samples group by output_bucket,name,metric)");
    // 对数桶定义与 Histogram::add 相同，零值单独保留，不对零取对数
    query.push(", bin_counts as (select output_bucket,name,metric,floor(log(2::numeric,value::numeric)*16)::integer as bin,count(*) as count from samples where value>0 group by output_bucket,name,metric,bin), merged_bins as (select output_bucket,name,metric,jsonb_object_agg(bin,count) as bins from bin_counts group by output_bucket,name,metric)");
    query.push(", merged_distributions as (select s.output_bucket,s.name,jsonb_object_agg(s.metric,jsonb_build_object('count',s.count,'sum',s.sum,'minimum',s.minimum,'maximum',s.maximum,'zero',s.zero,'bins',");
    query.push("coalesce(b.bins,'{}'::jsonb)");
    query.push(")) as distributions");
    query.push(" from sample_stats s");
    query.push(" left join merged_bins b using(output_bucket,name,metric)");
    query.push(" group by s.output_bucket,s.name) select c.output_bucket as bucket_start,c.name,c.counts,c.last_used_at,");
    query.push(if read_costs {
        "coalesce(k.costs,'{}'::jsonb)"
    } else {
        "'{}'::jsonb"
    });
    query.push(" as costs,coalesce(d.distributions,'{}'::jsonb) as distributions,");
    query.push("'{}'::jsonb");
    query.push(" as exact_percentiles from counted c left join merged_distributions d using(output_bucket,name)");
    if read_costs {
        query.push(" left join merged_costs k using(output_bucket,name)");
    }
}

// 事实聚合与汇总读取共用指标清单，防止新增计数只在其中一条路径生效
fn counter_expressions() -> Vec<(String, String)> {
    let mut counters = vec![
        ("request_count".to_owned(), "count(*)".to_owned()),
        ("completed_request_count".to_owned(), "count(*) filter(where is_completed_usage)".to_owned()),
        ("caller_error_count".to_owned(), "count(*) filter(where client_status_code between 400 and 499)".to_owned()),
        ("attempt_count".to_owned(), "coalesce(sum(attempt_count),0)".to_owned()),
        ("retry_count".to_owned(), "coalesce(sum(greatest(attempt_count-1,0)),0)".to_owned()),
        ("retried_request_count".to_owned(), "count(*) filter(where attempt_count>1)".to_owned()),
        ("capacity_sample_count".to_owned(), "count(capacity_total_slots)".to_owned()),
        ("window_total_tokens".to_owned(), "coalesce(sum(coalesce(total_tokens,coalesce(input_tokens,0)::numeric+coalesce(output_tokens,0))) filter(where is_completed_usage),0)".to_owned()),
        ("cache_eligible_request_count".to_owned(), "count(*) filter(where is_completed_usage and input_tokens is not null)".to_owned()),
        ("cache_hit_request_count".to_owned(), "count(*) filter(where is_completed_usage and input_tokens is not null and cached_input_tokens>0)".to_owned()),
        ("image_request_count".to_owned(), "count(*) filter(where is_completed_usage and image_generation_succeeded)".to_owned()),
        ("image_request_failed_count".to_owned(), "count(*) filter(where is_completed_usage and not image_generation_succeeded)".to_owned()),
    ];
    for (outcome, name) in [
        ("succeeded", "success"),
        ("failed", "failure"),
        ("cancelled", "cancelled"),
        ("incomplete", "incomplete"),
    ] {
        counters.push((
            format!("{name}_count"),
            format!("count(*) filter(where outcome='{outcome}')"),
        ));
        let extra = if outcome == "failed" {
            "+coalesce(sum(ops_failure_count),0)"
        } else {
            ""
        };
        counters.push((
            format!("attempt_{name}_count"),
            format!("count(*) filter(where attempt_count>0 and outcome='{outcome}'){extra}"),
        ));
    }
    for (name, predicate, source) in [
        (
            "rate_limited_count",
            "(error_kind in ('rate_limited','quota_exhausted') or coalesce(upstream_status_code,client_status_code)=429)",
            "ops_rate_limited_count",
        ),
        (
            "auth_failure_count",
            "(error_kind in ('authentication','authorization','invalid_credential') or coalesce(upstream_status_code,client_status_code) in (401,403))",
            "ops_auth_failure_count",
        ),
        (
            "provider_5xx_count",
            "coalesce(upstream_status_code,client_status_code) between 500 and 599",
            "ops_provider_5xx_count",
        ),
    ] {
        counters.push((name.to_owned(), format!("count(*) filter(where attempt_count>0 and outcome='failed' and {predicate})+coalesce(sum({source}),0)")));
    }
    for name in metrics::TOKEN_METRICS {
        counters.push((
            name.to_owned(),
            format!("coalesce(sum({name}) filter(where is_completed_usage),0)"),
        ));
        counters.push((
            format!("{name}_known_count"),
            format!("count({name}) filter(where is_completed_usage)"),
        ));
    }
    for name in ["latency_ms", "first_token_ms"] {
        counters.push((
            format!("{name}_sum"),
            format!("coalesce(sum({name}) filter(where is_completed_usage),0)"),
        ));
    }
    for (name, column) in [
        ("standard_usd", "standard_cost_amount"),
        ("no_cache_usd", "no_cache_cost_amount"),
    ] {
        counters.push((
            format!("{name}_known_count"),
            format!("count({column}) filter(where is_completed_usage)"),
        ));
    }
    for source in ["provider_reported", "calculated", "unavailable"] {
        counters.push((
            format!("{source}_count"),
            format!("count(*) filter(where is_completed_usage and cost_source='{source}')"),
        ));
    }
    counters
}

fn selected_counter_expressions(names: Option<&[&str]>) -> Vec<(String, String)> {
    let mut counters = counter_expressions();
    if let Some(names) = names {
        counters.retain(|(name, _)| names.contains(&name.as_str()));
    }
    counters
}

fn push_exact_distributions(query: &mut QueryBuilder<Postgres>, names: Option<&[&str]>) {
    let samples = selected_sample_expressions(names);
    // 独立 ordered-set 聚合只排序实际样本，避免把每条事实展开为六条记录再排序
    query.push("jsonb_build_object(");
    for (index, (name, expression)) in samples.iter().enumerate() {
        if index > 0 {
            query.push(",");
        }
        query.push(format!("'{name}',jsonb_build_object('count',count({expression}),'sum',coalesce(sum({expression}),0),'minimum',min({expression}),'maximum',max({expression}),'zero',count(*) filter(where ({expression})=0),'bins','{{}}'::jsonb)"));
    }
    query.push(") as distributions,jsonb_build_object(");
    for (index, (name, expression)) in samples.iter().enumerate() {
        if index > 0 {
            query.push(",");
        }
        query.push(format!("'{name}',coalesce(to_jsonb(percentile_cont(array[0.1,0.5,0.9,0.95,0.99]::double precision[]) within group(order by ({expression}))),'[]'::jsonb)"));
    }
    query.push(") as exact_percentiles");
}

fn sample_expressions() -> [(&'static str, &'static str); 6] {
    [
        (
            "admission_decision_ms",
            "admission_decision_ms::double precision",
        ),
        (
            "account_selection_wait_ms",
            "account_selection_wait_ms::double precision",
        ),
        (
            "capacity",
            "case when capacity_total_slots>0 then capacity_used_slots::double precision/capacity_total_slots::double precision*10000.0 end",
        ),
        (
            "latency_ms",
            "case when is_completed_usage then latency_ms::double precision end",
        ),
        (
            "first_token_ms",
            "case when is_completed_usage then first_token_ms::double precision end",
        ),
        (
            "throughput",
            "case when is_completed_usage and output_tokens>0 and upstream_response_ms>0 then output_tokens::double precision*1000.0/upstream_response_ms::double precision end",
        ),
    ]
}

fn selected_sample_expressions(names: Option<&[&str]>) -> Vec<(&'static str, &'static str)> {
    sample_expressions()
        .into_iter()
        .filter(|(name, _)| names.is_none_or(|names| names.contains(name)))
        .collect()
}

fn push_counter_object(query: &mut QueryBuilder<Postgres>, counters: &[(String, String)]) {
    // PostgreSQL 单个函数调用最多接收一百个参数，分块构造不限制指标数量
    for (index, chunk) in counters.chunks(40).enumerate() {
        if index > 0 {
            query.push(" || ");
        }
        query.push("jsonb_build_object(");
        for (position, (name, expression)) in chunk.iter().enumerate() {
            if position > 0 {
                query.push(",");
            }
            query.push(format!("'{name}',{expression}"));
        }
        query.push(")");
    }
}

/// 输入 matching 的行来自互斥时间桶，合并后每个最终输出组只返回一行
pub(super) fn push_summary_aggregation(
    query: &mut QueryBuilder<Postgres>,
    distributions: bool,
    costs: bool,
    counter_names: Option<&[&str]>,
    distribution_names: Option<&[&str]>,
) {
    // 每个源汇总只展开一次固定指标，避免按指标键放大行数再排序
    let counters = selected_counter_expressions(counter_names)
        .into_iter()
        .map(|(name, _)| {
            let expression = format!("coalesce(sum(metric.{name}),0)");
            (name, expression)
        })
        .collect::<Vec<_>>();
    query.push(", grouped as (select output_bucket,name,max(last_used_at) as last_used_at,");
    push_counter_object(query, &counters);
    query.push(" as counts from matching cross join lateral jsonb_to_record(counts) as metric(");
    query.push(
        counters
            .iter()
            .map(|(name, _)| format!("{name} numeric"))
            .collect::<Vec<_>>()
            .join(","),
    );
    query.push(") group by output_bucket,name)");
    if costs {
        query.push(", cost_values as (select output_bucket,name,c.key,sum(c.value::numeric)::text as value from matching cross join lateral jsonb_each_text(costs) c group by output_bucket,name,c.key), merged_costs as (select output_bucket,name,jsonb_object_agg(key,value) as costs from cost_values group by output_bucket,name)");
    }
    if distributions {
        // 先按最终输出组归并分布，再在每组内合并直方图键，避免全局展开后大排序
        query.push(", samples as (select output_bucket,name,d.key as metric,d.value from matching cross join lateral jsonb_each(distributions) d");
        if let Some(names) = distribution_names {
            query
                .push(" where d.key=any(")
                .push_bind(
                    names
                        .iter()
                        .map(|name| (*name).to_owned())
                        .collect::<Vec<_>>(),
                )
                .push("::text[])");
        }
        query.push("), sample_stats as (select output_bucket,name,metric,sum((value->>'count')::numeric) as count,sum((value->>'sum')::double precision) as sum,min((value->>'minimum')::double precision) as minimum,max((value->>'maximum')::double precision) as maximum,sum((value->>'zero')::numeric) as zero,array_agg(value->'bins') as bins from samples group by output_bucket,name,metric), merged_distributions as (select s.output_bucket,s.name,jsonb_object_agg(s.metric,jsonb_build_object('count',s.count,'sum',s.sum,'minimum',s.minimum,'maximum',s.maximum,'zero',s.zero,'bins',coalesce(b.bins,'{}'::jsonb))) as distributions from sample_stats s cross join lateral(select jsonb_object_agg(key,count) as bins from(select b.key,sum(b.value::numeric) as count from unnest(s.bins) input(value) cross join lateral jsonb_each_text(input.value) b group by b.key) merged)b group by s.output_bucket,s.name)");
    }
    query.push(" select g.output_bucket as bucket_start,g.name,g.last_used_at,g.counts,");
    query.push(if costs {
        "coalesce(k.costs,'{}'::jsonb)"
    } else {
        "'{}'::jsonb"
    });
    query.push(" as costs,");
    query.push(if distributions {
        "coalesce(d.distributions,'{}'::jsonb)"
    } else {
        "'{}'::jsonb"
    });
    query.push(" as distributions from grouped g");
    if costs {
        query.push(" left join merged_costs k using(output_bucket,name)");
    }
    if distributions {
        query.push(" left join merged_distributions d using(output_bucket,name)");
    }
}

pub(super) fn push_output_bucket(
    query: &mut QueryBuilder<Postgres>,
    column: &str,
    range: ObservabilityRange,
    granularity: Option<ObservationGranularity>,
    boundaries: Option<&[DateTime<Utc>]>,
) {
    match granularity {
        None => {
            query.push_bind(range.start).push("::timestamptz");
        }
        Some(ObservationGranularity::Day) => {
            // 使用有序阈值二分定位，保留部署时区及夏令时日界，不逐行扫描所有日界
            let boundaries = boundaries.unwrap_or_default();
            query
                .push("(")
                .push_bind(boundaries.to_vec())
                .push(format!("::timestamptz[])[width_bucket({column},"))
                .push_bind(boundaries.to_vec())
                .push("::timestamptz[])]");
        }
        Some(granularity) => {
            query
                .push("date_bin(make_interval(secs => ")
                .push_bind(granularity.seconds() as f64)
                .push(format!("),{column},timestamptz '2000-01-01 UTC')"));
        }
    }
}

pub(super) fn fact_dimension_sql(dimension: Option<DiagnosticDimension>) -> &'static str {
    match dimension {
        None => "''::text",
        Some(DiagnosticDimension::Provider) => "coalesce(nullif(mr.provider_kind,''),'unrouted')",
        Some(DiagnosticDimension::Model) => {
            "coalesce(nullif(mr.upstream_model_id,''),mr.requested_model_id,'')"
        }
        Some(DiagnosticDimension::Account) => {
            "coalesce(nullif(mr.provider_account_ref,''),'unrouted')"
        }
        Some(DiagnosticDimension::ApiKey) => "coalesce(mr.client_api_key_ref,'')",
        Some(DiagnosticDimension::Transport) => {
            "coalesce(nullif(mr.upstream_transport,''),nullif(mr.client_transport,''),'unknown')"
        }
        Some(DiagnosticDimension::Failure) => "coalesce(mr.error_kind,'')",
        Some(DiagnosticDimension::Status) => {
            "coalesce(mr.upstream_status_code::text,mr.client_status_code::text,'')"
        }
    }
}

pub(super) fn summary_dimension_sql(
    collection: &str,
    dimension: Option<DiagnosticDimension>,
) -> String {
    let offset = usize::from(matches!(collection, "account_model" | "key_model"));
    match dimension {
        None => "''::text".to_owned(),
        Some(DiagnosticDimension::Provider) => {
            format!("coalesce(nullif(dimensions[{}],''),'unrouted')", 1 + offset)
        }
        Some(DiagnosticDimension::Model) => format!(
            "coalesce(nullif(dimensions[{}],''),dimensions[{}],'')",
            3 + offset,
            2 + offset
        ),
        Some(DiagnosticDimension::Account) => {
            "coalesce(nullif(dimensions[1],''),'unrouted')".to_owned()
        }
        Some(DiagnosticDimension::Transport) => {
            "coalesce(nullif(dimensions[2],''),nullif(dimensions[1],''),'unknown')".to_owned()
        }
        Some(DiagnosticDimension::Status) => {
            "coalesce(nullif(dimensions[2],''),dimensions[1],'')".to_owned()
        }
        Some(DiagnosticDimension::ApiKey | DiagnosticDimension::Failure) => {
            "coalesce(dimensions[1],'')".to_owned()
        }
    }
}
