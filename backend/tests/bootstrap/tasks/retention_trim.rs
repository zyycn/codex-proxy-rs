use chrono::{Duration, Utc};
use codex_proxy_rs::{
    bootstrap::tasks::retention_trim::RetentionTrimTask,
    telemetry::{
        buckets::store::PgRequestBucketStore,
        ops::{store::PgOpsErrorLogStore, types::OpsErrorLog},
        usage::{store::PgUsageRecordStore, types::UsageRecord},
    },
    upstream::openai::fingerprint::PgFingerprintStore,
};

use crate::support::storage::init_test_db;

#[tokio::test]
async fn retention_trim_task_should_trim_all_growth_tables_in_one_run() {
    let (pool, _guard) = init_test_db("retention-trim-task").await;
    sqlx::query(
        "insert into runtime_settings (
           id, refresh_margin_seconds, refresh_concurrency,
           max_concurrent_per_account, request_interval_ms, rotation_strategy,
           usage_retention_days, ops_error_retention_days, bucket_retention_days, updated_at
         ) values (1, 3600, 2, 1, 0, 'smart', 1, 1, 1, now())",
    )
    .execute(&pool)
    .await
    .unwrap();

    let usage_store = PgUsageRecordStore::new(pool.clone());
    let ops_store = PgOpsErrorLogStore::new(pool.clone());
    let bucket_store = PgRequestBucketStore::new(pool.clone());
    let fingerprint_store = PgFingerprintStore::new(pool.clone());
    fingerprint_store
        .ensure_current_seed(&crate::support::fingerprint::test_fingerprint())
        .await
        .unwrap();

    let now = Utc::now();
    let mut old_usage = UsageRecord::new("request", "old", "acct", "gpt-old", 200);
    old_usage.id = "usage_old".to_string();
    old_usage.created_at = now - Duration::days(2);
    usage_store.append(&old_usage).await.unwrap();
    let mut current_usage = UsageRecord::new("request", "current", "acct", "gpt-new", 200);
    current_usage.id = "usage_current".to_string();
    current_usage.created_at = now;
    usage_store.append(&current_usage).await.unwrap();

    let mut old_error = OpsErrorLog::new("request", "old error");
    old_error.id = "ops_old".to_string();
    old_error.model = Some("gpt-old".to_string());
    old_error.created_at = now - Duration::days(2);
    ops_store.append(&old_error).await.unwrap();
    let mut current_error = OpsErrorLog::new("request", "current error");
    current_error.id = "ops_current".to_string();
    current_error.model = Some("gpt-new".to_string());
    current_error.created_at = now;
    ops_store.append(&current_error).await.unwrap();

    sqlx::query(
        "insert into fingerprint_update_history (
           id, current_fingerprint_id, app_version, build_number, source, created_at
         )
         select 'history_' || value, 'current', '1.0.' || value, value::text, 'test',
                now() + value * interval '1 microsecond'
         from generate_series(1, 101) as value",
    )
    .execute(&pool)
    .await
    .unwrap();

    RetentionTrimTask::new(usage_store, ops_store, bucket_store, fingerprint_store)
        .run_once()
        .await;

    let usage_ids: Vec<String> = sqlx::query_scalar("select id from usage_records order by id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let ops_ids: Vec<String> = sqlx::query_scalar("select id from ops_error_logs order by id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let old_buckets: i64 = sqlx::query_scalar(
        "select count(*) from request_time_buckets where bucket_start < now() - interval '1 day'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let history_count: i64 = sqlx::query_scalar("select count(*) from fingerprint_update_history")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(usage_ids, vec!["usage_current".to_string()]);
    assert_eq!(ops_ids, vec!["ops_current".to_string()]);
    assert_eq!(old_buckets, 0);
    assert_eq!(history_count, 100);
}
