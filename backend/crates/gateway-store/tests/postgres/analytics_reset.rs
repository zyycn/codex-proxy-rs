//! 统计版本清理的主键进度、事务回滚、表重写与进程重启验收

use super::{
    TestDatabase, analytics::seed, drain_analytics, observability_query_budget,
    observability_repository,
};
use chrono::{TimeDelta, Utc};
use gateway_store::postgres::{ObservabilityRange, PgRequestAnalyticsRepository};

#[tokio::test]
async fn version_reset_persists_primary_keys_atomically_and_resumes_after_table_rewrite() {
    let Some(database) = TestDatabase::create("analytics_reset_cursor").await else {
        return;
    };
    let now = Utc::now();
    let start = now - TimeDelta::hours(1);
    for index in 0..5 {
        seed(&database.pool, &format!("reset_{index:02}"), start, &[]).await;
    }
    drain_analytics(&database.pool).await;
    // 旧格式内容只能被清理，复合主键在同一时间桶内也需要持续前进
    for table in ["request_analytics_15m", "request_analytics_hourly"] {
        sqlx::query(sqlx::AssertSqlSafe(format!("insert into {table}(bucket_start,collection,dimensions,outcome,schema_version,histogram_version,counts,costs,distributions) select $1,'obsolete',array['key_'||i],outcome,0,0,'{{}}','{{}}','{{}}' from generate_series(1,3)i cross join unnest(array['failed','succeeded'])outcome")))
            .bind(start).execute(&database.pool).await.unwrap();
    }
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) select $1 + i * interval '1 day',$1 from generate_series(1,5)i")
        .bind(start).execute(&database.pool).await.unwrap();
    sqlx::query("update request_analytics_build_state set schema_version=0,histogram_version=0")
        .execute(&database.pool)
        .await
        .unwrap();
    // 游标保存失败必须回滚同批删除，不能跳过尚未成功清理的行
    sqlx::raw_sql("create function reject_reset_cursor() returns trigger language plpgsql as $$ begin if new.reset_cursor is not null then raise exception 'test reset progress failure'; end if; return new; end $$; create trigger reject_reset_cursor before update on request_analytics_build_state for each row execute function reject_reset_cursor();")
        .execute(&database.pool).await.unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    assert!(worker.maintain(2).await.is_err());
    let unchanged: (i64, i16, bool) = sqlx::query_as("select (select count(*) from request_analytics_facts),reset_phase,reset_cursor is null from request_analytics_build_state")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(unchanged, (5, 0, true));
    sqlx::raw_sql("drop trigger reject_reset_cursor on request_analytics_build_state; drop function reject_reset_cursor();")
        .execute(&database.pool).await.unwrap();
    assert!(worker.maintain(2).await.unwrap().reset_progress);
    let cursor: serde_json::Value =
        sqlx::query_scalar("select reset_cursor from request_analytics_build_state")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        cursor,
        serde_json::json!({"kind":"fact","key":{"id":"reset_01"}})
    );
    drop(worker);
    // 物理位置变化不改变跨事务主键进度，新增源记录即使 ID 落在游标之前也会重新回填
    sqlx::query("vacuum full request_analytics_facts")
        .execute(&database.pool)
        .await
        .unwrap();
    seed(&database.pool, "reset_00_new", start, &[]).await;
    sqlx::query("update model_requests set total_tokens=240 where id='reset_04'")
        .execute(&database.pool)
        .await
        .unwrap();
    let mut seen_phases = std::collections::BTreeSet::from([1_i16]);
    let mut completed = false;
    for _ in 0..100 {
        let before: (i16, Option<serde_json::Value>) =
            sqlx::query_as("select reset_phase,reset_cursor from request_analytics_build_state")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        let table = match before.0 {
            1 => "request_analytics_facts",
            2 => "request_analytics_15m",
            3 => "request_analytics_hourly",
            4 => "request_analytics_pending_buckets",
            _ => panic!("reset ended before final verification"),
        };
        let remaining: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {table}")))
                .fetch_one(&database.pool)
                .await
                .unwrap();
        // 每轮创建新实例，所有推进事实必须来自数据库而非 Worker 内存
        let worker =
            PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
        assert!(worker.maintain(2).await.unwrap().reset_progress);
        let after: (i16, Option<serde_json::Value>) =
            sqlx::query_as("select reset_phase,reset_cursor from request_analytics_build_state")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        let left: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {table}")))
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(remaining - left, remaining.min(2));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("select count(*) from request_analytics_pending_requests")
                .fetch_one(&database.pool)
                .await
                .unwrap(),
            2
        );
        if before.0 == after.0 {
            assert!(after.1.is_some());
            assert_ne!(before.1, after.1);
            seen_phases.insert(before.0);
        } else {
            assert!(after.1.is_none());
        }
        if after.0 == 0 {
            completed = true;
            break;
        }
    }
    assert!(completed);
    assert_eq!(seen_phases, std::collections::BTreeSet::from([1, 2, 3, 4]));
    drain_analytics(&database.pool).await;
    let repo = observability_repository(&database.pool);
    let range = ObservabilityRange::new(start - TimeDelta::minutes(1), now).unwrap();
    let result = repo.usage_summary(range, Default::default()).await.unwrap();
    assert_eq!(result.requests.request_count, 6);
    assert_eq!(result.requests.total_tokens, 840);
    assert_eq!(result.analytics.schema_version, 1);
    assert_eq!(result.analytics.histogram_version, 1);
    database.close().await;
}
