//! 单密钥趋势与模型汇总在数据库内归并，输出规模分别随日期数和模型数增长
use super::*;
use admin_observability::{KeyModelRow, KeyStatisticsOverview, KeyUsageMetric};
use gateway_core::time::DeploymentTimeZone;
use sqlx::types::Json;

const TOTALS: &str = "jsonb_build_object('requests',requests,'tokens',tokens,'costUsd',cost::text,
    'hasEstimatedCost',estimated,'hasMissingCost',missing)";

impl AnalyticsSnapshot<'_> {
    async fn key_usage_query(
        &mut self,
        range: ObservabilityRange,
        key: &str,
        timezone: DeploymentTimeZone,
    ) -> StoreResult<QueryBuilder<Postgres>> {
        self.require_visible(Some(range))?;
        let boundaries = calendar_buckets(range, timezone)?;
        let mut sql = QueryBuilder::new(
            "with parts(bucket,model,counts,costs,contribution) as (
            select bucket_start,dimensions[2],counts,costs,true from request_analytics_1h
            where collection='usage_contribution' and dimensions[4]=",
        );
        sql.push_bind(key.to_owned())
            .push(" and bucket_start >= ")
            .push_bind(range.start)
            .push(" and bucket_start < ")
            .push_bind(range.end);

        let spans = if boundaries
            .iter()
            .all(|t| t.timestamp().rem_euclid(900) == 0)
        {
            let hourly = boundaries
                .iter()
                .all(|t| t.timestamp().rem_euclid(3600) == 0);
            let (hours, quarters) = self.summary_buckets(range, hourly).await?;
            let model =
                aggregate::summary_dimension_sql("key_model", Some(DiagnosticDimension::Model));
            for (table, seconds, buckets) in [
                ("request_analytics_1h", 3600, hours),
                ("request_analytics_15m", 900, quarters),
            ] {
                if buckets.is_empty() {
                    continue;
                }
                sql.push(format!(
                    " union all select bucket_start,{model},counts,costs,false from {table}
                    where collection='key_model' and schema_version="
                ))
                .push_bind(SCHEMA_VERSION)
                .push(" and histogram_version=")
                .push_bind(HISTOGRAM_VERSION)
                .push(" and dimensions[1]=")
                .push_bind(key.to_owned());
                push_summary_buckets(&mut sql, &buckets, seconds);
            }
            self.uncovered_spans(range).await?
        } else {
            vec![(range.start, range.end)]
        };

        if !spans.is_empty() {
            let (starts, ends): (Vec<_>, Vec<_>) = spans.into_iter().unzip();
            sql.push(
                " union all select bucket_start,name,counts,costs,false from (with matching as (
                select mr.*,",
            )
            .push(completed_usage_fact_predicate("mr"))
            .push(" as is_completed_usage,");
            aggregate::push_output_bucket(
                &mut sql,
                "mr.started_at",
                range,
                Some(ObservationGranularity::Day),
                Some(&boundaries),
            );
            sql.push(" as output_bucket,")
                .push(aggregate::fact_dimension_sql(Some(
                    DiagnosticDimension::Model,
                )))
                .push(" as name from unnest(")
                .push_bind(starts)
                .push("::timestamptz[],")
                .push_bind(ends)
                .push(
                    "::timestamptz[]) span(start_at,end_at) cross join lateral (
                    select * from request_analytics_facts f where f.client_api_key_ref=",
                )
                .push_bind(key.to_owned())
                .push(" and f.started_at >= span.start_at and f.started_at < span.end_at");
            push_unrecovered_request_filter(&mut sql, "f");
            sql.push(" offset 0) mr)");
            aggregate::push_fact_aggregation_selected(
                &mut sql,
                false,
                true,
                DistributionRead::None,
                Some(&[
                    "request_count",
                    "total_tokens",
                    "total_tokens_known_count",
                    "calculated_count",
                    "unavailable_count",
                ]),
                None,
            );
            sql.push(") fact_parts");
        }
        sql.push("), daily as (
            select (bucket at time zone ").push_bind(timezone.name()).push(")::date as day, model,
            coalesce(sum((counts->>'request_count')::numeric),0) as requests,
            sum(case when contribution and not counts ? 'total_tokens' and counts ?| array['input_tokens','output_tokens']
                then coalesce((counts->>'input_tokens')::numeric,0) + coalesce((counts->>'output_tokens')::numeric,0)
                else (counts->>'total_tokens')::numeric end)
                filter(where contribution or (counts->>'total_tokens_known_count')::numeric > 0) as tokens,
            sum((costs->>'USD')::numeric) as cost,
            coalesce(sum((counts->>'calculated_count')::numeric),0)>0 as estimated,
            bool_or(coalesce((counts->>'unavailable_count')::numeric,0)>0
                or (costs - array['USD','standard_usd','no_cache_usd']) <> '{}'::jsonb
                or (coalesce((counts->>'request_count')::numeric,0)>0 and not costs ? 'USD')) as missing
            from parts group by 1,2
        ), models as (
            select model,sum(requests) as requests,sum(tokens) as tokens,sum(cost) as cost,
                bool_or(estimated) as estimated,bool_or(missing or cost is null) as missing,
                max(requests) as peak, count(*) filter(where requests>0) as active,
                bool_or(requests<0 or coalesce(tokens<0,false) or coalesce(cost<0,false))
                or coalesce(greatest(max(cost),sum(cost)) > ").push_bind(gateway_core::metering::Decimal::MAX.canonical())
            .push("::numeric,false) as invalid_amount
            from daily group by model
        ) ");
        Ok(sql)
    }

    pub(crate) async fn client_key_statistics(
        &mut self,
        range: ObservabilityRange,
        key: &str,
        timezone: DeploymentTimeZone,
        metric: KeyUsageMetric,
    ) -> StoreResult<KeyStatisticsOverview> {
        let field = match metric {
            KeyUsageMetric::CostUsd => "cost",
            KeyUsageMetric::Tokens => "tokens",
            KeyUsageMetric::Requests => "requests",
        };
        let mut sql = self.key_usage_query(range, key, timezone).await?;
        // 先按整个周期选出模型，再合并其余模型，防止同一颜色在不同日期代表不同模型
        sql.push(format!(", ranked as (
            select model,row_number() over(order by {field} desc nulls last,model collate \"C\") as pos from models
        ), series_days as (
            select day,case when pos<=5 then daily.model end as model,least(pos,6) as pos,sum({field})::text as value
            from daily join ranked using(model) group by 1,2,3
        ), series as (
            select model,pos,jsonb_agg(jsonb_build_object('date',day,'value',value) order by day) as points
            from series_days group by model,pos
        ), totals as (
            select coalesce(sum(requests),0) as requests,sum(tokens) as tokens,sum(cost) as cost,
                coalesce(bool_or(estimated),false) as estimated,coalesce(bool_or(missing),false) as missing,count(*) as model_count,coalesce(bool_or(invalid_amount),false) as invalid_amount from models
        ) select jsonb_build_object('totals',{TOTALS},'modelCount',model_count,'series',
            coalesce((select jsonb_agg(jsonb_build_object('model',model,'points',points) order by pos) from series),'[]'::jsonb)), invalid_amount
            from totals"));
        let (Json(result), invalid_amount): (Json<KeyStatisticsOverview>, bool) = sql
            .build_query_as()
            .fetch_one(&mut *self.tx)
            .await
            .map_err(|error| postgres_unavailable("read bounded key usage trend", error))?;
        if invalid_amount
            || result
                .totals
                .cost_usd
                .as_deref()
                .is_some_and(|amount| amount.parse::<gateway_core::metering::Decimal>().is_err())
        {
            return Err(invalid("key usage aggregate exceeds native range"));
        }
        Ok(result)
    }

    pub(crate) async fn client_key_models(
        &mut self,
        range: ObservabilityRange,
        key: &str,
        timezone: DeploymentTimeZone,
    ) -> StoreResult<Vec<KeyModelRow>> {
        let mut sql = self.key_usage_query(range, key, timezone).await?;
        sql.push(format!(
            "select jsonb_build_object('model',model,'totals',{TOTALS},
            'peakRequests',peak,'activeDays',active),invalid_amount from models order by model collate \"C\""
        ));
        let rows: Vec<(Json<KeyModelRow>, bool)> = sql
            .build_query_as()
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|error| postgres_unavailable("read key model totals", error))?;
        rows.into_iter()
            .map(|(Json(row), invalid_amount)| {
                if invalid_amount {
                    Err(invalid("key usage aggregate exceeds native range"))
                } else {
                    Ok(row)
                }
            })
            .collect()
    }
}
