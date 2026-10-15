//! 验证短保留期升级、分批清理边界与统计异步收敛期间的查询可用性

use std::num::NonZeroU32;

use chrono::{Duration as ChronoDuration, Utc};
use gateway_admin::{
    model::retention::{RetentionPolicy, RetentionTarget},
    ports::retention::RetentionStore as _,
};
use gateway_store::postgres::{
    ObservabilityRange, PgRequestAnalyticsRepository, PgRetentionRepository,
};

use super::{
    TEST_MIGRATOR, TestDatabase, admin_observability_store, analytics::seed, drain_analytics,
    observability_query_budget, observability_repository,
};

#[tokio::test]
async fn retention_upgrade_caps_request_window_and_preserves_other_settings_and_pending_work() {
    let Some(database) = TestDatabase::create_through("short_retention_upgrade", 31).await else {
        return;
    };
    sqlx::query("update runtime_settings set usage_retention_days=60,ops_event_retention_days=7,audit_retention_days=45")
        .execute(&database.pool)
        .await
        .unwrap();
    let start = Utc::now() - ChronoDuration::days(10);
    seed(&database.pool, "retention_upgrade", start, &[]).await;
    drain_analytics(&database.pool).await;
    sqlx::query("delete from model_requests where id='retention_upgrade'")
        .execute(&database.pool)
        .await
        .unwrap();
    let before: (String, i64, chrono::DateTime<Utc>) = sqlx::query_as(
        "select request_id,change_version,oldest_pending_at from request_analytics_pending_requests",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();

    TEST_MIGRATOR.run(&database.pool).await.unwrap();

    let repository = PgRetentionRepository::new(database.pool.clone());
    assert_eq!(
        repository
            .load_policy()
            .await
            .unwrap()
            .days(RetentionTarget::ModelRequests),
        31
    );
    let policy = repository.load_policy().await.unwrap();
    assert_eq!(policy.days(RetentionTarget::OpsEvents), 7);
    assert_eq!(policy.days(RetentionTarget::AdminAuditEvents), 45);
    let after: (String, i64, chrono::DateTime<Utc>) = sqlx::query_as(
        "select request_id,change_version,oldest_pending_at from request_analytics_pending_requests",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(after, before);
    // 升级时遗留的删除任务继续可读并最终收敛，不重建全量统计
    let range = ObservabilityRange::new(start, start + ChronoDuration::hours(1)).unwrap();
    let statistics = observability_repository(&database.pool);
    assert_eq!(
        statistics
            .usage_summary(range, Default::default())
            .await
            .unwrap()
            .requests
            .request_count,
        1
    );
    drain_analytics(&database.pool).await;
    assert_eq!(
        statistics
            .usage_summary(range, Default::default())
            .await
            .unwrap()
            .requests
            .request_count,
        0
    );
    for days in [1_i64, 3, 7, 15, 30, 31] {
        sqlx::query("update runtime_settings set usage_retention_days=$1")
            .bind(days)
            .execute(&database.pool)
            .await
            .unwrap();
        assert_eq!(
            i64::from(
                repository
                    .load_policy()
                    .await
                    .unwrap()
                    .days(RetentionTarget::ModelRequests)
            ),
            days
        );
    }
    for (usage, ops, audit) in [
        (0_i64, 30_i64, 90_i64),
        (32, 30, 90),
        (1, 0, 90),
        (1, 30, 0),
    ] {
        let error = sqlx::query("update runtime_settings set usage_retention_days=$1,ops_event_retention_days=$2,audit_retention_days=$3")
            .bind(usage).bind(ops).bind(audit).execute(&database.pool).await.unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23514")
        );
    }
    sqlx::query("update runtime_settings set usage_retention_days=default")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        repository
            .load_policy()
            .await
            .unwrap()
            .days(RetentionTarget::ModelRequests),
        31
    );
    database.close().await;
}

#[tokio::test]
async fn short_retention_keeps_statistics_readable_until_each_deletion_batch_is_projected() {
    use gateway_admin::{
        model::observability::{AnalyticsState, DashboardQuery, TimeRange},
        ports::store::ObservabilityStore as _,
    };

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
    let dashboard_query = DashboardQuery::new(TimeRange {
        start: now - ChronoDuration::days(1),
        end: now,
    });
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
        (select count(*) from request_analytics_hourly where bucket_start >= $1 and bucket_start < $2),
        (select count(*) from request_analytics_daily_counts where bucket_start >= $1 and bucket_start < $2)";
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
