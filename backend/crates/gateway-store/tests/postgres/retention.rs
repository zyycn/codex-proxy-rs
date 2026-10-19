//! 验证短保留期升级、分批清理边界与统计异步收敛期间的查询可用性

use std::{num::NonZeroU32, time::Duration};

use chrono::{Duration as ChronoDuration, Utc};
use gateway_admin::{
    model::retention::{RetentionPolicy, RetentionTarget},
    ports::{retention::RetentionStore as _, store::ObservabilityStore as _},
};
use gateway_store::{
    RequestAnalyticsCycle,
    postgres::{
        ObservabilityQueryBudget, ObservabilityRange, PgRequestAnalyticsRepository,
        PgRetentionRepository,
    },
};

use super::{
    TestDatabase, admin_observability_store, analytics::seed, drain_analytics,
    observability_query_budget, observability_repository,
};

#[tokio::test]
async fn short_retention_keeps_statistics_readable_until_each_deletion_batch_is_projected() {
    use gateway_admin::model::observability::{AnalyticsState, DashboardQuery, TimeRange};

    let Some(database) = TestDatabase::create("short_retention_readable").await else {
        return;
    };
    let now = "2026-10-15T12:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    for id in ["expired_a", "expired_b", "expired_c"] {
        seed(&database.pool, id, now - ChronoDuration::days(8), &[]).await;
        sqlx::query("insert into ops_events(id,model_request_id,attempt_index,level,component,operation,failure_kind,status_code,message,created_at) values($1,$2,1,'warning','routing','responses','rate_limited',429,'synthetic',$3)")
            .bind(format!("event_{id}")).bind(id).bind(now - ChronoDuration::days(8))
            .execute(&database.pool).await.unwrap();
    }
    seed(
        &database.pool,
        "boundary",
        now - ChronoDuration::days(7) - ChronoDuration::minutes(1),
        &[],
    )
    .await;
    seed(
        &database.pool,
        "recent",
        now - ChronoDuration::hours(1),
        &[],
    )
    .await;
    seed(
        &database.pool,
        "running",
        now - ChronoDuration::days(40),
        &[],
    )
    .await;
    sqlx::query("update model_requests set outcome='running',completed_at=null,client_status_code=null,downstream_committed_at=null where id='running'")
        .execute(&database.pool).await.unwrap();
    sqlx::query("update runtime_settings set usage_retention_days=7")
        .execute(&database.pool)
        .await
        .unwrap();
    drain_analytics(&database.pool).await;

    let range = ObservabilityRange::new(now - ChronoDuration::days(41), now).unwrap();
    let statistics = observability_repository(&database.pool);
    let dashboard = admin_observability_store(&database.pool);
    let dashboard_query = DashboardQuery::new(
        TimeRange {
            start: now - ChronoDuration::days(1),
            end: now,
        },
        Default::default(),
    )
    .expect("dashboard query");
    let original = statistics
        .usage_summary(range, Default::default())
        .await
        .unwrap();
    assert_eq!(original.requests.request_count, 6);
    let expired_day_start = (now - ChronoDuration::days(8))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    // 同时验证发布前确实存在各层统计，避免清理断言因未生成汇总而空通过
    const EXPIRED_ANALYTICS_ROWS: &str = "select
        (select count(*) from request_analytics_facts where id in ('expired_a','expired_b','expired_c')),
        (select count(*) from request_analytics_15m where bucket_start >= $1 and bucket_start < $2),
        (select count(*) from request_analytics_1h where bucket_start >= $1 and bucket_start < $2),
        (select count(*) from request_analytics_1d_counts where bucket_start >= $1 and bucket_start < $2)";
    let before: (i64, i64, i64, i64) = sqlx::query_as(EXPIRED_ANALYTICS_ROWS)
        .bind(expired_day_start)
        .bind(expired_day_start + ChronoDuration::days(1))
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(before.0, 3);
    assert!(before.1 > 0 && before.2 > 0 && before.3 > 0);
    let repository = PgRetentionRepository::new(database.pool.clone());
    let policy = repository.load_policy().await.unwrap();
    for expected_deleted in [2, 1, 0] {
        let deleted = repository
            .purge_batch(
                RetentionTarget::ModelRequests,
                now,
                policy,
                NonZeroU32::new(2).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted, expected_deleted);
        let published = statistics
            .usage_summary(range, Default::default())
            .await
            .unwrap();
        assert_eq!(published.requests.request_count, 6);
        assert_eq!(published.attempts.failure_count, 3);
        assert_eq!(
            published.analytics.generation,
            original.analytics.generation
        );
        assert_eq!(published.analytics.state, AnalyticsState::Lagging);
        let snapshot = dashboard
            .dashboard_summary(dashboard_query.clone(), now)
            .await
            .unwrap();
        assert_eq!(snapshot.totals.request_count, 6);
    }
    let retained: Vec<String> = sqlx::query_scalar("select id from model_requests order by id")
        .fetch_all(&database.pool)
        .await
        .unwrap();
    assert_eq!(retained, ["boundary", "recent", "running"]);
    let events: i64 = sqlx::query_scalar("select count(*) from ops_events")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(events, 0);

    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    for remaining_expired in [2, 1, 0] {
        worker.maintain(1).await.unwrap();
        let published = statistics
            .usage_summary(range, Default::default())
            .await
            .unwrap();
        assert_eq!(published.requests.request_count, 3 + remaining_expired);
        assert_eq!(published.attempts.failure_count, remaining_expired);
        assert_eq!(
            published.requests.total_tokens,
            120 * (2 + remaining_expired)
        );
        assert!(published.analytics.generation > original.analytics.generation);
        let snapshot = dashboard
            .dashboard_summary(dashboard_query.clone(), now)
            .await
            .unwrap();
        assert_eq!(snapshot.totals.request_count, 3 + remaining_expired);
    }
    let status = worker.status().await.unwrap();
    assert_eq!(status.pending_requests, 0);
    assert_eq!(status.pending_buckets, 0);
    let after: (i64, i64, i64, i64) = sqlx::query_as(EXPIRED_ANALYTICS_ROWS)
        .bind(expired_day_start)
        .bind(expired_day_start + ChronoDuration::days(1))
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(after, (0, 0, 0, 0));
    assert_eq!(
        statistics
            .usage_summary(range, Default::default())
            .await
            .unwrap()
            .analytics
            .state,
        AnalyticsState::Ready
    );
    database.close().await;
}

#[tokio::test]
async fn retention_batch_deletes_only_expired_rows_up_to_limit() {
    let Some(database) = TestDatabase::create("retention_cycle_budget").await else {
        return;
    };
    let expired_at = Utc::now() - ChronoDuration::days(100);
    sqlx::query(
        "insert into admin_audit_events (
           id, actor_kind, actor_ref, action, entity_kind, entity_ref,
           changed_fields, created_at
         )
         select 'retention-' || value::text, 'system', 'retention', 'cleanup',
                'fixture', 'fixture-' || value::text, array[]::text[],
                case when value = 6 then now() else $1 end
         from generate_series(1, 6) as value",
    )
    .bind(expired_at)
    .execute(&database.pool)
    .await
    .expect("seed expired and retained audit events");

    let repository = PgRetentionRepository::new(database.pool.clone());
    let deleted = repository
        .purge_batch(
            RetentionTarget::AdminAuditEvents,
            Utc::now(),
            RetentionPolicy::try_new(31, 30, 90).unwrap(),
            NonZeroU32::new(2).unwrap(),
        )
        .await
        .expect("bounded retention batch");
    assert_eq!(deleted, 2);
    let remaining: i64 =
        sqlx::query_scalar("select count(*) from admin_audit_events where id like 'retention-%'")
            .fetch_one(&database.pool)
            .await
            .expect("count remaining audit events");
    assert_eq!(remaining, 4);
    let policy = repository.load_policy().await.expect("load valid policy");
    assert_eq!(
        repository
            .purge_batch(
                RetentionTarget::AdminAuditEvents,
                Utc::now(),
                policy,
                NonZeroU32::new(10).unwrap(),
            )
            .await
            .expect("finish expired audit cleanup"),
        3,
    );
    let retained: Vec<String> =
        sqlx::query_scalar("select id from admin_audit_events where id like 'retention-%'")
            .fetch_all(&database.pool)
            .await
            .expect("read retained audit events");
    assert_eq!(retained, ["retention-6"]);
    database.close().await;
}

#[tokio::test]
async fn empty_request_cleanup_does_not_conflict_with_analytics_publication() {
    let Some(database) = TestDatabase::create("retention_empty_analytics").await else {
        return;
    };
    let repository = PgRetentionRepository::new(database.pool.clone());
    let mut publication = database.pool.begin().await.unwrap();
    sqlx::query("set transaction isolation level repeatable read")
        .execute(&mut *publication)
        .await
        .unwrap();
    let generation: i64 =
        sqlx::query_scalar("select generation from request_analytics_build_state where singleton")
            .fetch_one(&mut *publication)
            .await
            .unwrap();
    // 先固定回填事务快照，再让启动时的清理任务执行一次空删除
    let deleted = repository
        .purge_batch(
            RetentionTarget::ModelRequests,
            Utc::now(),
            RetentionPolicy::try_new(31, 30, 90).unwrap(),
            NonZeroU32::new(1000).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted, 0);
    let published: i64 = sqlx::query_scalar(
        "update request_analytics_build_state set backfill_complete=true where singleton returning generation",
    )
    .fetch_one(&mut *publication)
    .await
    .expect("empty cleanup must not invalidate the analytics transaction snapshot");
    assert_eq!(published, generation);
    publication.commit().await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn scattered_retention_deletions_commit_atomically_and_allow_new_requests_to_advance() {
    let Some(database) = TestDatabase::create("retention_scattered_groups").await else {
        return;
    };
    let now = Utc::now();
    for number in 0..600 {
        seed(
            &database.pool,
            &format!("expired_{number:04}"),
            now - ChronoDuration::days(30) + ChronoDuration::minutes(number * 20),
            &[],
        )
        .await;
    }
    drain_analytics(&database.pool).await;
    let worker = PgRequestAnalyticsRepository::new(
        database.pool.clone(),
        ObservabilityQueryBudget::try_new(2, Duration::from_secs(5), Duration::from_secs(120))
            .unwrap(),
    );
    let before = worker.status().await.unwrap();
    let retention = PgRetentionRepository::new(database.pool.clone());
    assert_eq!(
        retention
            .purge_batch(
                RetentionTarget::ModelRequests,
                now,
                RetentionPolicy::try_new(10, 30, 90).unwrap(),
                NonZeroU32::new(1000).unwrap(),
            )
            .await
            .unwrap(),
        600
    );
    seed(
        &database.pool,
        "new_request",
        now - ChronoDuration::minutes(2),
        &[],
    )
    .await;

    // 小时层失败时，已经执行的多个刻钟删除批次与事实删除也必须一起回滚
    sqlx::raw_sql("create function reject_hour_deletion() returns trigger language plpgsql as $$begin raise exception 'test hour deletion failure'; end$$;
        create trigger reject_hour_deletion before delete on request_analytics_1h for each statement execute function reject_hour_deletion()")
        .execute(&database.pool).await.unwrap();
    let cycle = RequestAnalyticsCycle::default();
    assert!(cycle.run(&worker).await.is_err());
    let failed = worker.status().await.unwrap();
    assert_eq!(failed.generation, before.generation);
    assert_eq!(failed.pending_requests, 601);
    let statistics = observability_repository(&database.pool);
    let range = ObservabilityRange::new(now - ChronoDuration::days(31), now).unwrap();
    assert_eq!(
        statistics
            .usage_summary(range, Default::default())
            .await
            .unwrap()
            .requests
            .request_count,
        600
    );
    sqlx::raw_sql("drop trigger reject_hour_deletion on request_analytics_1h; drop function reject_hour_deletion();
        create function slow_summary_statement() returns trigger language plpgsql as $$begin perform pg_sleep(0.002); return null; end$$;
        create trigger slow_quarter_delete before delete on request_analytics_15m for each statement execute function slow_summary_statement();
        create trigger slow_hour_delete before delete on request_analytics_1h for each statement execute function slow_summary_statement()")
        .execute(&database.pool).await.unwrap();

    // 按语句注入延迟，覆盖大量分组的 SQL 往返成本及多个集合删除批次
    cycle
        .run(&worker)
        .await
        .expect("retention backlog must advance within the cycle budget");
    let after = worker.status().await.unwrap();
    assert_eq!(after.pending_requests, 0);
    assert_eq!(after.pending_buckets, 0);
    assert!(after.generation > before.generation);
    let actual = statistics
        .usage_summary(range, Default::default())
        .await
        .unwrap();
    assert_eq!(actual.requests.request_count, 1);
    assert_eq!(actual.requests.total_tokens, 120);
    let timeline = admin_observability_store(&database.pool)
        .dashboard_summary(
            gateway_admin::model::observability::DashboardQuery::new(
                gateway_admin::model::observability::TimeRange {
                    start: now - ChronoDuration::days(1),
                    end: now,
                },
                Default::default(),
            )
            .unwrap(),
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        timeline
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        1
    );
    let remaining: (i64, i64, i64, i64) = sqlx::query_as(
        "select
        (select count(*) from request_analytics_facts where id like 'expired_%'),
        (select count(*) from request_analytics_15m where bucket_start < $1),
        (select count(*) from request_analytics_1h where bucket_start < $1),
        (select count(*) from request_analytics_1d_counts where bucket_start < $1)",
    )
    .bind(now - ChronoDuration::days(10))
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(remaining, (0, 0, 0, 0));
    database.close().await;
}

#[tokio::test]
async fn fresh_requests_and_retention_deletions_both_advance_within_each_batch() {
    let Some(database) = TestDatabase::create("retention_fair_projection").await else {
        return;
    };
    let now = Utc::now();
    for number in 0..20 {
        seed(
            &database.pool,
            &format!("expired_{number:04}"),
            now - ChronoDuration::days(30),
            &[],
        )
        .await;
    }
    drain_analytics(&database.pool).await;
    PgRetentionRepository::new(database.pool.clone())
        .purge_batch(
            RetentionTarget::ModelRequests,
            now,
            RetentionPolicy::try_new(10, 30, 90).unwrap(),
            NonZeroU32::new(1000).unwrap(),
        )
        .await
        .unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    for wave in 1..=3 {
        for number in 0..2 {
            seed(
                &database.pool,
                &format!("new_{wave}_{number}"),
                now - ChronoDuration::minutes(2),
                &[],
            )
            .await;
        }
        assert_eq!(worker.maintain(4).await.unwrap().projected_requests, 4);
        let facts: (i64, i64) = sqlx::query_as(
            "select count(*) filter (where id like 'expired_%'), count(*) filter (where id like 'new_%') from request_analytics_facts",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        // 历史队列尚未排空时，每批都须撤销旧贡献并发布本轮的新请求
        assert_eq!(facts, (20 - wave * 2, wave * 2));
        let current = observability_repository(&database.pool)
            .usage_summary(
                ObservabilityRange::new(now - ChronoDuration::days(1), now).unwrap(),
                Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(current.requests.request_count, wave as u64 * 2);
    }
    drain_analytics(&database.pool).await;
    let final_summary = observability_repository(&database.pool)
        .usage_summary(
            ObservabilityRange::new(now - ChronoDuration::days(31), now).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(final_summary.requests.request_count, 6);
    assert_eq!(final_summary.requests.total_tokens, 720);
    database.close().await;
}

#[tokio::test]
async fn timed_out_retention_batches_shrink_across_cycles_and_lock_contention() {
    let Some(database) = TestDatabase::create("retention_batch_timeout").await else {
        return;
    };
    let now = Utc::now();
    for number in 0..600 {
        seed(
            &database.pool,
            &format!("expired_{number:04}"),
            now - ChronoDuration::days(30),
            &[],
        )
        .await;
    }
    drain_analytics(&database.pool).await;
    sqlx::query("delete from model_requests")
        .execute(&database.pool)
        .await
        .unwrap();
    seed(
        &database.pool,
        "new_request",
        now - ChronoDuration::minutes(2),
        &[],
    )
    .await;
    // 数据库延迟与实际批量相关，小批量无需等待，以可观察进展验证跨轮退让
    sqlx::raw_sql("create function slow_large_fact_deletion() returns trigger language plpgsql as $$begin
        if (select count(*) from removed_facts) > 250 then perform pg_sleep(2); end if;
        return null; end$$;
        create trigger slow_large_fact_deletion after delete on request_analytics_facts referencing old table as removed_facts for each statement execute function slow_large_fact_deletion()")
        .execute(&database.pool).await.unwrap();
    let worker = PgRequestAnalyticsRepository::new(
        database.pool.clone(),
        ObservabilityQueryBudget::try_new(2, Duration::from_secs(1), Duration::from_secs(1))
            .unwrap(),
    );
    let cycle = RequestAnalyticsCycle::default();
    for _ in 0..2 {
        assert!(cycle.run(&worker).await.is_err());
        // 空轮可能来自别的实例持锁，不能因此恢复会反复超时的大批量
        let mut lock = database.pool.begin().await.unwrap();
        sqlx::query(
            "select pg_advisory_xact_lock(hashtextextended(current_schema()::text,382032))",
        )
        .execute(&mut *lock)
        .await
        .unwrap();
        let idle = cycle.run(&worker).await.unwrap();
        assert_eq!(idle.projected_requests, 0);
        lock.rollback().await.unwrap();
    }
    let pending = worker.status().await.unwrap().pending_requests;
    assert_eq!(pending, 601);
    // 此轮可以在后续批次超时前已有提交，下一轮重放仍须保持计数正确
    let _ = cycle.run(&worker).await;
    assert!(worker.status().await.unwrap().pending_requests < pending);
    for _ in 0..5 {
        // 与调度器错误重试一样留出退避时间，让已取消语句完成回滚并释放维护锁
        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ = cycle.run(&worker).await;
        if worker.status().await.unwrap().pending_requests == 0 {
            break;
        }
    }
    assert_eq!(worker.status().await.unwrap().pending_requests, 0);
    let actual = observability_repository(&database.pool)
        .usage_summary(
            ObservabilityRange::new(now - ChronoDuration::days(31), now).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(actual.requests.request_count, 1);
    assert_eq!(actual.requests.total_tokens, 120);
    database.close().await;
}
