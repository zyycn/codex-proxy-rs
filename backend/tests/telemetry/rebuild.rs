use chrono::{Duration, Utc};
use codex_proxy_rs::telemetry::{
    ops::{store::PgOpsErrorLogStore, types::OpsErrorLog},
    rebuild::rebuild_buckets,
    usage::{store::PgUsageRecordStore, types::UsageRecord},
};

use crate::support::storage::init_test_db;

#[tokio::test]
async fn rebuild_buckets_should_replace_only_fully_reconstructible_range() {
    let (pool, _guard) = init_test_db("rebuild-buckets").await;
    seed_runtime_settings(&pool, 30, 10).await;

    let now = Utc::now();
    let mut success = UsageRecord::new("v1.response", "ok", "acct-1", "gpt-5.5", 200);
    success.created_at = now;
    success.latency_ms = Some(0);
    success.first_token_ms = Some(0);
    success.input_tokens = Some(12);
    success.output_tokens = Some(5);
    success.cached_tokens = Some(3);
    success.cache_write_tokens = Some(2);
    PgUsageRecordStore::new(pool.clone())
        .append(&success)
        .await
        .unwrap();

    let mut failure = OpsErrorLog::new("v1.response", "failed");
    failure.created_at = now;
    failure.provider = Some("openai".to_string());
    failure.account_id = Some("acct-1".to_string());
    failure.model = Some("gpt-5.5".to_string());
    failure.status_code = Some(502);
    PgOpsErrorLogStore::new(pool.clone())
        .append(&failure)
        .await
        .unwrap();

    sqlx::query(
        "update request_time_buckets
         set success_count = 99, error_count = 99, input_tokens = 999",
    )
    .execute(&pool)
    .await
    .unwrap();

    let protected_bucket = now - Duration::days(20);
    sqlx::query(
        "insert into request_time_buckets
         (bucket_start, provider, account_id, model, service_tier,
          success_count, error_count, updated_at)
         values ($1, 'openai', 'old-account', 'old-model', '__unknown__', 7, 2, now())",
    )
    .bind(protected_bucket)
    .execute(&pool)
    .await
    .unwrap();

    let report = rebuild_buckets(&pool).await.unwrap();

    assert_eq!(report.deleted_rows, 1);
    assert_eq!(report.rebuilt_rows, 1);
    let rebuilt: (i64, i64, i64, i64, i64, i64, i64, i64, Option<i64>) = sqlx::query_as(
        "select success_count, error_count, input_tokens, output_tokens, cached_tokens,
                cache_write_tokens,
                first_token_latency_count, latency_count, min_latency_ms
         from request_time_buckets where account_id = 'acct-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rebuilt, (1, 1, 12, 5, 3, 2, 1, 1, Some(0)));

    let preserved: (i64, i64) = sqlx::query_as(
        "select success_count, error_count
         from request_time_buckets where account_id = 'old-account'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(preserved, (7, 2));
    assert!(report.cutoff > now - Duration::days(11));
}

async fn seed_runtime_settings(pool: &sqlx::PgPool, usage_days: i64, ops_days: i64) {
    sqlx::query(
        "insert into runtime_settings
         (id, model_aliases_json, refresh_margin_seconds, refresh_concurrency,
          max_concurrent_per_account, request_interval_ms, rotation_strategy,
          usage_retention_days, ops_error_retention_days, bucket_retention_days, updated_at)
         values (1, '{}', 3600, 2, 3, 50, 'smart', $1, $2, 90, now())",
    )
    .bind(usage_days)
    .bind(ops_days)
    .execute(pool)
    .await
    .unwrap();
}
