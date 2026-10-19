//! 有界增量投影、历史回填与时间桶覆盖重建

use super::buckets::{BucketProgress, pending_batch, rebuild_bucket_batch};
use super::reset::reset_obsolete_batch;
use super::*;
use gateway_admin::ports::store::UsageBillingProjector;
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct AnalyticsMaintenance {
    pub projected_requests: u64,
    pub rebuilt_buckets: u64,
    pub advanced_buckets: u64,
    pub backfilled_requests: u64,
    pub reset_progress: bool,
}

#[derive(Debug, Clone)]
pub struct AnalyticsStatus {
    pub coverage_start: Option<DateTime<Utc>>,
    pub coverage_end: Option<DateTime<Utc>>,
    pub oldest_pending_at: Option<DateTime<Utc>>,
    pub building: bool,
    pub generation: i64,
    pub pending_requests: i64,
    pub pending_buckets: i64,
    pub schema_version: u32,
    pub histogram_version: u32,
}

#[derive(Clone)]
pub struct PgRequestAnalyticsRepository {
    pool: PgPool,
    query_budget: ObservabilityQueryBudget,
    billing_projector: Option<Arc<dyn UsageBillingProjector>>,
}

impl PgRequestAnalyticsRepository {
    #[must_use]
    pub fn new(pool: PgPool, query_budget: ObservabilityQueryBudget) -> Self {
        Self {
            pool,
            query_budget,
            billing_projector: None,
        }
    }

    #[must_use]
    pub fn with_billing_projector(mut self, projector: Arc<dyn UsageBillingProjector>) -> Self {
        self.billing_projector = Some(projector);
        self
    }

    pub async fn status(&self) -> StoreResult<AnalyticsStatus> {
        self.query_budget
            .run(
                &self.pool,
                "analytics status",
                |mut connection| async move {
                    let mut transaction = begin_snapshot(&mut connection).await?;
                    let status = status(&mut transaction).await?;
                    transaction.commit().await.map_err(|error| {
                        postgres_unavailable("complete analytics status", error)
                    })?;
                    Ok(status)
                },
            )
            .await
    }

    pub async fn maintain(&self, batch_size: u32) -> StoreResult<AnalyticsMaintenance> {
        if batch_size == 0 || batch_size > 10_000 {
            return Err(invalid("analytics batch size must be between 1 and 10000"));
        }
        self.query_budget.run(&self.pool, "maintain request analytics", |mut connection| async move {
            // 数据库事务锁同时约束单副本内重复任务与过期 Worker，源写入不获取此锁
            let mut transaction = begin_snapshot(&mut connection).await?;
            let acquired: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended(current_schema()::text,382032))")
                .fetch_one(&mut *transaction).await.map_err(|error| postgres_unavailable("claim analytics maintenance", error))?;
            if !acquired { return Ok(AnalyticsMaintenance::default()); }
            if reset_obsolete_batch(&mut transaction, batch_size).await? {
                transaction.commit().await.map_err(|error| postgres_unavailable("save analytics reset progress", error))?;
                return Ok(AnalyticsMaintenance { reset_progress: true, ..Default::default() });
            }
            let mut result = AnalyticsMaintenance::default();
            // 历史删除可能一次排入大量旧任务，给仍存在的请求预留半批名额，避免实时统计饥饿
            // 其余名额仍按最早入队消费，两类任务都能前进；同一快照内选版本，沿用精确版本 ACK
            let rows = sqlx::query("with live as materialized (
                select pending.request_id, pending.change_version from request_analytics_pending_requests pending
                join model_requests source on source.id=pending.request_id
                order by pending.oldest_pending_at, pending.request_id limit ($1::bigint / 2)
            ), oldest as (
                select pending.request_id, pending.change_version from request_analytics_pending_requests pending
                where not exists (select 1 from live where live.request_id=pending.request_id)
                order by pending.oldest_pending_at, pending.request_id limit ($1::bigint - (select count(*) from live))
            ) select request_id, change_version from live union all select request_id, change_version from oldest")
                .bind(i64::from(batch_size)).fetch_all(&mut *transaction).await.map_err(|error| postgres_unavailable("load analytics request queue", error))?;
            let mut jobs = Vec::with_capacity(rows.len());
            for row in rows {
                jobs.push(ProjectionJob { id: get(&row,"request_id")?, version: get(&row,"change_version")? });
            }
            self.project_batch(&mut transaction, &jobs).await?;
            result.projected_requests = jobs.len() as u64;
            result.backfilled_requests = self.backfill(&mut transaction, batch_size).await?;
            let ids = jobs.iter().map(|job|job.id.clone()).collect::<Vec<_>>();
            let versions = jobs.iter().map(|job|job.version).collect::<Vec<_>>();
            if result.projected_requests + result.backfilled_requests > 0 {
                publish_generation(&mut transaction, true).await?;
            }
            transaction.commit().await.map_err(|error| postgres_unavailable("publish analytics maintenance", error))?;
            // 快照后提交的源版本会让 RR 下的行锁直接序列化失败，持续写入时整批投影会反复回滚
            // ACK 在投影提交后使用 READ COMMITTED，行锁按最新提交版本重新比较，已改写的版本不再匹配
            let mut transaction = connection.begin().await.map_err(|error| postgres_unavailable("begin analytics acknowledgement", error))?;
            sqlx::query("set local statement_timeout = '10s'").execute(&mut *transaction).await.map_err(|error| postgres_unavailable("set analytics query deadline", error))?;
            // ACK 跳过源事务持有的队列行，避免等待并发更新或删除
            // 被跳过的版本保留在持久队列中重放，只有精确匹配且已取得行锁的版本才确认
            sqlx::query("with acknowledged as materialized (select pending.request_id,pending.change_version from request_analytics_pending_requests pending join unnest($1::text[],$2::bigint[]) job(id,version) on pending.request_id=job.id and pending.change_version=job.version for update of pending skip locked) delete from request_analytics_pending_requests pending using acknowledged where pending.request_id=acknowledged.request_id and pending.change_version=acknowledged.change_version")
                .bind(ids).bind(versions).execute(&mut *transaction).await.map_err(|error| postgres_unavailable("confirm analytics request versions", error))?;
            transaction.commit().await.map_err(|error| postgres_unavailable("complete analytics acknowledgement", error))?;
            // 每个阶段使用独立短事务，聚合只锁自己的队列，不把读放大传递到请求终结
            let mut transaction = begin_snapshot(&mut connection).await?;
            let acquired: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended(current_schema()::text,382032))")
                .fetch_one(&mut *transaction).await.map_err(|error| postgres_unavailable("claim analytics bucket maintenance", error))?;
            if !acquired { return Ok(result); }
            let reset_in_progress: bool = sqlx::query_scalar("select reset_phase <> 0 or schema_version <> $1 or histogram_version <> $2 from request_analytics_build_state where singleton")
                .bind(SCHEMA_VERSION).bind(HISTOGRAM_VERSION).fetch_one(&mut *transaction).await.map_err(|error|postgres_unavailable("check analytics rebuild epoch",error))?;
            if reset_in_progress { return Ok(result); }
            for bucket in pending_batch(&mut transaction).await? {
                match rebuild_bucket_batch(&mut transaction,bucket,batch_size).await? {
                    BucketProgress::Waiting => {},
                    BucketProgress::Advanced => result.advanced_buckets += 1,
                    BucketProgress::Completed(count) => { result.advanced_buckets += 1; result.rebuilt_buckets += count; },
                }
            }
            if result.advanced_buckets > 0 { publish_generation(&mut transaction,false).await?; }
            transaction.commit().await.map_err(|error| postgres_unavailable("publish analytics buckets",error))?;
            Ok(result)
        }).await
    }

    async fn project_batch(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        jobs: &[ProjectionJob],
    ) -> StoreResult<()> {
        if jobs.is_empty() {
            return Ok(());
        }
        let ids = jobs.iter().map(|job| job.id.clone()).collect::<Vec<_>>();
        let versions = jobs.iter().map(|job| job.version).collect::<Vec<_>>();
        let previous = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{} where mr.id=any($1::text[])",
            fact_select()
        )))
        .bind(&ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| postgres_unavailable("read previous analytics contributions", error))?;
        sqlx::query("delete from request_analytics_facts where id=any($1::text[])")
            .bind(&ids)
            .execute(&mut **tx)
            .await
            .map_err(|error| postgres_unavailable("replace analytics facts", error))?;
        sqlx::query(PROJECT_REQUEST)
            .bind(&ids)
            .bind(versions)
            .execute(&mut **tx)
            .await
            .map_err(|error| postgres_unavailable("project analytics facts", error))?;
        self.project_billing_batch(tx, &ids).await?;
        let current = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{} where mr.id=any($1::text[])",
            fact_select()
        )))
        .bind(&ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| postgres_unavailable("read current analytics contributions", error))?;
        // 事实、已发布汇总与构建前缀在同一事务中撤销和添加，重试不会重复计数
        super::incremental::update_summaries(tx, &previous, &current).await?;
        Ok(())
    }

    async fn backfill(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        batch_size: u32,
    ) -> StoreResult<u64> {
        let state = sqlx::query("select backfill_complete, backfill_cursor_started_at, backfill_cursor_id from request_analytics_build_state where singleton")
            .fetch_one(&mut **tx).await.map_err(|error| postgres_unavailable("load analytics backfill cursor", error))?;
        if get::<bool>(&state, "backfill_complete")? {
            return Ok(0);
        }
        let mut query = QueryBuilder::<Postgres>::new(
            "select id,started_at,nextval('request_analytics_change_version') as version,exists(select 1 from request_analytics_facts f where f.id=model_requests.id) as projected from model_requests",
        );
        // 首批与后续批次使用两种明确的计划，避免可空游标的 OR 使通用计划退化为全索引过滤
        if let Some(start) = get::<Option<DateTime<Utc>>>(&state, "backfill_cursor_started_at")? {
            let id = get::<Option<String>>(&state, "backfill_cursor_id")?
                .ok_or_else(|| invalid("incomplete analytics backfill cursor"))?;
            query
                .push(" where (started_at,id)<(")
                .push_bind(start)
                .push(",")
                .push_bind(id)
                .push(")");
        }
        query
            .push(" order by started_at desc,id desc limit ")
            .push_bind(i64::from(batch_size));
        let rows = query
            .build()
            .fetch_all(&mut **tx)
            .await
            .map_err(|error| postgres_unavailable("read analytics backfill batch", error))?;
        let mut jobs = Vec::with_capacity(rows.len());
        for row in &rows {
            if get::<bool>(row, "projected")? {
                continue;
            }
            jobs.push(ProjectionJob {
                id: get(row, "id")?,
                version: get(row, "version")?,
            });
        }
        self.project_batch(tx, &jobs).await?;
        if let Some(last) = rows.last() {
            sqlx::query("update request_analytics_build_state set backfill_cursor_started_at = $1, backfill_cursor_id = $2, coverage_start = $1 + interval '1 microsecond' where singleton")
                .bind(get::<DateTime<Utc>>(last,"started_at")?).bind(get::<String>(last,"id")?).execute(&mut **tx).await.map_err(|error| postgres_unavailable("save analytics backfill cursor", error))?;
        }
        if rows.len() < usize::try_from(batch_size).unwrap_or(usize::MAX) {
            sqlx::query("update request_analytics_build_state set backfill_complete = true, coverage_end = clock_timestamp(), coverage_start = (select started_at from request_analytics_facts order by started_at,id limit 1) where singleton")
                .execute(&mut **tx).await.map_err(|error| postgres_unavailable("complete analytics backfill", error))?;
        }
        Ok(rows.len() as u64)
    }

    async fn project_billing_batch(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        ids: &[String],
    ) -> StoreResult<()> {
        let fact = completed_usage_fact_predicate("mr");
        let rows=sqlx::query(sqlx::AssertSqlSafe(format!("select mr.id, mr.billing_snapshot_json, mr.started_at as bucket_start, mr.provider_kind, mr.upstream_model_id, mr.service_tier, mr.input_tokens, mr.output_tokens, mr.cached_input_tokens, mr.cache_write_input_tokens, mr.cost_amount::text as amount, mr.cost_currency from model_request_observations mr where mr.id=any($1::text[]) and {fact} and mr.cost_source='calculated' and mr.cost_amount is not null and mr.cost_currency is not null and nullif(mr.provider_kind,'') is not null and nullif(mr.upstream_model_id,'') is not null")))
            .bind(ids).fetch_all(&mut **tx).await.map_err(|error| postgres_unavailable("read analytics billing source",error))?;
        if rows.is_empty() {
            return Ok(());
        }
        let projector = self
            .billing_projector
            .as_ref()
            .ok_or_else(|| invalid("analytics billing projector is not configured"))?;
        let mut request_ids = Vec::with_capacity(rows.len());
        let mut standard = Vec::with_capacity(rows.len());
        let mut no_cache = Vec::with_capacity(rows.len());
        for row in rows {
            let fact =
                admin_calculated_usage_billing_fact(calculated_usage_billing_fact_from_row(&row)?);
            let projection = projector
                .project(&fact)
                .map_err(|error| postgres_unavailable("project analytics billing", error))?;
            request_ids.push(get::<String>(&row, "id")?);
            standard.push(projection.standard_amount.map(|value| value.to_string()));
            no_cache.push(projection.no_cache_amount.map(|value| value.to_string()));
        }
        sqlx::query("update request_analytics_facts facts set standard_cost_amount=projection.standard::numeric,no_cache_cost_amount=projection.no_cache::numeric from unnest($1::text[],$2::text[],$3::text[]) projection(id,standard,no_cache) where facts.id=projection.id")
            .bind(request_ids).bind(standard).bind(no_cache).execute(&mut **tx).await.map_err(|error| postgres_unavailable("persist analytics billing projections",error))?;
        Ok(())
    }
}

pub(super) async fn status(
    tx: &mut sqlx::Transaction<'_, Postgres>,
) -> StoreResult<AnalyticsStatus> {
    read_status(tx, true).await
}

pub(super) async fn query_status(
    tx: &mut sqlx::Transaction<'_, Postgres>,
) -> StoreResult<AnalyticsStatus> {
    read_status(tx, false).await
}

async fn read_status(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    count_pending: bool,
) -> StoreResult<AnalyticsStatus> {
    // 在线元信息仅使用最旧待处理时间；精确队列长度留给后台运维状态查询
    let row = sqlx::query("select *, case when $1 then (select count(*) from request_analytics_pending_requests) else 0 end as pending_requests, case when $1 then (select count(*) from request_analytics_pending_buckets) else 0 end as pending_buckets, (select min(oldest_pending_at) from (select min(oldest_pending_at) as oldest_pending_at from request_analytics_pending_requests union all select min(oldest_pending_at) from request_analytics_pending_buckets) pending) as oldest_pending_at from request_analytics_build_state where singleton")
        .bind(count_pending).fetch_one(&mut **tx).await.map_err(|error| postgres_unavailable("read analytics status", error))?;
    Ok(AnalyticsStatus {
        coverage_start: get(&row, "coverage_start")?,
        coverage_end: get(&row, "coverage_end")?,
        oldest_pending_at: get(&row, "oldest_pending_at")?,
        building: !get::<bool>(&row, "backfill_complete")?
            || get::<i16>(&row, "reset_phase")? != 0
            || get::<i32>(&row, "schema_version")? != SCHEMA_VERSION
            || get::<i32>(&row, "histogram_version")? != HISTOGRAM_VERSION,
        generation: get(&row, "generation")?,
        pending_requests: get(&row, "pending_requests")?,
        pending_buckets: get(&row, "pending_buckets")?,
        schema_version: u32::try_from(get::<i32>(&row, "schema_version")?)
            .map_err(|_| invalid("invalid analytics version"))?,
        histogram_version: u32::try_from(get::<i32>(&row, "histogram_version")?)
            .map_err(|_| invalid("invalid histogram version"))?,
    })
}

// 投影与极值重算共用同一维度顺序，避免两条路径对集合身份作不同解释
pub(super) const COLLECTION_COLUMNS: &[(&str, &[&str])] = &[
    ("global", &[]),
    (
        "provider_model",
        &["provider_kind", "requested_model_id", "upstream_model_id"],
    ),
    ("account", &["provider_account_ref"]),
    ("key", &["client_api_key_ref"]),
    (
        "account_model",
        &[
            "provider_account_ref",
            "provider_kind",
            "requested_model_id",
            "upstream_model_id",
        ],
    ),
    (
        "key_model",
        &[
            "client_api_key_ref",
            "provider_kind",
            "requested_model_id",
            "upstream_model_id",
        ],
    ),
    ("operation", &["operation"]),
    ("transport", &["client_transport", "upstream_transport"]),
    ("status", &["client_status_code", "upstream_status_code"]),
    ("failure", &["error_kind"]),
];

pub(super) fn collections(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<Vec<(&'static str, Vec<String>)>> {
    let mut result = vec![(forecast::COLLECTION, forecast::dimensions(row)?)];
    // 预测保留原始完成状态，包括被恢复的请求；其他统计继续排除恢复记录
    if get::<Option<DateTime<Utc>>>(row, "recovered_at")?.is_some() {
        return Ok(result);
    }
    for &(collection, columns) in COLLECTION_COLUMNS {
        let dimensions = columns
            .iter()
            .map(|column| {
                if matches!(*column, "client_status_code" | "upstream_status_code") {
                    Ok(get::<Option<i32>>(row, column)?
                        .map(|value| value.to_string())
                        .unwrap_or_default())
                } else {
                    Ok(get::<Option<String>>(row, column)?.unwrap_or_default())
                }
            })
            .collect::<StoreResult<Vec<_>>>()?;
        result.push((collection, dimensions));
    }
    Ok(result)
}

struct ProjectionJob {
    id: String,
    version: i64,
}

async fn publish_generation(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    facts_changed: bool,
) -> StoreResult<()> {
    sqlx::query("update request_analytics_build_state set generation=generation+1,updated_at=clock_timestamp(),coverage_end=clock_timestamp(),coverage_start=case when not backfill_complete then backfill_cursor_started_at + interval '1 microsecond' when $1 then (select started_at from request_analytics_facts order by started_at,id limit 1) else coverage_start end where singleton")
        .bind(facts_changed).execute(&mut **tx).await.map_err(|error|postgres_unavailable("advance analytics generation",error))?;
    Ok(())
}

const PROJECT_REQUEST: &str = "insert into request_analytics_facts (id, started_at, completed_at, client_api_key_ref, provider_account_ref, routing_group_refs, operation, provider_kind, requested_model_id, upstream_model_id, outcome, client_status_code, upstream_status_code, client_transport, upstream_transport, attempt_count, client_response_id, upstream_response_id, upstream_request_id, recovered_at, downstream_committed_at, request_kind, image_generation_requested, image_generation_succeeded, error_kind, input_tokens, output_tokens, cached_input_tokens, cache_write_input_tokens, reasoning_output_tokens, image_input_tokens, image_output_tokens, total_tokens, cost_source, cost_currency, cost_amount, first_token_ms, latency_ms, admission_decision_ms, account_selection_wait_ms, upstream_response_ms, capacity_used_slots, capacity_total_slots, source_version, ops_failure_count, ops_rate_limited_count, ops_auth_failure_count, ops_provider_5xx_count) select mr.id, mr.started_at, mr.completed_at, mr.client_api_key_ref, mr.provider_account_ref, array(select distinct group_ref from unnest(mr.routing_group_refs) group_ref order by group_ref), mr.operation, mr.provider_kind, mr.requested_model_id, mr.upstream_model_id, mr.outcome, mr.client_status_code, mr.upstream_status_code, mr.client_transport, mr.upstream_transport, mr.attempt_count, mr.client_response_id, mr.upstream_response_id, mr.upstream_request_id, mr.recovered_at, mr.downstream_committed_at, mr.request_kind, mr.image_generation_requested, mr.image_generation_succeeded, mr.error_kind, mr.input_tokens, mr.output_tokens, mr.cached_input_tokens, mr.cache_write_input_tokens, mr.reasoning_output_tokens, mr.image_input_tokens, mr.image_output_tokens, mr.total_tokens, mr.cost_source, mr.cost_currency, mr.cost_amount, mr.first_token_ms, mr.latency_ms, mr.admission_decision_ms, mr.account_selection_wait_ms, mr.upstream_response_ms, mr.capacity_used_slots, mr.capacity_total_slots, job.version, ops.failure_count, ops.rate_limited_count, ops.auth_failure_count, ops.provider_5xx_count from unnest($1::text[],$2::bigint[]) job(id,version) join model_request_observations mr on mr.id=job.id cross join lateral (select count(*)::bigint as failure_count, count(*) filter(where failure_kind in ('rate_limited','quota_exhausted') or status_code=429)::bigint as rate_limited_count, count(*) filter(where failure_kind in ('authentication','authorization','invalid_credential') or status_code in (401,403))::bigint as auth_failure_count, count(*) filter(where status_code between 500 and 599)::bigint as provider_5xx_count from ops_events where model_request_id=mr.id) ops returning id, started_at";
