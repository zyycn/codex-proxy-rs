use std::num::NonZeroU32;

use chrono::Utc;
use codex_proxy_rs::telemetry::account_usage::store::PgAccountUsageStore;

use crate::support::storage::init_test_db;

#[tokio::test]
async fn trim_model_usage_should_keep_latest_rows_per_account() {
    let (pool, _guard) = init_test_db("account-model-usage-limit").await;
    sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at)
         values ('account-limit', 'token', 'active', $1, $1)",
    )
    .bind(Utc::now())
    .execute(&pool)
    .await
    .expect("insert account");
    sqlx::query(
        "insert into account_model_usage (account_id, model, request_count, last_used_at)
         select
           'account-limit',
           'model-' || lpad(sequence::text, 3, '0'),
           1,
           $1 + sequence * interval '1 second'
         from generate_series(0, 101) as sequence",
    )
    .bind(Utc::now())
    .execute(&pool)
    .await
    .expect("insert model usage rows");

    let deleted = PgAccountUsageStore::new(pool.clone())
        .trim_model_usage_to_limit(NonZeroU32::new(100).unwrap())
        .await
        .expect("trim model usage rows");
    let (remaining, oldest_remaining): (i64, i64) = sqlx::query_as(
        "select
           count(*),
           count(*) filter (where model in ('model-000', 'model-001'))
         from account_model_usage
         where account_id = 'account-limit'",
    )
    .fetch_one(&pool)
    .await
    .expect("count retained model usage rows");

    assert_eq!((deleted, remaining, oldest_remaining), (2, 100, 0));
}
