use std::sync::Arc;

use chrono::Utc;
use codex_proxy_rs::{
    infra::crypto::SecretBox,
    upstream::accounts::{
        pool::{AccountAcquireRequest, AccountPoolOptions, RuntimeAccountPoolService},
        store::{AccountStore, SqliteAccountStore},
    },
};
use secrecy::SecretString;

use crate::support::sqlite::init_test_db;

#[tokio::test]
async fn runtime_account_pool_should_restore_capacity_and_clear_runtime_state() {
    let (pool, _dir) = init_test_db("runtime-account-pool.sqlite").await;
    let secret_box = SecretBox::new([34u8; 32]);
    insert_account(&pool, &secret_box, "acct_pool").await;

    let store = SqliteAccountStore::new(pool, secret_box);
    let service = RuntimeAccountPoolService::new(
        Arc::new(store) as Arc<dyn AccountStore>,
        AccountPoolOptions {
            max_concurrent_per_account: 2,
            ..AccountPoolOptions::default()
        },
        0,
    );

    let restored = service
        .restore_from_repository()
        .await
        .expect("account pool should restore");
    let capacity = service.capacity_summary_now().await;
    let acquired = service
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await
        .expect("active account should be acquired");
    let used_capacity = service.capacity_summary_now().await;
    service.clear().await;
    let cleared_capacity = service.capacity_summary_now().await;

    assert_eq!(restored, 1);
    assert_eq!(capacity.total_slots, 2);
    assert_eq!(capacity.available_slots, 2);
    assert_eq!(acquired.account.id, "acct_pool");
    assert_eq!(used_capacity.used_slots, 1);
    assert_eq!(used_capacity.available_slots, 1);
    assert_eq!(cleared_capacity.total_slots, 0);
}

async fn insert_account(pool: &sqlx::SqlitePool, secret_box: &SecretBox, id: &str) {
    let access_token = secret_box
        .encrypt(&SecretString::new(format!("access-{id}").into()))
        .expect("access token should encrypt");
    sqlx::query(
        "insert into accounts (id, email, chatgpt_account_id, chatgpt_user_id, access_token_cipher, access_token_expires_at, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(format!("{id}@example.com"))
    .bind(format!("chatgpt-{id}"))
    .bind(format!("user-{id}"))
    .bind(access_token)
    .bind("2999-01-01T00:00:00Z")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .expect("account should be inserted");
}
