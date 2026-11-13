//! 小时重建合并、分段快照失效与阻塞桶调度的真实数据库验收

use chrono::{DateTime, TimeDelta, Utc};
use gateway_store::{
    RequestAnalyticsCycle,
    postgres::{ObservabilityRange, PgRequestAnalyticsRepository},
};

use super::{
    TestDatabase, analytics::seed, drain_analytics, observability_query_budget,
    observability_repository,
};

fn hour() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

async fn seed_hour(pool: &sqlx::PgPool, start: DateTime<Utc>, prefix: &str) {
    for quarter in 0..4 {
        for request in 0..2 {
            let id = format!("{prefix}_{quarter}_{request}");
            seed(
                pool,
                &id,
                start + TimeDelta::minutes(quarter * 15 + request + 1),
                &[],
            )
            .await;
            sqlx::query("update model_requests set provider_account_ref=$1 where id=$1")
                .bind(id)
                .execute(pool)
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn dirty_quarters_share_one_hour_rebuild_and_acknowledge_each_bucket() {
    let Some(database) = TestDatabase::create("analytics_coalesced_hour").await else {
        return;
    };
    sqlx::raw_sql("create table hourly_build_audit (bucket_start timestamptz); create function audit_hourly_build() returns trigger language plpgsql as $$begin if new.collection='global' then insert into hourly_build_audit values(new.bucket_start); end if; return new; end$$; create trigger audit_hourly_build after insert on request_analytics_1h for each row execute function audit_hourly_build()")
        .execute(&database.pool).await.unwrap();
    seed_hour(&database.pool, hour(), "coalesced").await;
    drain_analytics(&database.pool).await;
    sqlx::raw_sql("truncate hourly_build_audit; insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) select distinct date_bin(interval '15 minutes',started_at,timestamptz '2000-01-01 UTC'),now() from request_analytics_facts")
        .execute(&database.pool).await.unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    let mut acknowledged = 0;
    for _ in 0..30 {
        acknowledged += worker.maintain(1000).await.unwrap().rebuilt_buckets;
        let status = worker.status().await.unwrap();
        if !status.building && status.pending_requests == 0 && status.pending_buckets == 0 {
            break;
        }
    }
    assert_eq!(worker.status().await.unwrap().pending_buckets, 0);
    assert_eq!(acknowledged, 4);
    let builds: i64 = sqlx::query_scalar("select count(*) from hourly_build_audit")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        builds, 1,
        "one shared hour must not be rebuilt once per dirty quarter"
    );
    let result = observability_repository(&database.pool)
        .usage_summary(
            ObservabilityRange::new(hour(), hour() + TimeDelta::hours(1)).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(result.requests.request_count, 8);
    assert_eq!(result.requests.total_tokens, 960);
    database.close().await;
}

#[tokio::test]
async fn changed_quarter_invalidates_a_partial_hour_before_its_inputs_are_rebuilt() {
    let Some(database) = TestDatabase::create("analytics_hour_changed_input").await else {
        return;
    };
    seed_hour(&database.pool, hour(), "changed").await;
    drain_analytics(&database.pool).await;
    sqlx::query("update model_requests set total_tokens=130")
        .execute(&database.pool)
        .await
        .unwrap();
    // 显式修复任务重建整桶，普通更新只维护当前贡献，不重新启动构建
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) select distinct date_bin(interval '15 minutes',started_at,timestamptz '2000-01-01 UTC'),now() from request_analytics_facts")
        .execute(&database.pool).await.unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    let mut owner = None;
    for _ in 0..100 {
        worker.maintain(2).await.unwrap();
        owner = sqlx::query_scalar::<_, DateTime<Utc>>("select bucket_start from request_analytics_pending_buckets where build_phase=4 and build_started_at is not null")
            .fetch_optional(&database.pool).await.unwrap();
        if owner.is_some() {
            break;
        }
    }
    let owner = owner.expect("hour must persist a partial merge");
    let changed: String = sqlx::query_scalar("select id from model_requests where date_bin(interval '15 minutes',started_at,timestamptz '2000-01-01 UTC') <> $1 order by id limit 1")
        .bind(owner).fetch_one(&database.pool).await.unwrap();
    sqlx::query("update model_requests set total_tokens=9999 where id=$1")
        .bind(&changed)
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) select date_bin(interval '15 minutes',started_at,timestamptz '2000-01-01 UTC'),now() from model_requests where id=$1 on conflict(bucket_start) do update set change_version=nextval('request_analytics_change_version')")
        .bind(changed).execute(&database.pool).await.unwrap();
    worker.maintain(2).await.unwrap();
    let phase: i16 = sqlx::query_scalar(
        "select build_phase from request_analytics_pending_buckets where bucket_start=$1",
    )
    .bind(owner)
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(
        phase, 2,
        "a changed input invalidates the staged hour before quarter rebuilding"
    );
    let range = ObservabilityRange::new(hour(), hour() + TimeDelta::hours(1)).unwrap();
    let result = observability_repository(&database.pool)
        .usage_summary(range, Default::default())
        .await
        .unwrap();
    assert_eq!(result.requests.request_count, 8);
    assert_eq!(result.requests.total_tokens, 7 * 130 + 9999);
    // 每轮换实例验证所有进度均保存在数据库，小时片段不能依赖进程内快照
    for _ in 0..150 {
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget())
            .maintain(2)
            .await
            .unwrap();
        if worker.status().await.unwrap().pending_buckets == 0 {
            break;
        }
    }
    assert_eq!(worker.status().await.unwrap().pending_buckets, 0);
    let hour_total: i64 = sqlx::query_scalar("select (counts->>'total_tokens')::bigint from request_analytics_1h where collection='global'")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(hour_total, 7 * 130 + 9999);
    database.close().await;
}

#[tokio::test]
async fn waiting_hours_cannot_fill_all_slots_and_starve_their_unbuilt_quarters() {
    let Some(database) = TestDatabase::create("analytics_hour_waiting_slots").await else {
        return;
    };
    for index in 0..9 {
        for quarter in 0..2 {
            seed(
                &database.pool,
                &format!("waiting_{index}_{quarter}"),
                hour() + TimeDelta::hours(index) + TimeDelta::minutes(quarter * 15 + 1),
                &[],
            )
            .await;
        }
    }
    drain_analytics(&database.pool).await;
    // 九个更早的等待项超过单批八个槽位，各自依赖排在后面的 quarter
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) select distinct date_bin(interval '15 minutes',started_at,timestamptz '2000-01-01 UTC'),case when extract(minute from started_at)<15 then now()-interval '1 hour' else now() end from request_analytics_facts")
        .execute(&database.pool).await.unwrap();
    sqlx::query("update request_analytics_pending_buckets set build_phase=2,build_version=change_version where extract(minute from bucket_start)=0")
        .execute(&database.pool).await.unwrap();
    drain_analytics(&database.pool).await;
    let result = observability_repository(&database.pool)
        .usage_summary(
            ObservabilityRange::new(hour(), hour() + TimeDelta::hours(9)).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(result.requests.request_count, 18);
    assert_eq!(result.requests.total_tokens, 18 * 120);
    database.close().await;
}

#[tokio::test]
async fn continuous_updates_do_not_restart_partial_quarter_and_hour_builds() {
    let Some(database) = TestDatabase::create("analytics_continuous_stage_updates").await else {
        return;
    };
    for quarter in 0..2 {
        for name in ["A", "a", "Z", "z", "Ä", "ä", "中", "é", "e", "_", "-", "2"] {
            seed(
                &database.pool,
                &format!("prefix_{name}_{quarter}"),
                hour() + TimeDelta::minutes(quarter * 15 + 1),
                &[],
            )
            .await;
        }
    }
    drain_analytics(&database.pool).await;
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) select distinct date_bin(interval '15 minutes',started_at,timestamptz '2000-01-01 UTC'),now() from request_analytics_facts")
        .execute(&database.pool).await.unwrap();
    let mut quarter_prefix_seen = false;
    let mut hour_prefix_seen = false;
    let mut completed = false;
    for round in 0..150_i64 {
        // 同时间的大小写及非 ASCII ID、改变排序位置的账号维度同时覆盖两种前缀
        sqlx::query("update model_requests set total_tokens=$1,provider_account_ref=$2 where id in ('prefix_A_0','prefix_ä_1')")
            .bind(200+round).bind(if round%2==0 {"account_Ä"}else{"account_Z"})
            .execute(&database.pool).await.unwrap();
        let worker =
            PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
        worker.maintain(2).await.unwrap();
        let phases: (bool,bool,i64) = sqlx::query_as("select coalesce(bool_or(build_phase=1 and build_id is not null),false),coalesce(bool_or(build_phase=4 and build_collection is not null),false),count(*) from request_analytics_pending_buckets")
            .fetch_one(&database.pool).await.unwrap();
        quarter_prefix_seen |= phases.0;
        hour_prefix_seen |= phases.1;
        if phases.2 == 0 {
            completed = true;
            break;
        }
    }
    assert!(
        quarter_prefix_seen && hour_prefix_seen,
        "both persisted prefixes must be exercised"
    );
    assert!(
        completed,
        "ongoing updates cannot repeatedly reset a partial rebuild"
    );
    drain_analytics(&database.pool).await;
    let expected: (i64, i64) =
        sqlx::query_as("select count(*),sum(total_tokens)::bigint from model_requests")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    let actual = observability_repository(&database.pool)
        .usage_summary(
            ObservabilityRange::new(hour(), hour() + TimeDelta::hours(1)).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(actual.requests.request_count, expected.0 as u64);
    assert_eq!(actual.requests.total_tokens, expected.1 as u64);
    database.close().await;
}

#[tokio::test]
async fn incremental_deletions_repair_extrema_last_usage_and_zero_currency_presence() {
    let Some(database) = TestDatabase::create("analytics_incremental_extrema").await else {
        return;
    };
    for number in 1..=4_i64 {
        let id = format!("extrema_{number}");
        seed(
            &database.pool,
            &id,
            hour() + TimeDelta::minutes(number),
            &[],
        )
        .await;
        sqlx::query("update model_requests set provider_account_ref='extrema_account',request_observation_json=jsonb_set(request_observation_json,'{timings,local}',jsonb_build_object('latencyMs',$2::bigint,'firstTokenMs',$2::bigint)),cost_source=case when $2<=2 then 'provider_reported' else 'unavailable' end,cost_currency=case $2 when 1 then 'EUR' when 2 then 'USD' end,cost_amount=case $2 when 1 then 1::numeric when 2 then 0::numeric end where id=$1")
            .bind(id).bind(number).execute(&database.pool).await.unwrap();
    }
    drain_analytics(&database.pool).await;
    sqlx::query("delete from model_requests where id in ('extrema_1','extrema_4')")
        .execute(&database.pool)
        .await
        .unwrap();
    drain_analytics(&database.pool).await;
    let row: (i64,f64,f64,DateTime<Utc>,serde_json::Value) = sqlx::query_as("select (counts->>'request_count')::bigint,(distributions->'latency_ms'->>'minimum')::double precision,(distributions->'latency_ms'->>'maximum')::double precision,last_used_at,costs from request_analytics_1h where collection='global'")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(
        (row.0, row.1, row.2, row.3),
        (2, 2.0, 3.0, hour() + TimeDelta::minutes(3))
    );
    assert_eq!(row.4.as_object().unwrap().len(), 1);
    assert_eq!(
        row.4["USD"]
            .as_str()
            .unwrap()
            .parse::<gateway_core::metering::Decimal>()
            .unwrap(),
        gateway_core::metering::Decimal::ZERO
    );
    sqlx::query("update model_requests set cost_source='unavailable',cost_currency=null,cost_amount=null where id='extrema_2'")
        .execute(&database.pool).await.unwrap();
    drain_analytics(&database.pool).await;
    let costs: serde_json::Value =
        sqlx::query_scalar("select costs from request_analytics_1h where collection='global'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(costs, serde_json::json!({}));
    sqlx::query("select enqueue_request_analytics('extrema_3')")
        .execute(&database.pool)
        .await
        .unwrap();
    drain_analytics(&database.pool).await;
    let actual = observability_repository(&database.pool)
        .usage_summary(
            ObservabilityRange::new(hour(), hour() + TimeDelta::hours(1)).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(actual.requests.request_count, 2);
    assert_eq!(actual.requests.total_tokens, 240);
    assert!(actual.attempts.costs.is_empty());
    database.close().await;
}

#[tokio::test]
async fn projection_work_mem_is_local_to_the_worker_transaction() {
    let Some(database) = TestDatabase::create("work_mem").await else {
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(database.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    // 固定同一会话，直接记录事实写入和队列确认时的值，并在复用连接时检查恢复
    sqlx::raw_sql(
        "set work_mem='2MB';
        create table work_mem_audit(stage text, value text);
        create function record_work_mem() returns trigger language plpgsql as $$
        begin
            insert into work_mem_audit values (tg_argv[0], current_setting('work_mem'));
            return null;
        end $$;
        create trigger audit_fact after insert on request_analytics_facts
            for each row execute function record_work_mem('projection');
        create trigger audit_ack after delete on request_analytics_pending_requests
            for each row execute function record_work_mem('ack')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let worker = PgRequestAnalyticsRepository::new(pool.clone(), observability_query_budget());
    let session = || {
        sqlx::query_as::<_, (i32, String)>("select pg_backend_pid(), current_setting('work_mem')")
    };
    let original = session().fetch_one(&pool).await.unwrap();
    for (worker, expected) in [
        (worker.clone(), "2MB"),
        (worker.with_projection_work_mem_mb(Some(8)), "8MB"),
    ] {
        seed(&pool, expected, hour(), &[]).await;
        assert_eq!(worker.maintain(10).await.unwrap().projected_requests, 1);
        let audit: Vec<(String, String)> =
            sqlx::query_as("select stage, value from work_mem_audit order by stage")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            audit,
            [
                ("ack".into(), "2MB".into()),
                ("projection".into(), expected.into())
            ]
        );
        assert_eq!(session().fetch_one(&pool).await.unwrap(), original);
        sqlx::query("truncate work_mem_audit")
            .execute(&pool)
            .await
            .unwrap();
    }
    // 事实写入失败时投影事务回滚，原会话仍可供下一轮使用
    sqlx::query("alter table work_mem_audit add check (value <> '8MB')")
        .execute(&pool)
        .await
        .unwrap();
    seed(&pool, "rollback", hour(), &[]).await;
    let worker = PgRequestAnalyticsRepository::new(pool.clone(), observability_query_budget())
        .with_projection_work_mem_mb(Some(8));
    let error = worker.maintain(10).await.unwrap_err();
    assert!(
        error.to_string().contains("project analytics facts"),
        "{error}"
    );
    assert_eq!(session().fetch_one(&pool).await.unwrap(), original);
    pool.close().await;
    database.close().await;
}

#[tokio::test]
async fn incremental_summary_write_failure_rolls_back_facts_and_preserves_queue() {
    let Some(database) = TestDatabase::create("analytics_incremental_rollback").await else {
        return;
    };
    seed(
        &database.pool,
        "atomic_incremental",
        hour() + TimeDelta::minutes(1),
        &[],
    )
    .await;
    drain_analytics(&database.pool).await;
    sqlx::raw_sql("create function reject_incremental_summary() returns trigger language plpgsql as $$begin raise exception 'test summary write failure'; end$$; create trigger reject_incremental_summary before insert or update on request_analytics_15m for each row execute function reject_incremental_summary()")
        .execute(&database.pool).await.unwrap();
    sqlx::query("update model_requests set total_tokens=240 where id='atomic_incremental'")
        .execute(&database.pool)
        .await
        .unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    assert!(worker.maintain(10).await.is_err());
    let state:(i64,i64,i64)=sqlx::query_as("select (select total_tokens from request_analytics_facts where id='atomic_incremental'),(select (counts->>'total_tokens')::bigint from request_analytics_15m where collection='global'),(select count(*) from request_analytics_pending_requests)")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(state, (120, 120, 1));
    sqlx::raw_sql("drop trigger reject_incremental_summary on request_analytics_15m; drop function reject_incremental_summary()")
        .execute(&database.pool).await.unwrap();
    drain_analytics(&database.pool).await;
    let actual = observability_repository(&database.pool)
        .usage_summary(
            ObservabilityRange::new(hour(), hour() + TimeDelta::hours(1)).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(actual.requests.total_tokens, 240);
    assert_eq!(actual.requests.request_count, 1);
    database.close().await;
}

#[tokio::test]
async fn extrema_rebuild_reads_only_its_bucket_when_history_is_large() {
    let Some(database) = TestDatabase::create("analytics_rebuild_bucket_range").await else {
        return;
    };
    // 强制刷新只作用于当前连接，Worker 与计数读取必须复用同一条连接
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(database.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    for number in 1..=3_i64 {
        let id = format!("rebuild_range_{number}");
        seed(&pool, &id, hour() + TimeDelta::minutes(number), &[]).await;
        sqlx::query("update model_requests set provider_account_ref='rebuild_range_account',request_observation_json=jsonb_set(request_observation_json,'{timings,local}',jsonb_build_object('latencyMs',$2::bigint,'firstTokenMs',$2::bigint)) where id=$1")
            .bind(id).bind(number).execute(&pool).await.unwrap();
    }
    drain_analytics(&pool).await;
    // 只扩充其他时间桶的事实，规划器拿到真实行数后不能再为一个分组扫描全部历史
    sqlx::query("insert into request_analytics_facts select ('history_'||n)::text as id,(f.started_at-make_interval(hours=>n/1000+2))::timestamptz,f.completed_at-make_interval(hours=>n/1000+2),f.client_api_key_ref,f.provider_account_ref,f.routing_group_refs,f.operation,f.provider_kind,f.requested_model_id,f.upstream_model_id,f.outcome,f.client_status_code,f.upstream_status_code,f.client_transport,f.upstream_transport,f.attempt_count,null,null,null,f.recovered_at,f.downstream_committed_at,f.request_kind,f.image_generation_requested,f.image_generation_succeeded,f.error_kind,f.input_tokens,f.output_tokens,f.cached_input_tokens,f.cache_write_input_tokens,f.reasoning_output_tokens,f.image_input_tokens,f.image_output_tokens,f.total_tokens,f.cost_source,f.cost_currency,f.cost_amount,f.first_token_ms,f.latency_ms,f.admission_decision_ms,f.account_selection_wait_ms,f.upstream_response_ms,f.capacity_used_slots,f.capacity_total_slots,f.standard_cost_amount,f.no_cache_cost_amount,f.source_version,f.schema_version,f.ops_failure_count,f.ops_rate_limited_count,f.ops_auth_failure_count,f.ops_provider_5xx_count from request_analytics_facts f cross join generate_series(1,300000) n where f.id='rebuild_range_2'")
        .execute(&pool).await.unwrap();
    sqlx::query("analyze request_analytics_facts")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("delete from model_requests where id='rebuild_range_1'")
        .execute(&pool)
        .await
        .unwrap();
    let worker = PgRequestAnalyticsRepository::new(pool.clone(), observability_query_budget());
    let scanned_before = sequentially_read_facts(&pool).await;
    RequestAnalyticsCycle::default()
        .run(&worker)
        .await
        .expect("extrema rebuild must finish within the cycle budget");
    // 耗时随机器和数据量变化，用顺序扫描读取的行数判断重建是否越出所在统计桶
    let scanned = sequentially_read_facts(&pool).await - scanned_before;
    assert!(
        scanned < 300_000,
        "extrema rebuild scanned {scanned} fact rows"
    );
    let status = worker.status().await.unwrap();
    assert_eq!((status.pending_requests, status.pending_buckets), (0, 0));
    let row: (i64, f64, f64) = sqlx::query_as("select (counts->>'request_count')::bigint,(distributions->'latency_ms'->>'minimum')::double precision,(distributions->'latency_ms'->>'maximum')::double precision from request_analytics_15m where collection='global' and bucket_start=$1")
        .bind(hour()).fetch_one(&pool).await.unwrap();
    assert_eq!(row, (2, 2.0, 3.0));
    pool.close().await;
    database.close().await;
}

async fn sequentially_read_facts(pool: &sqlx::PgPool) -> i64 {
    sqlx::query("select pg_stat_force_next_flush()")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query_scalar("select coalesce(seq_tup_read,0) from pg_stat_user_tables where relid='request_analytics_facts'::regclass")
        .fetch_one(pool)
        .await
        .unwrap()
}
