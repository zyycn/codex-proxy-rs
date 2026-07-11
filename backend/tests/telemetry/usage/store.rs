use chrono::{Duration, Utc};
use codex_proxy_rs::telemetry::{
    usage::store::{PgUsageRecordStore, UsageRecordFilter},
    usage::types::UsageRecord,
};

use crate::support::storage::init_test_db;

#[tokio::test]
async fn usage_store_should_filter_numbered_pages_of_success_facts() {
    let (pool, _guard) = init_test_db("usage-record-filter").await;
    let store = PgUsageRecordStore::new(pool);
    let mut matching = success_record("matching");
    matching.id = "usage_matching".to_string();
    matching.request_id = Some("req_1".to_string());
    matching.route = Some("/v1/responses".to_string());
    matching.client_api_key_id = Some("key_1".to_string());
    store.append(&matching).await.unwrap();
    store.append(&success_record("other")).await.unwrap();

    let page = store
        .list_page(
            UsageRecordFilter {
                kind: Some("request".to_string()),
                client_api_key_id: Some("key_1".to_string()),
                search: Some("matching".to_string()),
                ..UsageRecordFilter::default()
            },
            1,
            20,
        )
        .await
        .unwrap();

    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, "usage_matching");
    assert_eq!(page.items[0].request_id.as_deref(), Some("req_1"));
}

#[tokio::test]
async fn usage_store_should_list_recent_records_in_descending_order() {
    let (pool, _guard) = init_test_db("usage-record-recent").await;
    let store = PgUsageRecordStore::new(pool);
    let now = Utc::now();
    for (id, seconds) in [("usage_old", 0), ("usage_middle", 1), ("usage_new", 2)] {
        let mut record = success_record(id);
        record.id = id.to_string();
        record.created_at = now + Duration::seconds(seconds);
        store.append(&record).await.unwrap();
    }

    let records = store
        .list_recent(UsageRecordFilter::default(), 2)
        .await
        .unwrap();

    assert_eq!(
        records
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        ["usage_new", "usage_middle"]
    );
}

#[tokio::test]
async fn usage_record_and_bucket_are_one_transaction_and_keep_zero_ms_samples() {
    let (pool, _guard) = init_test_db("usage-record-transaction").await;
    let store = PgUsageRecordStore::new(pool.clone());
    let mut record = success_record("zero latency");
    record.id = "usage_zero_ms".to_string();
    record.latency_ms = Some(0);
    record.first_token_ms = Some(0);
    record.input_tokens = Some(11);
    record.output_tokens = Some(7);
    store.append(&record).await.unwrap();

    let bucket: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "select success_count, input_tokens, output_tokens, latency_count,
                first_token_latency_count
         from request_time_buckets where account_id = $1 and model = $2",
    )
    .bind(&record.account_id)
    .bind(&record.model)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bucket, (1, 11, 7, 1, 1));

    sqlx::raw_sql(
        "create function reject_test_bucket() returns trigger language plpgsql as $$
         begin raise exception 'bucket rejected'; end $$;
         create trigger reject_test_bucket before insert or update on request_time_buckets
         for each row execute function reject_test_bucket();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut rejected = success_record("must rollback");
    rejected.id = "usage_rolled_back".to_string();
    assert!(store.append(&rejected).await.is_err());
    let count: i64 = sqlx::query_scalar("select count(*) from usage_records where id = $1")
        .bind(&rejected.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn usage_store_trims_by_runtime_retention() {
    let (pool, _guard) = init_test_db("usage-record-retention").await;
    sqlx::query(
        "insert into runtime_settings (
           id, refresh_margin_seconds, refresh_concurrency,
           max_concurrent_per_account, request_interval_ms,
           rotation_strategy, updated_at
         ) values (1, 3600, 2, 1, 0, 'smart', now())",
    )
    .execute(&pool)
    .await
    .unwrap();
    let store = PgUsageRecordStore::new(pool);
    let now = Utc::now();
    let mut expired = success_record("expired");
    expired.id = "usage_expired".to_string();
    expired.created_at = now - Duration::days(31);
    store.append(&expired).await.unwrap();
    let mut retained = success_record("retained");
    retained.id = "usage_retained".to_string();
    retained.created_at = now - Duration::days(29);
    store.append(&retained).await.unwrap();

    assert_eq!(store.trim_to_retention(now).await.unwrap(), 1);
    assert!(store.get("usage_expired").await.unwrap().is_none());
    assert!(store.get("usage_retained").await.unwrap().is_some());
}

fn success_record(message: &str) -> UsageRecord {
    UsageRecord::new("request", message, "acct_usage", "gpt-5", 200)
}
