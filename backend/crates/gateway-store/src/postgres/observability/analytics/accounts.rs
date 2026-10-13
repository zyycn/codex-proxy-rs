//! 账号用量复用汇总并补齐未投影源版本，保持同步查询的精确窗口口径

use super::query::push_summary_buckets;
use super::*;
use gateway_admin::model::{
    account_groups::AccountGroupUsage,
    accounts::{
        AccountCost, AccountModelUsage, AccountUsage, AccountUsageWindowQuery,
        AccountUsageWindowResult,
    },
};

type AccountStates = BTreeMap<(String, String), MetricState>;

impl AnalyticsSnapshot<'_> {
    pub(crate) async fn account_usage_states(
        &mut self,
        range: ObservabilityRange,
        account_ids: &[String],
    ) -> StoreResult<AccountStates> {
        self.account_states(range, account_ids, true).await
    }

    async fn account_states(
        &mut self,
        range: ObservabilityRange,
        account_ids: &[String],
        by_model: bool,
    ) -> StoreResult<AccountStates> {
        if account_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        // 原同步接口在首次回填期间仍可查询；只有完整覆盖的区间才允许用汇总替代源事实
        if !self.projection_covers(range) {
            return self
                .account_fact_states("model_requests", range, account_ids, false, by_model)
                .await;
        }
        let mut states = BTreeMap::new();
        // 排序与分组费用只需要账号总量，不额外展开模型维度
        let (collection, model) = if by_model {
            (
                "account_model",
                "coalesce(nullif(dimensions[4],''),dimensions[3])",
            )
        } else {
            ("account", "''::text")
        };
        let (hours, quarters) = self.summary_buckets(range, true).await?;
        for (table, seconds, buckets) in [
            ("request_analytics_hourly", 3600, hours),
            ("request_analytics_15m", 900, quarters),
        ] {
            if buckets.is_empty() {
                continue;
            }
            let mut query = QueryBuilder::<Postgres>::new(format!(
                "with selected as materialized (select dimensions[1] as account_id,{model} as model,counts,costs,last_used_at from {table} a where collection='{collection}' and schema_version="
            ));
            query
                .push_bind(SCHEMA_VERSION)
                .push(" and histogram_version=")
                .push_bind(HISTOGRAM_VERSION)
                .push(" and dimensions[1]=any(")
                .push_bind(account_ids)
                .push("::text[])");
            push_summary_buckets(&mut query, &buckets, seconds);
            let counters = usage_counter_names();
            // 每行一次展开指标，避免对可能位于 TOAST 的同一 JSON 反复解码
            query.push(format!(
                "), totals as (select account_id,model,jsonb_build_object({}) as counts,max(last_used_at) as last_used_at from selected cross join lateral jsonb_to_record(counts) as metric({}) group by account_id,model),
                currencies as (select account_id,model,c.key,sum(c.value::numeric)::text as amount from selected cross join lateral jsonb_each_text(costs) c where c.key not in ('standard_usd','no_cache_usd') group by account_id,model,c.key),
                costs as (select account_id,model,jsonb_object_agg(key,amount) as costs from currencies group by account_id,model)
                select totals.*,coalesce(costs.costs,'{{}}'::jsonb) as costs,'{{}}'::jsonb as distributions from totals left join costs using(account_id,model)",
                counters.iter().map(|name| format!("'{name}',coalesce(sum(metric.{name}),0)")).collect::<Vec<_>>().join(","),
                counters.iter().map(|name| format!("{name} numeric")).collect::<Vec<_>>().join(",")
            ));
            let rows = query
                .build()
                .fetch_all(&mut *self.tx)
                .await
                .map_err(|error| {
                    postgres_unavailable("aggregate account usage summaries", error)
                })?;
            merge_account_rows(&mut states, rows)?;
        }
        for (start, end) in self.uncovered_spans(range).await? {
            let boundary = self
                .account_fact_states(
                    "request_analytics_facts",
                    ObservabilityRange { start, end },
                    account_ids,
                    false,
                    by_model,
                )
                .await?;
            merge_account_states(&mut states, boundary)?;
        }
        if !self.has_pending_requests().await? {
            states.retain(|_, state| state.count("completed_request_count") > 0);
            return Ok(states);
        }
        let old = self
            .account_fact_states(
                "request_analytics_facts",
                range,
                account_ids,
                true,
                by_model,
            )
            .await?;
        let current = self
            .account_fact_states("model_requests", range, account_ids, true, by_model)
            .await?;
        let mut repair = BTreeSet::new();
        for (key, previous) in old {
            let state = states.entry(key.clone()).or_default();
            for (name, count) in &previous.counts {
                let value = state.counts.entry(name.clone()).or_default();
                *value = value
                    .checked_sub(*count)
                    .ok_or_else(|| invalid("account usage pending counter underflow"))?;
            }
            for (currency, amount) in &previous.costs {
                let before = state
                    .costs
                    .get(currency)
                    .ok_or_else(|| invalid("account usage pending cost is missing"))?;
                let before = before
                    .parse::<gateway_core::metering::Decimal>()
                    .map_err(|_| invalid("invalid account cost"))?;
                let removed = amount
                    .parse::<gateway_core::metering::Decimal>()
                    .map_err(|_| invalid("invalid pending account cost"))?;
                let difference = before
                    .scaled()
                    .checked_sub(removed.scaled())
                    .ok_or_else(|| invalid("account usage pending cost underflow"))?;
                let value = gateway_core::metering::Decimal::from_scaled(difference)
                    .map_err(|_| invalid("invalid account cost difference"))?;
                state.costs.insert(currency.clone(), value.canonical());
            }
            repair.insert(key);
        }
        merge_account_states(&mut states, current)?;
        // 撤销最大值不能用减法；只对发生源版本替换的账号模型做索引定位
        let completed = completed_usage_fact_predicate("mr");
        for (account, model) in repair {
            let state = states
                .get_mut(&(account.clone(), model.clone()))
                .ok_or_else(|| invalid("pending account state disappeared"))?;
            if state.count("completed_request_count") == 0 {
                *state = MetricState::default();
                continue;
            }
            let statement = format!(
                "select started_at from model_requests mr where provider_account_ref=$1 and ($2::text is null or coalesce(upstream_model_id,requested_model_id,'')=$2) and started_at >= $3 and started_at < $4 and {completed} order by started_at desc,id desc limit 1"
            );
            state.last_used_at = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
                .bind(&account)
                .bind(by_model.then_some(&model))
                .bind(range.start)
                .bind(range.end)
                .fetch_optional(&mut *self.tx)
                .await
                .map_err(|error| postgres_unavailable("repair account latest usage", error))?;
            let zero_currencies = state
                .costs
                .iter()
                .filter(|(_, amount)| amount.as_str() == "0")
                .map(|(currency, _)| currency.clone())
                .collect::<Vec<_>>();
            for currency in zero_currencies {
                let statement = format!(
                    "select exists(select 1 from model_requests mr where provider_account_ref=$1 and ($2::text is null or coalesce(upstream_model_id,requested_model_id,'')=$2) and started_at >= $3 and started_at < $4 and {completed} and cost_currency=$5 and cost_amount is not null)"
                );
                let exists: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
                    .bind(&account)
                    .bind(by_model.then_some(&model))
                    .bind(range.start)
                    .bind(range.end)
                    .bind(&currency)
                    .fetch_one(&mut *self.tx)
                    .await
                    .map_err(|error| postgres_unavailable("repair account cost coverage", error))?;
                if !exists {
                    state.costs.remove(&currency);
                }
            }
        }
        states.retain(|_, state| state.count("completed_request_count") > 0);
        Ok(states)
    }

    pub(super) async fn has_pending_requests(&mut self) -> StoreResult<bool> {
        if self.status.oldest_pending_at.is_none() {
            return Ok(false);
        }
        sqlx::query_scalar("select exists(select 1 from request_analytics_pending_requests)")
            .fetch_one(&mut *self.tx)
            .await
            .map_err(|error| postgres_unavailable("check pending account usage", error))
    }

    pub(super) fn projection_covers(&self, range: ObservabilityRange) -> bool {
        self.status.schema_version == SCHEMA_VERSION as u32
            && self.status.histogram_version == HISTOGRAM_VERSION as u32
            && (!self.status.building
                || (self
                    .status
                    .coverage_start
                    .is_some_and(|start| range.start >= start)
                    && self.status.coverage_end.is_some()))
    }

    pub(crate) async fn account_request_counts(
        &mut self,
        range: ObservabilityRange,
        account_ids: &[String],
    ) -> StoreResult<BTreeMap<(String, i64), u64>> {
        if account_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut counts = BTreeMap::new();
        // 公开小时图采用 UTC 整点；非整刻内部窗口直接在源表按相对小时分组
        if !self.projection_covers(range)
            || range.start.timestamp().rem_euclid(900) != 0
            || range.start.timestamp_subsec_nanos() != 0
        {
            let rows = self
                .account_request_fact_counts(
                    "model_requests",
                    range,
                    range.start,
                    account_ids,
                    false,
                )
                .await?;
            merge_request_counts(&mut counts, rows, false)?;
            return Ok(counts);
        }
        let (hours, quarters) = self
            .summary_buckets(range, range.start.timestamp().rem_euclid(3600) == 0)
            .await?;
        for (table, seconds, buckets) in [
            ("request_analytics_hourly", 3600, hours),
            ("request_analytics_15m", 900, quarters),
        ] {
            if buckets.is_empty() {
                continue;
            }
            let mut query = QueryBuilder::<Postgres>::new(
                "select dimensions[1] as account_id,floor(extract(epoch from (bucket_start-",
            );
            query.push_bind(range.start).push("::timestamptz))/3600)::bigint as bucket_index,coalesce(sum((counts->>'completed_request_count')::bigint),0)::bigint as request_count from ")
                .push(table).push(" a where collection='account' and schema_version=").push_bind(SCHEMA_VERSION)
                .push(" and histogram_version=").push_bind(HISTOGRAM_VERSION).push(" and dimensions[1]=any(").push_bind(account_ids).push("::text[])");
            push_summary_buckets(&mut query, &buckets, seconds);
            query.push(" group by dimensions[1],bucket_index");
            let rows = query
                .build()
                .fetch_all(&mut *self.tx)
                .await
                .map_err(|error| {
                    postgres_unavailable("read account hourly request summaries", error)
                })?;
            merge_request_counts(&mut counts, rows, false)?;
        }
        for (start, end) in self.uncovered_spans(range).await? {
            let rows = self
                .account_request_fact_counts(
                    "request_analytics_facts",
                    ObservabilityRange { start, end },
                    range.start,
                    account_ids,
                    false,
                )
                .await?;
            merge_request_counts(&mut counts, rows, false)?;
        }
        if self.has_pending_requests().await? {
            let old = self
                .account_request_fact_counts(
                    "request_analytics_facts",
                    range,
                    range.start,
                    account_ids,
                    true,
                )
                .await?;
            merge_request_counts(&mut counts, old, true)?;
            let current = self
                .account_request_fact_counts(
                    "model_requests",
                    range,
                    range.start,
                    account_ids,
                    true,
                )
                .await?;
            merge_request_counts(&mut counts, current, false)?;
        }
        Ok(counts)
    }

    async fn account_request_fact_counts(
        &mut self,
        table: &'static str,
        range: ObservabilityRange,
        origin: DateTime<Utc>,
        account_ids: &[String],
        pending_only: bool,
    ) -> StoreResult<Vec<sqlx::postgres::PgRow>> {
        let mut query = QueryBuilder::<Postgres>::new(
            "select mr.provider_account_ref as account_id,floor(extract(epoch from (mr.started_at-",
        );
        query.push_bind(origin).push("::timestamptz))/3600)::bigint as bucket_index,count(*)::bigint as request_count from ").push(table).push(" mr");
        if pending_only {
            query.push(" join request_analytics_pending_requests p on p.request_id=mr.id");
        }
        query
            .push(" where mr.provider_account_ref=any(")
            .push_bind(account_ids)
            .push("::text[]) and mr.started_at >= ")
            .push_bind(range.start)
            .push(" and mr.started_at < ")
            .push_bind(range.end);
        push_completed_usage_fact_filter(&mut query, "mr");
        query.push(" group by mr.provider_account_ref,bucket_index");
        query
            .build()
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|error| {
                postgres_unavailable("aggregate account hourly request boundaries", error)
            })
    }

    async fn account_fact_states(
        &mut self,
        table: &'static str,
        range: ObservabilityRange,
        account_ids: &[String],
        pending_only: bool,
        by_model: bool,
    ) -> StoreResult<AccountStates> {
        let mut query = QueryBuilder::<Postgres>::new(usage_fact_aggregate_sql(table, by_model));
        if pending_only {
            query.push(
                " join request_analytics_pending_requests pending on pending.request_id=mr.id",
            );
        }
        query
            .push(" where mr.provider_account_ref=any(")
            .push_bind(account_ids)
            .push("::text[]) and mr.started_at >= ")
            .push_bind(range.start)
            .push(" and mr.started_at < ")
            .push_bind(range.end);
        push_completed_usage_fact_filter(&mut query, "mr");
        query.push(" group by mr.provider_account_ref,mr.cost_currency");
        if by_model {
            query.push(",coalesce(mr.upstream_model_id,mr.requested_model_id,'')");
        }
        let rows = query
            .build()
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|error| postgres_unavailable("aggregate account usage facts", error))?;
        let mut states = BTreeMap::new();
        merge_account_rows(&mut states, rows)?;
        Ok(states)
    }
}

fn merge_request_counts(
    counts: &mut BTreeMap<(String, i64), u64>,
    rows: Vec<sqlx::postgres::PgRow>,
    subtract: bool,
) -> StoreResult<()> {
    for row in rows {
        let value = unsigned(&row, "request_count")?;
        let total = counts
            .entry((get(&row, "account_id")?, get(&row, "bucket_index")?))
            .or_default();
        *total = if subtract {
            total.checked_sub(value)
        } else {
            total.checked_add(value)
        }
        .ok_or_else(|| invalid("invalid account request count delta"))?;
    }
    Ok(())
}

fn merge_account_rows(
    states: &mut AccountStates,
    rows: Vec<sqlx::postgres::PgRow>,
) -> StoreResult<()> {
    for row in rows {
        states
            .entry((get(&row, "account_id")?, get(&row, "model")?))
            .or_default()
            .merge(&MetricState::decode(&row)?)?;
    }
    Ok(())
}

fn merge_account_states(states: &mut AccountStates, incoming: AccountStates) -> StoreResult<()> {
    for (key, state) in incoming {
        states.entry(key).or_default().merge(&state)?;
    }
    Ok(())
}

fn usage_counter_names() -> Vec<String> {
    let mut names = [
        "completed_request_count",
        "image_request_count",
        "image_request_failed_count",
        "window_total_tokens",
        "provider_reported_count",
        "calculated_count",
        "unavailable_count",
    ]
    .map(str::to_owned)
    .to_vec();
    for name in super::metrics::TOKEN_METRICS {
        names.push(name.to_owned());
        names.push(format!("{name}_known_count"));
    }
    names
}

fn usage_fact_aggregate_sql(table: &str, by_model: bool) -> String {
    let model = if by_model {
        "coalesce(mr.upstream_model_id,mr.requested_model_id,'')"
    } else {
        "''::text"
    };
    let mut counters = vec![
        "'completed_request_count',count(*)".to_owned(),
        "'image_request_count',count(*) filter(where image_generation_succeeded is true)".to_owned(),
        "'image_request_failed_count',count(*) filter(where image_generation_succeeded is false)".to_owned(),
        "'window_total_tokens',coalesce(sum(coalesce(total_tokens,coalesce(input_tokens,0)+coalesce(output_tokens,0))),0)".to_owned(),
    ];
    for name in super::metrics::TOKEN_METRICS {
        counters.push(format!(
            "'{name}',coalesce(sum({name}),0),'{name}_known_count',count({name})"
        ));
    }
    for source in ["provider_reported", "calculated", "unavailable"] {
        counters.push(format!(
            "'{source}_count',count(*) filter(where cost_source='{source}')"
        ));
    }
    format!("select mr.provider_account_ref as account_id,{model} as model,
         jsonb_build_object({}) as counts,case when mr.cost_currency is not null and sum(mr.cost_amount) is not null
           then jsonb_build_object(mr.cost_currency,sum(mr.cost_amount)::text) else '{{}}'::jsonb end as costs,
         '{{}}'::jsonb as distributions,max(mr.started_at) as last_used_at from {table} mr",counters.join(","))
}

pub(crate) async fn account_usage_windows(
    connection: &mut ObservabilityConnection,
    windows: &[AccountUsageWindowQuery],
) -> StoreResult<Vec<AccountUsageWindowResult>> {
    let mut snapshot = AnalyticsSnapshot::new_for_accounts(connection).await?;
    // 相同窗口一次查询所有账号，避免列表逐账号重复读取同一套时间桶
    let mut grouped = BTreeMap::<(DateTime<Utc>, DateTime<Utc>), BTreeSet<String>>::new();
    for window in windows {
        grouped
            .entry((window.range.start, window.range.end))
            .or_default()
            .insert(window.account_id.clone());
    }
    let mut results = Vec::with_capacity(windows.len());
    for ((start, end), accounts) in grouped {
        let ids = accounts.into_iter().collect::<Vec<_>>();
        let states = snapshot
            .account_usage_states(ObservabilityRange { start, end }, &ids)
            .await?;
        for window in windows
            .iter()
            .filter(|window| window.range.start == start && window.range.end == end)
        {
            let mut total = MetricState::default();
            let mut models = Vec::new();
            for ((account, model), state) in &states {
                if account != &window.account_id {
                    continue;
                }
                total.merge(state)?;
                if !model.is_empty()
                    && let Some(last_used_at) = state.last_used_at()
                {
                    models.push(AccountModelUsage {
                        model: model.clone(),
                        request_count: state.count("completed_request_count"),
                        success_count: state.count("completed_request_count"),
                        input_tokens: state.optional_count("input_tokens"),
                        output_tokens: state.optional_count("output_tokens"),
                        cached_input_tokens: state.optional_count("cached_input_tokens"),
                        cache_write_input_tokens: state.optional_count("cache_write_input_tokens"),
                        reasoning_output_tokens: state.optional_count("reasoning_output_tokens"),
                        image_input_tokens: state.optional_count("image_input_tokens"),
                        image_output_tokens: state.optional_count("image_output_tokens"),
                        total_tokens: Some(state.count("window_total_tokens")),
                        image_request_count: state.count("image_request_count"),
                        image_request_failed_count: state.count("image_request_failed_count"),
                        cost_coverage: admin_cost_coverage(state.coverage()),
                        costs: account_costs(state)?,
                        last_used_at,
                    });
                }
            }
            models.sort_by(|a, b| {
                b.request_count
                    .cmp(&a.request_count)
                    .then(a.model.cmp(&b.model))
            });
            results.push(AccountUsageWindowResult {
                account_id: window.account_id.clone(),
                key: window.key.clone(),
                usage: AccountUsage {
                    account_id: window.account_id.clone(),
                    request_count: total.count("completed_request_count"),
                    success_count: total.count("completed_request_count"),
                    input_tokens: total.optional_count("input_tokens"),
                    output_tokens: total.optional_count("output_tokens"),
                    cached_input_tokens: total.optional_count("cached_input_tokens"),
                    cache_write_input_tokens: total.optional_count("cache_write_input_tokens"),
                    reasoning_output_tokens: total.optional_count("reasoning_output_tokens"),
                    image_input_tokens: total.optional_count("image_input_tokens"),
                    image_output_tokens: total.optional_count("image_output_tokens"),
                    total_tokens: Some(total.count("window_total_tokens")),
                    image_request_count: total.count("image_request_count"),
                    image_request_failed_count: total.count("image_request_failed_count"),
                    cost_coverage: admin_cost_coverage(total.coverage()),
                    costs: account_costs(&total)?,
                    last_used_at: total.last_used_at(),
                    request_buckets: Vec::new(),
                    models,
                },
            });
        }
    }
    snapshot.finish().await?;
    results.sort_by(|a, b| a.account_id.cmp(&b.account_id).then(a.key.cmp(&b.key)));
    Ok(results)
}

fn account_costs(state: &MetricState) -> StoreResult<Vec<AccountCost>> {
    Ok(state
        .costs()?
        .into_iter()
        .map(|cost| AccountCost {
            currency: cost.currency,
            amount: cost.amount,
        })
        .collect())
}

pub(crate) async fn account_usage_sort_values(
    snapshot: &mut AnalyticsSnapshot<'_>,
    now: DateTime<Utc>,
) -> StoreResult<(Vec<String>, Vec<i64>, Vec<Option<DateTime<Utc>>>)> {
    let days: i32 =
        sqlx::query_scalar("select usage_retention_days::integer from runtime_settings where id=1")
            .fetch_one(&mut *snapshot.tx)
            .await
            .map_err(|error| postgres_unavailable("read account usage retention", error))?;
    let ids: Vec<String> = sqlx::query_scalar("select id from provider_accounts")
        .fetch_all(&mut *snapshot.tx)
        .await
        .map_err(|error| postgres_unavailable("read account usage sort identities", error))?;
    let range = ObservabilityRange::new(now - TimeDelta::days(i64::from(days)), now)?;
    let mut totals = BTreeMap::<String, MetricState>::new();
    for ((account, _), state) in snapshot.account_states(range, &ids, false).await? {
        totals.entry(account).or_default().merge(&state)?;
    }
    let mut tokens = Vec::with_capacity(ids.len());
    let mut latest = Vec::with_capacity(ids.len());
    for id in &ids {
        let state = totals.remove(id).unwrap_or_default();
        tokens.push(
            i64::try_from(state.count("window_total_tokens"))
                .map_err(|_| invalid("account usage sort token overflow"))?,
        );
        latest.push(state.last_used_at());
    }
    Ok((ids, tokens, latest))
}

pub(crate) async fn account_group_usage(
    connection: &mut ObservabilityConnection,
    group_ids: &[String],
    timezone: gateway_core::time::DeploymentTimeZone,
) -> StoreResult<BTreeMap<String, AccountGroupUsage>> {
    if group_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut snapshot = AnalyticsSnapshot::new_for_accounts(connection).await?;
    let now = Utc::now();
    let days: i32 =
        sqlx::query_scalar("select usage_retention_days::integer from runtime_settings where id=1")
            .fetch_one(&mut *snapshot.tx)
            .await
            .map_err(|error| postgres_unavailable("read group usage retention", error))?;
    let memberships:Vec<(String,String)>=sqlx::query_as("select account_group_id,provider_account_id from account_group_accounts where account_group_id=any($1::text[])").bind(group_ids).fetch_all(&mut *snapshot.tx).await.map_err(|error|postgres_unavailable("read group usage membership",error))?;
    let ids = memberships
        .iter()
        .map(|(_, account)| account.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let retained = ObservabilityRange::new(now - TimeDelta::days(i64::from(days)), now)?;
    let today = ObservabilityRange::new(
        timezone
            .day_start(now)
            .ok_or_else(|| invalid("invalid business date"))?
            .max(retained.start),
        now,
    )?;
    let retained_states = snapshot.account_states(retained, &ids, false).await?;
    let today_states = snapshot.account_states(today, &ids, false).await?;
    let mut results = BTreeMap::new();
    for group in group_ids {
        let mut usage = AccountGroupUsage {
            today_usd: "0".parse().map_err(|_| invalid("invalid zero cost"))?,
            retained_total_usd: "0".parse().map_err(|_| invalid("invalid zero cost"))?,
        };
        for (_, account) in memberships.iter().filter(|(id, _)| id == group) {
            for (states, total) in [
                (&retained_states, &mut usage.retained_total_usd),
                (&today_states, &mut usage.today_usd),
            ] {
                for ((id, _), state) in states {
                    if id == account
                        && let Some(amount) = state.costs.get("USD")
                    {
                        let amount = amount
                            .parse()
                            .map_err(|_| invalid("invalid group usage cost"))?;
                        *total = total
                            .checked_add(&amount)
                            .ok_or_else(|| invalid("group usage cost overflow"))?;
                    }
                }
            }
        }
        results.insert(group.clone(), usage);
    }
    snapshot.finish().await?;
    Ok(results)
}
