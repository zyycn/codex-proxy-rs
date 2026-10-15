//! 将调用方维护的聚合贡献合入原生统计，保留查询预算与数值边界

use super::super::super::usage_data::AGGREGATE_COLLECTION;
use super::query::ReadSelection;
use super::*;
use futures::TryStreamExt;

pub(super) struct ContributionQuery<'a> {
    pub range: ObservabilityRange,
    pub filter: &'a UsageRecordFilter,
    pub dimension: Option<DiagnosticDimension>,
    pub granularity: Option<ObservationGranularity>,
    pub timezone: gateway_core::time::DeploymentTimeZone,
    pub account_models: Option<bool>,
    pub selection: ReadSelection<'a>,
}

impl AnalyticsSnapshot<'_> {
    pub(super) async fn merge_contributions(
        &mut self,
        input: ContributionQuery<'_>,
        states: &mut BTreeMap<(DateTime<Utc>, String), MetricState>,
    ) -> StoreResult<()> {
        let ContributionQuery {
            range,
            filter,
            dimension,
            granularity,
            timezone,
            account_models,
            selection,
        } = input;
        // 聚合未提供请求级身份或状态维度，不能用猜测的明细值匹配这些筛选
        if filter.request_id.is_some()
            || filter.response_id.is_some()
            || filter.upstream_request_id.is_some()
            || filter.attempt_index.is_some()
            || filter.status_code.is_some()
            || filter.outcome.is_some()
            || filter.search.is_some()
            || matches!(
                dimension,
                Some(DiagnosticDimension::Failure | DiagnosticDimension::Status)
            )
        {
            return Ok(());
        }
        let name = if let Some(by_model) = account_models {
            if by_model {
                "jsonb_build_array(dimensions[3],dimensions[2])::text"
            } else {
                "jsonb_build_array(dimensions[3],'')::text"
            }
        } else {
            match dimension {
                None => "''::text",
                Some(DiagnosticDimension::Provider) => {
                    "coalesce(nullif(dimensions[1],''),'unrouted')"
                }
                Some(DiagnosticDimension::Model) => "dimensions[2]",
                Some(DiagnosticDimension::Account) => {
                    "coalesce(nullif(dimensions[3],''),'unrouted')"
                }
                Some(DiagnosticDimension::ApiKey) => "dimensions[4]",
                Some(DiagnosticDimension::Transport) => {
                    "coalesce(nullif(dimensions[6],''),'unknown')"
                }
                _ => return Ok(()),
            }
        };
        let boundaries = if granularity == Some(ObservationGranularity::Day) {
            Some(calendar_buckets(range, timezone)?)
        } else {
            None
        };
        let mut q = QueryBuilder::<Postgres>::new("with matching as materialized (select ");
        aggregate::push_output_bucket(
            &mut q,
            "bucket_start",
            range,
            granularity,
            boundaries.as_deref(),
        );
        q.push(format!(" as output_bucket,{name} as name,counts,costs from request_analytics_1h where collection=")).push_bind(AGGREGATE_COLLECTION).push(" and bucket_start >= ").push_bind(range.start).push(" and bucket_start < ").push_bind(range.end);
        for (index, value) in [
            (1, &filter.provider_kind),
            (2, &filter.model),
            (3, &filter.provider_account_ref),
            (4, &filter.client_api_key_ref),
            (5, &filter.operation),
            (6, &filter.transport),
        ] {
            if let Some(value) = value {
                q.push(format!(" and dimensions[{index}]="))
                    .push_bind(value);
            }
        }
        if let Some(keys) = selection.keys {
            let key = if account_models.is_some() {
                "dimensions[3]"
            } else {
                name
            };
            q.push(format!(" and ({key})=any("))
                .push_bind(keys)
                .push(")");
        }
        let dimension_index = match dimension {
            Some(DiagnosticDimension::Provider) => Some(1),
            Some(DiagnosticDimension::Model) => Some(2),
            Some(DiagnosticDimension::Account) => Some(3),
            Some(DiagnosticDimension::ApiKey) => Some(4),
            Some(DiagnosticDimension::Transport) => Some(6),
            _ => None,
        };
        if let Some(index) = dimension_index {
            // 缺少归属维度的贡献只进入总量，不分配给虚构的未知分组
            q.push(format!(" and dimensions[{index}]<>''"));
        }
        q.push("), entries as (select output_bucket,name,'count' as kind,c.key,c.value::numeric as value from matching cross join lateral jsonb_each_text(counts) c");
        if let Some(counters) = selection.counters {
            q.push(" where c.key=any(").push_bind(counters).push(")");
        }
        if selection.costs {
            q.push(" union all select output_bucket,name,'cost',c.key,c.value::numeric from matching cross join lateral jsonb_each_text(costs) c");
        }
        q.push("), totals as (select output_bucket,name,kind,key,sum(value)::text as value from entries group by output_bucket,name,kind,key) select output_bucket,name,jsonb_object_agg(key,value) filter(where kind='count') as counts,jsonb_object_agg(key,value) filter(where kind='cost') as costs from totals group by output_bucket,name");
        let mut rows = q.build().fetch(&mut *self.tx);
        while let Some(row) = rows
            .try_next()
            .await
            .map_err(|e| postgres_unavailable("read maintained aggregate contributions", e))?
        {
            let mut name: String = get(&row, "name")?;
            if account_models.is_some() {
                let key: (String, String) = serde_json::from_str(&name)
                    .map_err(|_| invalid("invalid aggregate account key"))?;
                name = serde_json::to_string(&key)
                    .map_err(|_| invalid("encode aggregate account key"))?;
            }
            let key = (get(&row, "output_bucket")?, name);
            let state = states.entry(key).or_default();
            let counts =
                get::<Option<sqlx::types::Json<BTreeMap<String, String>>>>(&row, "counts")?
                    .map(|v| v.0)
                    .unwrap_or_default();
            for (name, value) in &counts {
                let delta = value
                    .parse::<i128>()
                    .map_err(|_| invalid("aggregate counter exceeds arithmetic range"))?;
                apply_count(state, name, delta)?;
                if metrics::TOKEN_METRICS.contains(&name.as_str()) {
                    // 只标记指标已知，不虚构请求明细、延迟样本或费用覆盖率
                    state.increment(&format!("{name}_known_count"), 1)?;
                }
            }
            if selection.counters.is_none() {
                let total = counts
                    .get("total_tokens")
                    .map(|v| v.parse::<i128>())
                    .transpose()
                    .map_err(|_| invalid("aggregate token overflow"))?;
                let token_delta = match total {
                    Some(total) => total,
                    None => {
                        let input = counts
                            .get("input_tokens")
                            .map(|v| v.parse::<i128>())
                            .transpose()
                            .map_err(|_| invalid("aggregate input overflow"))?
                            .unwrap_or_default();
                        let output = counts
                            .get("output_tokens")
                            .map(|v| v.parse::<i128>())
                            .transpose()
                            .map_err(|_| invalid("aggregate output overflow"))?
                            .unwrap_or_default();
                        input
                            .checked_add(output)
                            .ok_or_else(|| invalid("aggregate token overflow"))?
                    }
                };
                apply_count(state, "window_total_tokens", token_delta)?;
            }
            let costs = get::<Option<sqlx::types::Json<BTreeMap<String, String>>>>(&row, "costs")?
                .map(|v| v.0)
                .unwrap_or_default();
            for (currency, value) in costs {
                let current = state
                    .costs
                    .get(&currency)
                    .map(String::as_str)
                    .unwrap_or("0");
                let current = current
                    .parse::<gateway_core::metering::Decimal>()
                    .map_err(|_| invalid("native cost exceeds decimal range"))?;
                let magnitude = value
                    .strip_prefix('-')
                    .unwrap_or(&value)
                    .parse::<gateway_core::metering::Decimal>()
                    .map_err(|_| invalid("aggregate cost exceeds decimal range"))?;
                let scaled = if value.starts_with('-') {
                    current.scaled().checked_sub(magnitude.scaled())
                } else {
                    current.scaled().checked_add(magnitude.scaled())
                }
                .ok_or_else(|| invalid("aggregate cost is outside native result range"))?;
                let amount = gateway_core::metering::Decimal::from_scaled(scaled)
                    .map_err(|_| invalid("aggregate cost exceeds native result range"))?;
                state.costs.insert(currency, amount.canonical());
            }
        }
        Ok(())
    }
}
fn apply_count(state: &mut MetricState, name: &str, delta: i128) -> StoreResult<()> {
    let total = i128::from(state.count(name))
        .checked_add(delta)
        .and_then(|v| u64::try_from(v).ok())
        .ok_or_else(|| invalid("aggregate counter is outside native result range"))?;
    state.counts.insert(name.into(), total);
    Ok(())
}
