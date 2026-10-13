//! 预测累计使用开始时间与完成时间的二维汇总，采样边界及待投影版本读取精确事实

use super::*;
use gateway_admin::model::provider_credentials::ProviderDocument;
use gateway_admin::model::quota_forecast_sampling::{
    MAX_FORECAST_HISTORY_POINTS, QuotaForecastHistory, QuotaForecastHistoryPoint,
    QuotaForecastUsage,
};
use gateway_core::account::OpaqueProviderData;
use sqlx::postgres::PgRow;

pub(super) const COLLECTION: &str = "account_completion";
const COMPLETION_SECONDS: i64 = 300;
const COUNTERS: [&str; 10] = [
    "request_count",
    "completed_request_count",
    "window_total_tokens",
    "input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "missing_token_count",
    "known_cost_count",
    "unavailable_cost_count",
    "excluded_request_count",
];

pub(super) fn dimensions(row: &PgRow) -> StoreResult<Vec<String>> {
    let completed: Option<DateTime<Utc>> = get(row, "completed_at")?;
    let terminal = get::<String>(row, "outcome")? != "running";
    Ok(vec![
        get::<Option<String>>(row, "provider_account_ref")?.unwrap_or_default(),
        completed
            .filter(|_| terminal)
            .map(|time| time.timestamp().div_euclid(COMPLETION_SECONDS).to_string())
            .unwrap_or_default(),
    ])
}

pub(super) fn state(row: &PgRow) -> StoreResult<MetricState> {
    let mut state = MetricState::default();
    state.increment("request_count", 1)?;
    state.add_cost("USD", "0")?;
    if get::<Option<DateTime<Utc>>>(row, "completed_at")?.is_none()
        || get::<String>(row, "outcome")? == "running"
    {
        return Ok(state);
    }
    if !get::<bool>(row, "is_completed_usage")? {
        state.increment("excluded_request_count", 1)?;
        return Ok(state);
    }
    state.increment("completed_request_count", 1)?;
    let input = optional_unsigned(row, "input_tokens")?;
    let output = optional_unsigned(row, "output_tokens")?;
    let total = optional_unsigned(row, "total_tokens")?;
    state.increment(
        "window_total_tokens",
        total.unwrap_or(
            input
                .unwrap_or_default()
                .checked_add(output.unwrap_or_default())
                .ok_or_else(|| invalid("forecast token overflow"))?,
        ),
    )?;
    state.increment("input_tokens", input.unwrap_or_default())?;
    state.increment("output_tokens", output.unwrap_or_default())?;
    state.increment(
        "cached_input_tokens",
        optional_unsigned(row, "cached_input_tokens")?.unwrap_or_default(),
    )?;
    state.increment(
        "missing_token_count",
        u64::from(total.is_none() && (input.is_none() || output.is_none())),
    )?;
    let source: String = get(row, "cost_source")?;
    let amount: Option<String> = get(row, "cost_amount_text")?;
    let known = matches!(source.as_str(), "calculated" | "provider_reported")
        && get::<Option<String>>(row, "cost_currency")?.as_deref() == Some("USD")
        && amount.is_some();
    state.increment("known_cost_count", u64::from(known))?;
    state.increment("unavailable_cost_count", u64::from(!known))?;
    if known {
        state.add_cost(
            "USD",
            amount
                .as_deref()
                .ok_or_else(|| invalid("missing forecast amount"))?,
        )?;
    }
    Ok(state)
}

impl AnalyticsSnapshot<'_> {
    pub(crate) async fn forecast_history(
        &mut self,
        account: &str,
        range: ObservabilityRange,
    ) -> StoreResult<QuotaForecastHistory> {
        let documents = sqlx::query(DOCUMENTS)
            .bind(account)
            .bind(range.start)
            .bind(range.end)
            .bind(MAX_FORECAST_HISTORY_POINTS as i32)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|error| postgres_unavailable("sample forecast documents", error))?;
        let mut cutoffs = documents
            .iter()
            .map(|row| get(row, "completed_at"))
            .collect::<StoreResult<Vec<DateTime<Utc>>>>()?;
        // 最后一个状态专门代表整个窗口，文档恰好完成于窗口末端时复用同一截止值
        if cutoffs.last() != Some(&range.end) {
            cutoffs.push(range.end);
        }
        let boundaries = cutoffs
            .iter()
            .map(|time| time.timestamp().div_euclid(COMPLETION_SECONDS))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut states = vec![MetricState::default(); cutoffs.len() + 1];
        if self.projection_covers(range) {
            let (_, quarters) = self.summary_buckets(range, false).await?;
            if !quarters.is_empty() {
                let sums = COUNTERS
                    .iter()
                    .map(|name| format!("'{name}',coalesce(sum((counts->>'{name}')::numeric),0)"))
                    .collect::<Vec<_>>()
                    .join(",");
                let statement = format!(
                    "with selected as (select counts,costs,case when dimensions[2]='' then cardinality($3::timestamptz[]) else width_bucket(to_timestamp((dimensions[2]::bigint+1)*{COMPLETION_SECONDS})-interval '1 microsecond',$3::timestamptz[]) end as slot
                     from request_analytics_15m where collection='{COLLECTION}' and dimensions[1]=$1 and bucket_start=any($2::timestamptz[]) and schema_version=$5 and histogram_version=$6
                     and (dimensions[2]='' or not(nullif(dimensions[2],'')::bigint=any($4::bigint[]))))
                     select slot,jsonb_build_object({sums}) as counts,jsonb_build_object('USD',coalesce(sum((costs->>'USD')::numeric),0)::text) as costs,'{{}}'::jsonb as distributions,null::timestamptz as last_used_at from selected group by slot"
                );
                let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
                    .bind(account)
                    .bind(&quarters)
                    .bind(&cutoffs)
                    .bind(&boundaries)
                    .bind(SCHEMA_VERSION)
                    .bind(HISTOGRAM_VERSION)
                    .fetch_all(&mut *self.tx)
                    .await
                    .map_err(|error| {
                        postgres_unavailable("read forecast completion summaries", error)
                    })?;
                merge_rows(&mut states, rows, false)?;
                // 只有跨过采样截止值的完成时间小桶需要回读，按完成时间索引逐段定位
                let rows = self
                    .forecast_facts(
                        account,
                        range,
                        &cutoffs,
                        FactSelection::CompletionEdges {
                            quarters: &quarters,
                            boundaries: &boundaries,
                        },
                    )
                    .await?;
                merge_rows(&mut states, rows, false)?;
            }
            for (start, end) in self.uncovered_spans(range).await? {
                let rows = self
                    .forecast_facts(
                        account,
                        ObservabilityRange { start, end },
                        &cutoffs,
                        FactSelection::Projected,
                    )
                    .await?;
                merge_rows(&mut states, rows, false)?;
            }
            if self.has_pending_requests().await? {
                let old = self
                    .forecast_facts(account, range, &cutoffs, FactSelection::Previous)
                    .await?;
                merge_rows(&mut states, old, true)?;
                let current = self
                    .forecast_facts(account, range, &cutoffs, FactSelection::Current)
                    .await?;
                merge_rows(&mut states, current, false)?;
            }
        } else {
            let rows = self
                .forecast_facts(account, range, &cutoffs, FactSelection::Source)
                .await?;
            merge_rows(&mut states, rows, false)?;
        }
        let pending_request_count = states
            .last()
            .map_or(0, |state| state.count("request_count"));
        let mut cumulative = MetricState::default();
        let mut usages = Vec::with_capacity(cutoffs.len());
        for state in states.iter().take(cutoffs.len()) {
            cumulative.merge(state)?;
            usages.push(usage(&cumulative)?);
        }
        let mut points = Vec::with_capacity(documents.len());
        for (row, usage) in documents.into_iter().zip(&usages) {
            let document: sqlx::types::Json<serde_json::Map<String, serde_json::Value>> =
                get(&row, "document")?;
            points.push(QuotaForecastHistoryPoint {
                started_at: get(&row, "started_at")?,
                completed_at: get(&row, "completed_at")?,
                usage: usage.clone(),
                provider_observation: ProviderDocument::new(OpaqueProviderData::new(document.0)),
            });
        }
        Ok(QuotaForecastHistory {
            points,
            usage: usage(&cumulative)?,
            pending_request_count,
        })
    }

    async fn forecast_facts(
        &mut self,
        account: &str,
        range: ObservabilityRange,
        cutoffs: &[DateTime<Utc>],
        selection: FactSelection<'_>,
    ) -> StoreResult<Vec<PgRow>> {
        let source = matches!(selection, FactSelection::Source | FactSelection::Current);
        let table = if source {
            "model_requests"
        } else {
            "request_analytics_facts"
        };
        let mut query = QueryBuilder::<Postgres>::new(
            "with matching as (select mr.*,case when mr.completed_at is not null and mr.outcome <> 'running' then width_bucket(mr.completed_at-interval '1 microsecond',",
        );
        query.push_bind(cutoffs).push("::timestamptz[]) else ").push_bind(cutoffs.len() as i32)
            .push(" end as slot,coalesce((").push(completed_usage_fact_predicate("mr"))
            .push("),false) and mr.completed_at is not null and mr.outcome <> 'running' as included from ");
        if let FactSelection::CompletionEdges { boundaries, .. } = selection {
            // 保留按完成时间小桶的参数化索引扫描，避免展开后扫描整段账号事实再逐桶匹配
            // OFFSET 0 不裁剪数据，只阻止此相关子查询被合并到外层连接
            query.push("unnest(").push_bind(boundaries).push(format!("::bigint[]) edge(bucket) cross join lateral (select * from {table} f where f.provider_account_ref="))
                .push_bind(account).push(format!(" and f.completed_at >= to_timestamp(edge.bucket*{COMPLETION_SECONDS}) and f.completed_at < to_timestamp((edge.bucket+1)*{COMPLETION_SECONDS}) and f.outcome <> 'running' offset 0) mr"));
        } else {
            query.push(format!("{table} mr"));
        }
        if matches!(selection, FactSelection::Previous | FactSelection::Current) {
            query.push(" join request_analytics_pending_requests p on p.request_id=mr.id");
        }
        query
            .push(" where mr.provider_account_ref=")
            .push_bind(account)
            .push(" and mr.started_at >= ")
            .push_bind(range.start)
            .push(" and mr.started_at < ")
            .push_bind(range.end);
        if let FactSelection::CompletionEdges { quarters, .. } = selection {
            query.push(" and date_bin(interval '15 minutes',mr.started_at,timestamptz '2000-01-01 UTC')=any(").push_bind(quarters).push("::timestamptz[])");
        }
        // 多个累计指标复用同一分类结果，避免展开后重复判断完成用量并携带宽事实排序
        // OFFSET 0 保留全部行，只让分类在聚合前计算一次
        query.push("), classified as (select *,coalesce(cost_source in ('calculated','provider_reported') and cost_amount is not null and cost_currency='USD',false) as known_cost from matching offset 0)
            select slot,jsonb_build_object(
                'request_count',count(*),
                'completed_request_count',count(*) filter(where included),
                'window_total_tokens',coalesce(sum(coalesce(total_tokens,coalesce(input_tokens,0)+coalesce(output_tokens,0))) filter(where included),0),
                'input_tokens',coalesce(sum(input_tokens) filter(where included),0),
                'output_tokens',coalesce(sum(output_tokens) filter(where included),0),
                'cached_input_tokens',coalesce(sum(cached_input_tokens) filter(where included),0),
                'missing_token_count',count(*) filter(where included and total_tokens is null and (input_tokens is null or output_tokens is null)),
                'known_cost_count',count(*) filter(where included and known_cost),
                'unavailable_cost_count',count(*) filter(where included and not known_cost),
                'excluded_request_count',count(*) filter(where not included and completed_at is not null and outcome <> 'running')) as counts,
                jsonb_build_object('USD',coalesce(sum(cost_amount) filter(where included and known_cost),0)::text) as costs,
                '{}'::jsonb as distributions,null::timestamptz as last_used_at from classified group by slot");
        query
            .build()
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|error| postgres_unavailable("read exact forecast boundaries", error))
    }
}

#[derive(Clone, Copy)]
enum FactSelection<'a> {
    Source,
    Projected,
    Previous,
    Current,
    CompletionEdges {
        quarters: &'a [DateTime<Utc>],
        boundaries: &'a [i64],
    },
}

fn merge_rows(states: &mut [MetricState], rows: Vec<PgRow>, subtract: bool) -> StoreResult<()> {
    for row in rows {
        let index = usize::try_from(get::<i32>(&row, "slot")?)
            .map_err(|_| invalid("negative forecast slot"))?;
        let target = states
            .get_mut(index)
            .ok_or_else(|| invalid("forecast slot out of range"))?;
        let state = MetricState::decode(&row)?;
        if subtract {
            incremental::subtract(target, &state, &MetricState::default())?;
        } else {
            target.merge(&state)?;
        }
    }
    Ok(())
}

fn usage(state: &MetricState) -> StoreResult<QuotaForecastUsage> {
    Ok(QuotaForecastUsage {
        request_count: state.count("completed_request_count"),
        tokens: state.count("window_total_tokens"),
        input_tokens: state.count("input_tokens"),
        output_tokens: state.count("output_tokens"),
        cached_input_tokens: state.count("cached_input_tokens"),
        missing_token_count: state.count("missing_token_count"),
        known_cost_count: state.count("known_cost_count"),
        unavailable_cost_count: state.count("unavailable_cost_count"),
        excluded_request_count: state.count("excluded_request_count"),
        usd: state
            .costs
            .get("USD")
            .map(|value| {
                value
                    .parse::<f64>()
                    .map_err(|_| invalid("invalid forecast dollars"))
            })
            .transpose()?
            .unwrap_or_default(),
    })
}

// 微秒边界向上取整，最后一槽包含观测时刻；文档采样与累计量补差共用读事务
const DOCUMENTS: &str = "select mr.started_at,mr.completed_at,mr.provider_observation_json as document
    from generate_series(0,$4-1) b(bucket) cross join lateral (
        select mr.started_at,mr.completed_at,mr.provider_observation_json
        from model_requests mr where mr.provider_account_ref=$1 and mr.started_at >= $2 and mr.started_at < $3
          and mr.completed_at >= $2 + ceil(b.bucket*greatest(1,extract(epoch from ($3::timestamptz-$2::timestamptz))/$4)*1000000)::bigint*interval '1 microsecond'
          and mr.completed_at < case when b.bucket=$4-1 then $3+interval '1 microsecond' else least($3+interval '1 microsecond',$2+ceil((b.bucket+1)*greatest(1,extract(epoch from ($3::timestamptz-$2::timestamptz))/$4)*1000000)::bigint*interval '1 microsecond') end
          and mr.outcome <> 'running' and mr.provider_observation_json is not null
        order by mr.completed_at desc,mr.id desc limit 1
    ) mr order by mr.completed_at";
