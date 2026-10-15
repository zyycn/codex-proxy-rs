//! 在一致性快照中选择互斥的小时、十五分钟与窄事实区间

use super::*;
use futures::TryStreamExt;

pub(crate) struct AnalyticsSnapshot<'a> {
    pub tx: sqlx::Transaction<'a, Postgres>,
    pub(super) status: AnalyticsStatus,
    approximate: bool,
}

#[derive(Clone, Copy)]
pub(super) struct ReadSelection<'a> {
    pub(super) distributions: bool,
    pub(super) costs: bool,
    pub(super) keys: Option<&'a [String]>,
    pub(super) counters: Option<&'a [&'a str]>,
    pub(super) distribution_names: Option<&'a [&'a str]>,
}

impl<'a> AnalyticsSnapshot<'a> {
    pub async fn new(connection: &'a mut ObservabilityConnection) -> StoreResult<Self> {
        Self::begin(connection, true).await
    }

    pub(crate) async fn new_for_accounts(
        connection: &'a mut ObservabilityConnection,
    ) -> StoreResult<Self> {
        Self::begin(connection, false).await
    }

    async fn begin(
        connection: &'a mut ObservabilityConnection,
        require_current_schema: bool,
    ) -> StoreResult<Self> {
        let mut tx = begin_snapshot(connection).await?;
        // 时间跨度和分组选择性差异很大，通用计划会把长区间误估成短区间
        // 仅当前只读事务使用实际参数规划，不改变连接池后续事务的计划策略
        sqlx::query("set local plan_cache_mode = force_custom_plan")
            .execute(&mut *tx)
            .await
            .map_err(|error| postgres_unavailable("plan analytics with actual bounds", error))?;
        let status = worker::query_status(&mut tx).await?;
        if require_current_schema
            && (status.schema_version != SCHEMA_VERSION as u32
                || status.histogram_version != HISTOGRAM_VERSION as u32)
        {
            return Err(unavailable(
                "request analytics is building; retry after the background projection completes",
            ));
        }
        Ok(Self {
            tx,
            status,
            approximate: false,
        })
    }

    pub fn require_visible(&self, range: Option<ObservabilityRange>) -> StoreResult<()> {
        if self.status.building
            && !range.is_some_and(|range| {
                self.status
                    .coverage_start
                    .is_some_and(|start| range.start >= start)
                    && self.status.coverage_end.is_some()
            })
        {
            return Err(unavailable(
                "request analytics has not backfilled the complete requested range",
            ));
        }
        // 源清理尚未投影时继续读取已发布事实，Worker 在同一事务撤销事实与汇总贡献
        Ok(())
    }

    pub fn metadata(&self) -> admin_observability::AnalyticsMetadata {
        let generated_at = Utc::now();
        admin_observability::AnalyticsMetadata {
            coverage_start: self.status.coverage_start,
            coverage_end: self.status.coverage_end,
            generated_at,
            lag_seconds: self.status.oldest_pending_at.map_or(0, |time| {
                generated_at
                    .signed_duration_since(time)
                    .num_seconds()
                    .max(0) as u64
            }),
            state: if self.status.building {
                admin_observability::AnalyticsState::Building
            } else if self.status.oldest_pending_at.is_some() {
                admin_observability::AnalyticsState::Lagging
            } else {
                admin_observability::AnalyticsState::Ready
            },
            percentiles_approximate: self.approximate,
            generation: self.status.generation.max(0) as u64,
            schema_version: self.status.schema_version,
            histogram_version: self.status.histogram_version,
        }
    }

    pub async fn finish(self) -> StoreResult<()> {
        self.tx
            .commit()
            .await
            .map_err(|error| postgres_unavailable("complete analytics query snapshot", error))
    }

    pub async fn totals(&mut self) -> StoreResult<DashboardTotals> {
        self.require_visible(None)?;
        let bounds=sqlx::query("select least((select started_at from request_analytics_facts order by started_at,id limit 1),(select bucket_start from request_analytics_1h where collection='usage_contribution' order by bucket_start limit 1)) as first_started_at,greatest((select started_at from request_analytics_facts order by started_at desc,id desc limit 1),(select bucket_start from request_analytics_1h where collection='usage_contribution' order by bucket_start desc limit 1)) as last_started_at")
            .fetch_one(&mut *self.tx).await.map_err(|error|postgres_unavailable("read retained analytics bounds",error))?;
        let start =
            get::<Option<DateTime<Utc>>>(&bounds, "first_started_at")?.unwrap_or_else(Utc::now);
        let last = get::<Option<DateTime<Utc>>>(&bounds, "last_started_at")?.unwrap_or(start);
        // 保留期总量没有用户边界，向外对齐完整桶避免反复扫描首尾热桶
        let range = ObservabilityRange {
            start: bucket_for(start, 900)?,
            end: bucket_for(last, 900)?
                .checked_add_signed(TimeDelta::minutes(15))
                .ok_or_else(|| invalid("analytics time boundary overflow"))?,
        };
        let states = self
            .load(
                range,
                &UsageRecordFilter::default(),
                None,
                None,
                Default::default(),
                false,
            )
            .await?;
        let state = merge_states(states.into_values())?;
        Ok(DashboardTotals {
            request_count: state.count("request_count"),
            input_tokens: state.count("input_tokens"),
            cached_input_tokens: state.count("cached_input_tokens"),
            total_tokens: state.count("total_tokens"),
            billing_usd: state
                .costs()?
                .into_iter()
                .find(|value| value.currency == "USD")
                .map(|value| value.amount),
        })
    }

    pub async fn overview(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
    ) -> StoreResult<UsageOverview> {
        let states = self
            .load(range, filter, None, None, Default::default(), true)
            .await?;
        let state = merge_states(states.into_values())?;
        self.overview_from_state(range, filter, state).await
    }

    async fn overview_from_state(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
        state: MetricState,
    ) -> StoreResult<UsageOverview> {
        // Provider 列表只消费计数，不重复合并费用与分布
        let provider_states = self
            .load_selected(
                range,
                filter,
                Some(DiagnosticDimension::Provider),
                None,
                Default::default(),
                ReadSelection {
                    distributions: false,
                    costs: false,
                    keys: None,
                    counters: Some(&[
                        "request_count",
                        "attempt_count",
                        "failure_count",
                        "total_tokens",
                    ]),
                    distribution_names: None,
                },
            )
            .await?;
        let mut providers = provider_states
            .into_iter()
            .map(|((_, name), state)| ProviderObservation {
                provider_kind: name,
                request_count: state.count("request_count"),
                attempt_count: state.count("attempt_count"),
                failure_count: state.count("failure_count"),
                total_tokens: state.count("total_tokens"),
            })
            .collect::<Vec<_>>();
        providers.sort_by(|a, b| {
            b.request_count
                .cmp(&a.request_count)
                .then(a.provider_kind.cmp(&b.provider_kind))
        });
        Ok(UsageOverview {
            range,
            requests: state.request_metrics()?,
            attempts: state.attempts()?,
            providers,
            analytics: self.metadata(),
        })
    }

    pub async fn trend(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
        granularity: ObservationGranularity,
        timezone: gateway_core::time::DeploymentTimeZone,
        include_costs: bool,
    ) -> StoreResult<Vec<RequestMetricPoint>> {
        let states = self
            .load_selected(
                range,
                filter,
                None,
                Some(granularity),
                timezone,
                ReadSelection {
                    distributions: true,
                    costs: include_costs,
                    keys: None,
                    counters: None,
                    distribution_names: None,
                },
            )
            .await?;
        metric_points(range, granularity, timezone, &states, include_costs)
    }

    pub async fn insights(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
        granularity: ObservationGranularity,
        timezone: gateway_core::time::DeploymentTimeZone,
    ) -> StoreResult<(
        UsageOverview,
        Vec<RequestMetricPoint>,
        Vec<admin_observability::UsageProjectedCostPoint>,
    )> {
        let states = self
            .load(range, filter, None, Some(granularity), timezone, true)
            .await?;
        let trend = metric_points(range, granularity, timezone, &states, true)?;
        let costs = states
            .iter()
            .map(|((bucket_start, _), state)| {
                Ok(admin_observability::UsageProjectedCostPoint {
                    bucket_start: *bucket_start,
                    standard_amount: state.projected_cost("standard_usd")?,
                    no_cache_amount: state.projected_cost("no_cache_usd")?,
                })
            })
            .collect::<StoreResult<Vec<_>>>()?;
        // 趋势已包含可合并的完整指标和情景费用，概览直接复用同一份状态
        // 精确分位数不可跨桶合并，明细回退时仍按完整区间单独计算概览
        let overview = if states.values().any(MetricState::has_exact_percentiles) {
            self.overview(range, filter).await?
        } else {
            self.overview_from_state(range, filter, merge_states(states.into_values())?)
                .await?
        };
        Ok((overview, trend, costs))
    }

    pub async fn diagnostics(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
        dimension: DiagnosticDimension,
        limit: u16,
    ) -> StoreResult<DiagnosticsObservation> {
        let limit = ObservabilityPageSize::new(limit)
            .map_err(|_| invalid("invalid diagnostic limit"))?
            .get();
        // 高基数账号和 Key 先合并整个区间的精确计数选 TopN，再读取少量选中组的分布
        let ranked = if matches!(
            dimension,
            DiagnosticDimension::Account | DiagnosticDimension::ApiKey
        ) && matches!(
            collection_for(filter, Some(dimension)),
            Some("account" | "key")
        ) {
            Some(
                self.diagnostic_top_keys(range, filter, dimension, limit)
                    .await?,
            )
        } else {
            None
        };
        let selected_keys = ranked.as_ref().map(|(_, keys)| keys.as_slice());
        let states = self
            .load_selected(
                range,
                filter,
                Some(dimension),
                None,
                Default::default(),
                ReadSelection {
                    distributions: true,
                    costs: true,
                    keys: selected_keys,
                    counters: Some(&[
                        "request_count",
                        "success_count",
                        "failure_count",
                        "attempt_count",
                        "total_tokens",
                        "latency_ms_sum",
                        "cancelled_count",
                        "incomplete_count",
                        "retry_count",
                        "retried_request_count",
                        "provider_reported_count",
                        "calculated_count",
                        "unavailable_count",
                    ]),
                    distribution_names: Some(&["latency_ms", "first_token_ms"]),
                },
            )
            .await?;
        let total_request_count = if let Some((total, _)) = &ranked {
            *total
        } else {
            states.values().try_fold(0_u64, |sum, state| {
                sum.checked_add(state.count("request_count"))
                    .ok_or_else(|| invalid("analytics counter overflow"))
            })?
        };
        let mut states = states.into_iter().collect::<Vec<_>>();
        states.sort_by(|a, b| {
            b.1.count("request_count")
                .cmp(&a.1.count("request_count"))
                .then(a.0.1.cmp(&b.0.1))
        });
        states.truncate(usize::from(limit));
        let mut observations = Vec::with_capacity(states.len());
        for ((_, key), state) in states {
            let metrics = state.request_metrics()?;
            observations.push(DiagnosticObservation {
                key: key.clone(),
                name: key,
                account_provider_kind: None,
                account_plan_type: None,
                request_count: metrics.request_count,
                success_count: metrics.success_count,
                failure_count: metrics.failure_count,
                attempt_count: state.count("attempt_count"),
                total_tokens: metrics.total_tokens,
                average_latency_ms: (metrics.latency_count > 0).then(|| {
                    (metrics.latency_sum_ms as f64 / metrics.latency_count as f64).round() as u64
                }),
                latency_p95_ms: state
                    .percentile("latency_ms", 0.95)
                    .map(|value| value.round() as u64),
                first_token_p95_ms: state
                    .percentile("first_token_ms", 0.95)
                    .map(|value| value.round() as u64),
                non_completion_count: metrics
                    .cancelled_count
                    .checked_add(metrics.incomplete_count)
                    .ok_or_else(|| invalid("non-completion count overflow"))?,
                retry_count: state.count("retry_count"),
                retried_request_count: state.count("retried_request_count"),
                cost_coverage: state.coverage(),
                costs: state.costs()?,
            });
        }
        if matches!(
            dimension,
            DiagnosticDimension::Account | DiagnosticDimension::ApiKey
        ) && !observations.is_empty()
        {
            let ids = observations
                .iter()
                .map(|item| item.key.clone())
                .collect::<Vec<_>>();
            let statement = if dimension == DiagnosticDimension::Account {
                "select requested.id, historical.provider_kind, historical.provider_account_plan_type_snapshot as plan_type, coalesce(historical.provider_account_email_snapshot,historical.provider_account_name_snapshot,requested.id) as name from unnest($1::text[]) requested(id) left join lateral (select provider_kind,provider_account_plan_type_snapshot,provider_account_email_snapshot,provider_account_name_snapshot from model_request_observations mr where mr.provider_account_ref=requested.id order by mr.started_at desc,mr.id desc limit 1) historical on true"
            } else {
                "select requested.id, null::text as provider_kind, null::text as plan_type,coalesce(historical.client_api_key_name_snapshot,requested.id) as name from unnest($1::text[]) requested(id) left join lateral (select client_api_key_name_snapshot from model_request_observations mr where mr.client_api_key_ref=requested.id order by mr.started_at desc,mr.id desc limit 1) historical on true"
            };
            let labels = sqlx::query(statement)
                .bind(ids)
                .fetch_all(&mut *self.tx)
                .await
                .map_err(|error| postgres_unavailable("read historical analytics labels", error))?;
            for row in labels {
                let id: String = get(&row, "id")?;
                if let Some(item) = observations.iter_mut().find(|item| item.key == id) {
                    item.name = get(&row, "name")?;
                    item.account_provider_kind = get(&row, "provider_kind")?;
                    item.account_plan_type = get(&row, "plan_type")?;
                }
            }
        }
        Ok(DiagnosticsObservation {
            total_request_count,
            items: observations,
            analytics: self.metadata(),
        })
    }

    async fn diagnostic_top_keys(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
        dimension: DiagnosticDimension,
        limit: u16,
    ) -> StoreResult<(u64, Vec<String>)> {
        filter.validate()?;
        self.require_visible(Some(range))?;
        let (collection, column, reference) = if dimension == DiagnosticDimension::Account {
            (
                "account",
                "provider_account_ref",
                &filter.provider_account_ref,
            )
        } else {
            ("key", "client_api_key_ref", &filter.client_api_key_ref)
        };
        let spans = self.uncovered_spans(range).await?;
        let (starts, ends): (Vec<_>, Vec<_>) = spans.into_iter().unzip();
        let (hours, quarters) = self.summary_buckets(range, true).await?;
        let mut day_hours = BTreeMap::<DateTime<Utc>, usize>::new();
        for hour in &hours {
            *day_hours.entry(bucket_for(*hour, 86400)?).or_default() += 1;
        }
        let days = day_hours
            .into_iter()
            .filter_map(|(day, count)| (count == 24).then_some(day))
            .collect::<Vec<_>>();
        let hours = hours
            .into_iter()
            .filter(|hour| {
                !days
                    .iter()
                    .any(|day| *hour >= *day && *hour < *day + TimeDelta::days(1))
            })
            .collect::<Vec<_>>();
        let mut query = QueryBuilder::<Postgres>::new("with edge_facts as materialized (");
        if starts.is_empty() {
            query.push("select null::text as key,0::bigint as request_count where false");
        } else {
            query.push("select edge.key,sum(edge.request_count)::bigint as request_count from unnest(").push_bind(starts).push("::timestamptz[],").push_bind(ends).push("::timestamptz[]) span(start_at,end_at) cross join lateral (select ")
                .push(format!("coalesce(mr.{column},'') as key,count(*)::bigint as request_count from request_analytics_facts mr where mr.started_at >= span.start_at and mr.started_at < span.end_at"));
            push_unrecovered_request_filter(&mut query, "mr");
            push_analytics_filter(&mut query, filter, "mr");
            push_diagnostic_dimension_filter(&mut query, dimension);
            query.push(format!(
                " group by coalesce(mr.{column},'')) edge group by edge.key"
            ));
        }
        query.push("), contributions as (select key,request_count from edge_facts");
        for (table, column, seconds, buckets) in [
            ("request_analytics_1d_counts", "dimension", 86400, days),
            ("request_analytics_1h", "dimensions[1]", 3600, hours),
            ("request_analytics_15m", "dimensions[1]", 900, quarters),
        ] {
            if buckets.is_empty() {
                continue;
            }
            // 范围扫描可并行合并计数，额外桶集合排除 dirty 与首尾不完整桶
            query.push(format!(" union all select {column} as key,sum(request_count)::bigint as request_count from {table} a where collection in ('account','key') and collection="))
                .push_bind(collection).push(" and schema_version=").push_bind(SCHEMA_VERSION)
                .push(" and histogram_version=").push_bind(HISTOGRAM_VERSION);
            push_summary_buckets(&mut query, &buckets, seconds);
            if let Some(outcome) = &filter.outcome {
                query.push(" and outcome=").push_bind(outcome);
            }
            if let Some(reference) = reference {
                query.push(format!(" and {column}=")).push_bind(reference);
            }
            query.push(format!(" group by {column}"));
        }
        if filter.outcome.is_none() {
            // 贡献也在数据库内参与 TopN，避免有补录时退回加载全部账号或 Key
            let index = if dimension == DiagnosticDimension::Account {
                3
            } else {
                4
            };
            query.push(format!(" union all select dimensions[{index}] as key,coalesce((counts->>'request_count')::bigint,0) as request_count from request_analytics_1h where collection='usage_contribution' and dimensions[{index}]<>'' and bucket_start >= "))
                .push_bind(range.start).push(" and bucket_start < ").push_bind(range.end);
            if let Some(reference) = reference {
                query
                    .push(format!(" and dimensions[{index}]="))
                    .push_bind(reference);
            }
        }
        query.push("), grouped as (select key,sum(request_count)::bigint as request_count from contributions group by key) select key,request_count,sum(request_count) over()::bigint as total_request_count from grouped order by request_count desc,");
        if dimension == DiagnosticDimension::Account {
            query.push("coalesce(nullif(key,''),'unrouted')");
        } else {
            query.push("key");
        }
        query.push(" limit ").push_bind(i64::from(limit));
        let rows = query
            .build()
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|error| postgres_unavailable("rank full-range analytics groups", error))?;
        let total = rows
            .first()
            .map(|row| unsigned(row, "total_request_count"))
            .transpose()?
            .unwrap_or_default();
        let keys = rows
            .iter()
            .map(|row| get(row, "key"))
            .collect::<StoreResult<Vec<String>>>()?;
        Ok((total, keys))
    }

    pub(super) async fn summary_buckets(
        &mut self,
        range: ObservabilityRange,
        use_hourly: bool,
    ) -> StoreResult<(Vec<DateTime<Utc>>, Vec<DateTime<Utc>>)> {
        // 先按时间确定互斥的完整桶，空的十五分钟集合不读取汇总表
        let rows = sqlx::query("with quarters as materialized (select q.bucket_start,date_bin(interval '1 hour',q.bucket_start,timestamptz '2000-01-01 UTC') as hour_start from generate_series(date_bin(interval '15 minutes',$1::timestamptz,timestamptz '2000-01-01 UTC'),$2::timestamptz-interval '15 minutes',interval '15 minutes') q(bucket_start) where q.bucket_start >= $1 and not exists(select 1 from request_analytics_pending_buckets p where p.bucket_start=q.bucket_start)), hours as (select hour_start from quarters where $3 group by hour_start having count(*)=4) select q.bucket_start,h.hour_start from quarters q left join hours h on h.hour_start=q.hour_start order by q.bucket_start")
            .bind(range.start).bind(range.end).bind(use_hourly)
            .fetch_all(&mut *self.tx).await.map_err(|error|postgres_unavailable("select disjoint analytics buckets",error))?;
        let mut hours = Vec::new();
        let mut quarters = Vec::new();
        for row in rows {
            if let Some(hour) = get::<Option<DateTime<Utc>>>(&row, "hour_start")? {
                if hours.last() != Some(&hour) {
                    hours.push(hour);
                }
            } else {
                quarters.push(get(&row, "bucket_start")?);
            }
        }
        Ok((hours, quarters))
    }

    pub(super) async fn uncovered_spans(
        &mut self,
        range: ObservabilityRange,
    ) -> StoreResult<Vec<(DateTime<Utc>, DateTime<Utc>)>> {
        let first = bucket_for(range.start, 900)?;
        let complete_start = if first == range.start {
            first
        } else {
            first + TimeDelta::minutes(15)
        };
        let complete_end = bucket_for(range.end, 900)?;
        let mut spans = Vec::new();
        if range.start < complete_start {
            spans.push((range.start, complete_start.min(range.end)));
        }
        let right_start = complete_end.max(complete_start).max(range.start);
        if right_start < range.end {
            spans.push((right_start, range.end));
        }
        if complete_start < complete_end {
            let dirty:Vec<DateTime<Utc>>=sqlx::query_scalar("select bucket_start from request_analytics_pending_buckets where bucket_start >= $1 and bucket_start < $2 order by bucket_start")
                .bind(complete_start).bind(complete_end).fetch_all(&mut *self.tx).await.map_err(|error|postgres_unavailable("plan bounded analytics fact spans",error))?;
            spans.extend(
                dirty
                    .into_iter()
                    .map(|start| (start, start + TimeDelta::minutes(15))),
            );
        }
        spans.sort_unstable();
        let mut merged: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
        for (start, end) in spans {
            if let Some(previous) = merged.last_mut()
                && previous.1 == start
            {
                previous.1 = end;
            } else {
                merged.push((start, end));
            }
        }
        Ok(merged)
    }

    async fn load(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
        dimension: Option<DiagnosticDimension>,
        granularity: Option<ObservationGranularity>,
        timezone: gateway_core::time::DeploymentTimeZone,
        read_distributions: bool,
    ) -> StoreResult<BTreeMap<(DateTime<Utc>, String), MetricState>> {
        self.load_selected(
            range,
            filter,
            dimension,
            granularity,
            timezone,
            ReadSelection {
                distributions: read_distributions,
                costs: true,
                keys: None,
                counters: None,
                distribution_names: None,
            },
        )
        .await
    }

    async fn load_selected(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
        dimension: Option<DiagnosticDimension>,
        granularity: Option<ObservationGranularity>,
        timezone: gateway_core::time::DeploymentTimeZone,
        selection: ReadSelection<'_>,
    ) -> StoreResult<BTreeMap<(DateTime<Utc>, String), MetricState>> {
        let mut states = self
            .load_native_selected(range, filter, dimension, granularity, timezone, selection)
            .await?;
        self.merge_contributions(
            super::contributions::ContributionQuery {
                range,
                filter,
                dimension,
                granularity,
                timezone,
                account_models: None,
                selection,
            },
            &mut states,
        )
        .await?;
        Ok(states)
    }

    async fn load_native_selected(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
        dimension: Option<DiagnosticDimension>,
        granularity: Option<ObservationGranularity>,
        timezone: gateway_core::time::DeploymentTimeZone,
        selection: ReadSelection<'_>,
    ) -> StoreResult<BTreeMap<(DateTime<Utc>, String), MetricState>> {
        let ReadSelection {
            distributions: read_distributions,
            costs: read_costs,
            keys: selected_keys,
            counters,
            distribution_names,
        } = selection;
        filter.validate()?;
        self.require_visible(Some(range))?;
        let selected = collection_for(filter, dimension);
        let boundaries = if granularity == Some(ObservationGranularity::Day) {
            Some(calendar_buckets(range, timezone)?)
        } else {
            None
        };
        // 非整桶日界和短区间不能用完整桶替代，统一在窄事实上执行精确口径
        let selected = if boundaries.as_ref().is_some_and(|values| {
            values
                .iter()
                .any(|value| value.timestamp().rem_euclid(900) != 0)
        }) {
            None
        } else {
            selected
        };
        let mut result = BTreeMap::<(DateTime<Utc>, String), MetricState>::new();
        if let Some(collection) = selected {
            self.approximate |= read_distributions;
            let use_hourly = granularity != Some(ObservationGranularity::FifteenMinutes)
                && boundaries.as_ref().is_none_or(|values| {
                    values
                        .iter()
                        .all(|value| value.timestamp().rem_euclid(3600) == 0)
                });
            let (hours, quarters) = self.summary_buckets(range, use_hourly).await?;
            for (table, seconds, buckets) in [
                ("request_analytics_1h", 3600, hours),
                ("request_analytics_15m", 900, quarters),
            ] {
                if buckets.is_empty() {
                    continue;
                }
                let mut query =
                    QueryBuilder::<Postgres>::new("with matching as not materialized (select ");
                aggregate::push_output_bucket(
                    &mut query,
                    "bucket_start",
                    range,
                    granularity,
                    boundaries.as_deref(),
                );
                let name = aggregate::summary_dimension_sql(collection, dimension);
                let costs = if read_costs {
                    "costs"
                } else {
                    "'{}'::jsonb as costs"
                };
                let distributions = if read_distributions {
                    "distributions"
                } else {
                    "'{}'::jsonb as distributions"
                };
                query.push(format!(" as output_bucket,{name} as name,counts,{costs},{distributions},last_used_at from {table} a where collection="))
                    .push_bind(collection).push(" and schema_version=").push_bind(SCHEMA_VERSION)
                    .push(" and histogram_version=").push_bind(HISTOGRAM_VERSION);
                push_summary_buckets(&mut query, &buckets, seconds);
                push_collection_filter(&mut query, collection, filter);
                if let Some(keys) = selected_keys {
                    query
                        .push(" and dimensions[1]=any(")
                        .push_bind(keys)
                        .push(")");
                }
                if matches!(
                    dimension,
                    Some(
                        DiagnosticDimension::Model
                            | DiagnosticDimension::Status
                            | DiagnosticDimension::Failure
                    )
                ) {
                    query.push(format!(" and ({name})<>''"));
                }
                query.push(")");
                aggregate::push_summary_aggregation(
                    &mut query,
                    read_distributions,
                    read_costs,
                    counters,
                    distribution_names,
                );
                let mut rows = query.build().fetch(&mut *self.tx);
                while let Some(row) = rows
                    .try_next()
                    .await
                    .map_err(|error| postgres_unavailable("merge analytics summaries", error))?
                {
                    let bucket = get(&row, "bucket_start")?;
                    let name = get(&row, "name")?;
                    let state = MetricState::decode(&row)?;
                    result.entry((bucket, name)).or_default().merge(&state)?;
                }
            }
        }
        let spans = if selected.is_some() {
            self.uncovered_spans(range).await?
        } else {
            vec![(range.start, range.end)]
        };
        if spans.is_empty() {
            return Ok(result);
        }
        // 单段直接绑定边界，多个互斥区间用索引读取，所有统计都在数据库内完成
        let single_span = spans.len() == 1;
        let mut query = QueryBuilder::<Postgres>::new("with matching as not materialized (");
        if !single_span {
            let (starts, ends): (Vec<_>, Vec<_>) = spans.iter().copied().unzip();
            query
                .push("select fact.* from unnest(")
                .push_bind(starts)
                .push("::timestamptz[],")
                .push_bind(ends)
                .push("::timestamptz[]) span(start_at,end_at) cross join lateral (");
        }
        query.push(format!(
            "select mr.*,coalesce(({}),false) as is_completed_usage,",
            completed_usage_fact_predicate("mr")
        ));
        aggregate::push_output_bucket(
            &mut query,
            "mr.started_at",
            range,
            granularity,
            boundaries.as_deref(),
        );
        query.push(format!(
            " as output_bucket,{} as name from request_analytics_facts mr where ",
            aggregate::fact_dimension_sql(dimension)
        ));
        if single_span {
            query
                .push("mr.started_at >= ")
                .push_bind(spans[0].0)
                .push(" and mr.started_at < ")
                .push_bind(spans[0].1);
        } else {
            query.push("mr.started_at >= span.start_at and mr.started_at < span.end_at");
        }
        push_unrecovered_request_filter(&mut query, "mr");
        push_analytics_filter(&mut query, filter, "mr");
        if let Some(dimension) = dimension {
            push_diagnostic_dimension_filter(&mut query, dimension);
        }
        if let Some(keys) = selected_keys {
            let column = if dimension == Some(DiagnosticDimension::Account) {
                "provider_account_ref"
            } else {
                "client_api_key_ref"
            };
            query
                .push(format!(" and (mr.{column}=any("))
                .push_bind(keys)
                .push(")");
            if keys.iter().any(String::is_empty) {
                query.push(format!(" or mr.{column} is null"));
            }
            query.push(")");
        }
        if !single_span {
            // 保留逐区间的参数化索引扫描，避免展开 LATERAL 后全表读取再过滤
            // OFFSET 0 不限制返回行数
            query.push(" offset 0) fact");
        }
        query.push(")");
        let exact = selected.is_none();
        aggregate::push_fact_aggregation_selected(
            &mut query,
            exact,
            read_costs,
            read_distributions,
            counters,
            distribution_names,
        );
        let mut rows = query.build().fetch(&mut *self.tx);
        while let Some(row) = rows
            .try_next()
            .await
            .map_err(|error| postgres_unavailable("aggregate analytics facts", error))?
        {
            let key = (get(&row, "bucket_start")?, get(&row, "name")?);
            let state = MetricState::decode_aggregate(&row, exact)?;
            if exact {
                result.insert(key, state);
            } else {
                result.entry(key).or_default().merge(&state)?;
            }
        }
        Ok(result)
    }
}

pub(super) fn push_summary_buckets(
    query: &mut QueryBuilder<Postgres>,
    buckets: &[DateTime<Utc>],
    seconds: i64,
) {
    let (Some(first), Some(last)) = (buckets.first(), buckets.last()) else {
        query.push(" and false");
        return;
    };
    query
        .push(" and bucket_start >= ")
        .push_bind(*first)
        .push(" and bucket_start <= ")
        .push_bind(*last);
    // 连续干净区间直接范围扫描；只有被 dirty 桶分割时才追加集合过滤
    if buckets
        .windows(2)
        .any(|pair| pair[1] - pair[0] != TimeDelta::seconds(seconds))
    {
        query
            .push(" and bucket_start=any(")
            .push_bind(buckets.to_vec())
            .push(")");
    }
}

fn metric_points(
    range: ObservabilityRange,
    granularity: ObservationGranularity,
    timezone: gateway_core::time::DeploymentTimeZone,
    states: &BTreeMap<(DateTime<Utc>, String), MetricState>,
    include_costs: bool,
) -> StoreResult<Vec<RequestMetricPoint>> {
    let mut points = BTreeMap::new();
    for ((bucket, _), state) in states {
        points.insert(
            *bucket,
            RequestMetricPoint {
                bucket_start: *bucket,
                granularity,
                metrics: state.request_metrics()?,
                cost_coverage: state.coverage(),
                costs: if include_costs {
                    state.costs()?
                } else {
                    Vec::new()
                },
            },
        );
    }
    fill_metric_gaps(range, granularity, points, timezone)
}

fn merge_states(states: impl Iterator<Item = MetricState>) -> StoreResult<MetricState> {
    let mut result: Option<MetricState> = None;
    for state in states {
        if let Some(total) = &mut result {
            total.merge(&state)?;
        } else {
            result = Some(state);
        }
    }
    Ok(result.unwrap_or_default())
}

pub(super) fn collection_for(
    filter: &UsageRecordFilter,
    dimension: Option<DiagnosticDimension>,
) -> Option<&'static str> {
    if filter.request_id.is_some()
        || filter.attempt_index.is_some()
        || filter.response_id.is_some()
        || filter.upstream_request_id.is_some()
        || filter.search.is_some()
    {
        return None;
    }
    let mut required = BTreeSet::new();
    if filter.provider_kind.is_some() || filter.model.is_some() {
        required.insert("provider_model");
    }
    if filter.provider_account_ref.is_some() {
        required.insert("account");
    }
    if filter.client_api_key_ref.is_some() {
        required.insert("key");
    }
    if filter.operation.is_some() {
        required.insert("operation");
    }
    if filter.transport.is_some() {
        required.insert("transport");
    }
    if filter.status_code.is_some() {
        required.insert("status");
    }
    if let Some(dimension) = dimension {
        required.insert(match dimension {
            DiagnosticDimension::Provider | DiagnosticDimension::Model => "provider_model",
            DiagnosticDimension::Account => "account",
            DiagnosticDimension::ApiKey => "key",
            DiagnosticDimension::Transport => "transport",
            DiagnosticDimension::Failure => "failure",
            DiagnosticDimension::Status => "status",
        });
    }
    if required.len() == 2 && required.contains("provider_model") && required.contains("account") {
        Some("account_model")
    } else if required.len() == 2 && required.contains("provider_model") && required.contains("key")
    {
        Some("key_model")
    } else if required.len() > 1 {
        None
    } else {
        Some(required.first().copied().unwrap_or("global"))
    }
}

pub(crate) fn push_analytics_filter(
    query: &mut QueryBuilder<Postgres>,
    filter: &UsageRecordFilter,
    alias: &str,
) {
    let mut typed = filter.clone();
    typed.search = None;
    push_usage_filter(query, &typed, alias);
    if filter.search.is_some() {
        query.push(format!(" and exists(select 1 from model_request_observations searched where searched.id={alias}.id"));
        let text = UsageRecordFilter {
            search: filter.search.clone(),
            ..Default::default()
        };
        push_usage_filter(query, &text, "searched");
        query.push(")");
    }
}

pub(super) fn push_collection_filter(
    query: &mut QueryBuilder<Postgres>,
    collection: &str,
    filter: &UsageRecordFilter,
) {
    let (dimension_filter, either_filter, either_positions) = match collection {
        "provider_model" => (filter.provider_kind.clone(), filter.model.clone(), (2, 3)),
        "account_model" => (
            filter.provider_account_ref.clone(),
            filter.model.clone(),
            (3, 4),
        ),
        "key_model" => (
            filter.client_api_key_ref.clone(),
            filter.model.clone(),
            (3, 4),
        ),
        "account" => (filter.provider_account_ref.clone(), None, (1, 2)),
        "key" => (filter.client_api_key_ref.clone(), None, (1, 2)),
        "operation" => (filter.operation.clone(), None, (1, 2)),
        "transport" => (None, filter.transport.clone(), (1, 2)),
        "status" => (
            None,
            filter.status_code.map(|value| value.to_string()),
            (1, 2),
        ),
        _ => (None, None, (1, 2)),
    };
    // 可选筛选仅在有值时生成谓词，保留维度与时间索引的选择空间
    if let Some(outcome) = &filter.outcome {
        query.push(" and outcome=").push_bind(outcome);
    }
    if let Some(value) = dimension_filter {
        query.push(" and dimensions[1]=").push_bind(value);
    }
    if matches!(collection, "account_model" | "key_model")
        && let Some(provider) = &filter.provider_kind
    {
        query.push(" and dimensions[2]=").push_bind(provider);
    }
    if let Some(value) = either_filter {
        query
            .push(format!(" and (dimensions[{}]=", either_positions.0))
            .push_bind(value.clone())
            .push(format!(" or dimensions[{}]=", either_positions.1))
            .push_bind(value)
            .push(")");
    }
}
