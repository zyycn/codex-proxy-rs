use codex_proxy_rs::telemetry::{
    ops::store::{OpsErrorFilter, PgOpsErrorLogStore},
    ops::types::OpsErrorLog,
};

use crate::support::storage::init_test_db;

#[tokio::test]
async fn ops_store_filters_diagnostic_dimensions() {
    let (pool, _guard) = init_test_db("ops-error-filter").await;
    let store = PgOpsErrorLogStore::new(pool);
    let mut error = OpsErrorLog::new("upstream", "rate limited");
    error.id = "ops_error_matching".to_string();
    error.request_id = Some("req_error".to_string());
    error.client_api_key_id = Some("key_1".to_string());
    error.failure_class = Some("rate_limited".to_string());
    error.upstream_status_code = Some(429);
    error.transport = Some("http_sse".to_string());
    store.append(&error).await.unwrap();

    let page = store
        .list_page(
            OpsErrorFilter {
                client_api_key_id: Some("key_1".to_string()),
                failure_class: Some("rate_limited".to_string()),
                upstream_status_code: Some(429),
                search: Some("req_error".to_string()),
                ..OpsErrorFilter::default()
            },
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, "ops_error_matching");
}

#[tokio::test]
async fn ops_error_and_bucket_are_one_transaction() {
    let (pool, _guard) = init_test_db("ops-error-transaction").await;
    let store = PgOpsErrorLogStore::new(pool.clone());
    sqlx::raw_sql(
        "create function reject_error_bucket() returns trigger language plpgsql as $$
         begin raise exception 'bucket rejected'; end $$;
         create trigger reject_error_bucket before insert or update on request_time_buckets
         for each row execute function reject_error_bucket();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut error = OpsErrorLog::new("upstream", "must rollback");
    error.id = "ops_error_rolled_back".to_string();
    assert!(store.append(&error).await.is_err());
    let count: i64 = sqlx::query_scalar("select count(*) from ops_error_logs where id = $1")
        .bind(&error.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
