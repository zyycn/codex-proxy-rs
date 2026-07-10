use std::sync::Arc;

use chrono::Utc;
use codex_proxy_rs::accounts::{
    pool::{
        AccountAcquireRequest, AccountPoolOptions, RotationStrategy, RuntimeAccountPoolService,
    },
    store::{AccountStore, PgAccountStore},
};

use crate::support::storage::init_test_db;

#[tokio::test]
async fn runtime_account_pool_should_restore_capacity_and_clear_runtime_state() {
    let (pool, _dir) = init_test_db("runtime-account-pool").await;
    insert_account(&pool, "acct_pool").await;

    let store = PgAccountStore::new(pool);
    let service = RuntimeAccountPoolService::new(
        Arc::new(store) as Arc<dyn AccountStore>,
        AccountPoolOptions {
            max_concurrent_per_account: 2,
            ..AccountPoolOptions::default()
        },
        0,
    );

    let restored = service
        .restore_from_store()
        .await
        .expect("account pool should restore");
    let capacity = service.capacity_summary_now().await;
    let acquired = service
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await
        .expect("active account should be acquired");
    let used_capacity = service.capacity_summary_now().await;
    let restored_again = service
        .restore_from_store()
        .await
        .expect("account pool should restore again");
    let restored_capacity = service.capacity_summary_now().await;

    assert_eq!(restored, 1);
    assert_eq!(capacity.total_slots, 2);
    assert_eq!(capacity.available_slots, 2);
    assert_eq!(acquired.account.id, "acct_pool");
    assert_eq!(used_capacity.used_slots, 1);
    assert_eq!(used_capacity.available_slots, 1);
    assert_eq!(restored_again, 1);
    assert_eq!(restored_capacity.used_slots, 0);
    assert_eq!(restored_capacity.available_slots, 2);
}

#[tokio::test]
async fn runtime_account_pool_should_restore_accounts_by_added_at_and_id() {
    let (pool, _dir) = init_test_db("runtime-account-pool-order").await;
    insert_account(&pool, "acct_c").await;
    insert_account(&pool, "acct_a").await;

    let store = PgAccountStore::new(pool);
    let service = RuntimeAccountPoolService::new(
        Arc::new(store) as Arc<dyn AccountStore>,
        AccountPoolOptions {
            rotation_strategy: RotationStrategy::RoundRobin,
            ..AccountPoolOptions::default()
        },
        0,
    );

    service
        .restore_from_store()
        .await
        .expect("account pool should restore");
    let acquired = service
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await
        .expect("active account should be acquired");

    assert_eq!(acquired.account.id, "acct_a");
}

async fn insert_account(pool: &sqlx::PgPool, id: &str) {
    sqlx::query(
        "insert into accounts (id, email, chatgpt_account_id, chatgpt_user_id, access_token, access_token_expires_at, status, added_at, updated_at) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(format!("{id}@example.com"))
    .bind(format!("chatgpt-{id}"))
    .bind(format!("user-{id}"))
    .bind(format!("access-{id}"))
    .bind(crate::support::storage::timestamp("2999-01-01T00:00:00Z"))
    .bind("active")
    .bind(crate::support::storage::timestamp("2026-06-18T00:00:00Z"))
    .bind(crate::support::storage::timestamp("2026-06-18T00:00:00Z"))
    .execute(pool)
    .await
    .expect("account should be inserted");
}
