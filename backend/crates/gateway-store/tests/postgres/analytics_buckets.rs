//! 小时重建合并、分段快照失效与阻塞桶调度的真实数据库验收

use chrono::{DateTime, TimeDelta, Utc};
use gateway_store::postgres::{ObservabilityRange, PgRequestAnalyticsRepository};

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
    sqlx::raw_sql("create table hourly_build_audit (bucket_start timestamptz); create function audit_hourly_build() returns trigger language plpgsql as $$begin if new.collection='global' then insert into hourly_build_audit values(new.bucket_start); end if; return new; end$$; create trigger audit_hourly_build after insert on request_analytics_hourly for each row execute function audit_hourly_build()")
        .execute(&database.pool).await.unwrap();
    seed_hour(&database.pool, hour(), "coalesced").await;
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
        .bind(changed)
        .execute(&database.pool)
        .await
        .unwrap();
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
    let hour_total: i64 = sqlx::query_scalar("select (counts->>'total_tokens')::bigint from request_analytics_hourly where collection='global'")
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
