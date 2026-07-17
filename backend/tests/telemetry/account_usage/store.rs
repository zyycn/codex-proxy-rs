use chrono::{Duration, Utc};
use codex_proxy_rs::fleet::usage::{AccountUsageStore, AccountUsageWindow};
use codex_proxy_rs::telemetry::account_usage::store::PgAccountUsageStore;

use crate::support::storage::init_test_db;

#[tokio::test]
async fn rate_limit_window_update_should_reset_counters_atomically() {
    let (pool, _guard) = init_test_db("account-rate-limit-window").await;
    sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at)
         values ('account-window', 'token', 'active', now(), now())",
    )
    .execute(&pool)
    .await
    .unwrap();
    let store = PgAccountUsageStore::new(pool.clone());
    let initial_reset_at = Utc::now() + Duration::hours(1);
    store
        .sync_runtime_window(
            "account-window",
            AccountUsageWindow {
                request_count: 7,
                input_tokens: 11,
                output_tokens: 13,
                started_at: Some(Utc::now()),
                reset_at: Some(initial_reset_at),
                limit_window_seconds: Some(3_600),
                ..AccountUsageWindow::default()
            },
        )
        .await
        .unwrap();

    store
        .sync_rate_limit_window(
            "account-window",
            initial_reset_at + Duration::hours(1),
            Some(3_600),
        )
        .await
        .unwrap();

    let counters: (i64, i64, i64) = sqlx::query_as(
        "select window_request_count, window_input_tokens, window_output_tokens
         from account_usage where account_id = 'account-window'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counters, (0, 0, 0));
}
