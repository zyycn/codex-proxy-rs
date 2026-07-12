use std::sync::Arc;

use chrono::Utc;
use codex_proxy_rs::fleet::{
    pool::{AccountAcquireRequest, AccountPoolOptions, AccountPoolService, RotationStrategy},
    store::{AccountStore, PgAccountStore},
};
use codex_proxy_rs::telemetry::account_usage::store::PgAccountUsageStore;

use crate::support::storage::init_test_db;

#[tokio::test]
async fn runtime_account_pool_should_restore_capacity_and_clear_runtime_state() {
    let (pool, _dir) = init_test_db("runtime-account-pool").await;
    insert_account(&pool, "acct_pool").await;

    let store = PgAccountStore::new(pool.clone());
    let service = AccountPoolService::new(
        Arc::new(store) as Arc<dyn AccountStore>,
        Arc::new(PgAccountUsageStore::new(pool)),
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

    let store = PgAccountStore::new(pool.clone());
    let service = AccountPoolService::new(
        Arc::new(store) as Arc<dyn AccountStore>,
        Arc::new(PgAccountUsageStore::new(pool)),
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

#[tokio::test]
async fn runtime_account_lease_drop_should_release_capacity() {
    let (pool, _dir) = init_test_db("runtime-account-pool-lease-drop").await;
    insert_account(&pool, "acct_drop").await;

    let store = PgAccountStore::new(pool.clone());
    let service = AccountPoolService::new(
        Arc::new(store) as Arc<dyn AccountStore>,
        Arc::new(PgAccountUsageStore::new(pool)),
        AccountPoolOptions {
            max_concurrent_per_account: 1,
            ..AccountPoolOptions::default()
        },
        0,
    );
    service
        .restore_from_store()
        .await
        .expect("account pool should restore");
    let lease = service
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await
        .expect("active account should be acquired");

    drop(lease);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if service.capacity_summary_now().await.available_slots == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropped lease should release its slot");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_account_pool_should_preserve_slot_invariants_under_contention() {
    let (pool, _dir) = init_test_db("runtime-account-pool-contention").await;
    insert_account(&pool, "acct_contention").await;
    let service = Arc::new(AccountPoolService::new(
        Arc::new(PgAccountStore::new(pool.clone())) as Arc<dyn AccountStore>,
        Arc::new(PgAccountUsageStore::new(pool)),
        AccountPoolOptions {
            max_concurrent_per_account: 4,
            ..AccountPoolOptions::default()
        },
        0,
    ));
    service.restore_from_store().await.unwrap();

    let acquisitions = futures::future::join_all((0..64).map(|_| {
        let service = Arc::clone(&service);
        async move {
            service
                .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
                .await
        }
    }))
    .await;
    let leases = acquisitions.into_iter().flatten().collect::<Vec<_>>();
    let held = service.capacity_summary_now().await;
    assert_eq!(leases.len(), 4);
    assert_eq!(held.used_slots, 4);
    assert_eq!(held.available_slots, 0);

    futures::future::join_all(
        leases
            .into_iter()
            .map(|lease| lease.release_without_usage()),
    )
    .await;
    let released = service.capacity_summary_now().await;
    assert_eq!(released.used_slots, 0);
    assert_eq!(released.available_slots, 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_account_pool_should_release_slots_when_tasks_are_cancelled() {
    let (pool, _dir) = init_test_db("runtime-account-pool-cancel").await;
    insert_account(&pool, "acct_cancel").await;
    let service = Arc::new(AccountPoolService::new(
        Arc::new(PgAccountStore::new(pool.clone())) as Arc<dyn AccountStore>,
        Arc::new(PgAccountUsageStore::new(pool)),
        AccountPoolOptions {
            max_concurrent_per_account: 8,
            ..AccountPoolOptions::default()
        },
        0,
    ));
    service.restore_from_store().await.unwrap();

    let ready = Arc::new(tokio::sync::Barrier::new(9));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let service = Arc::clone(&service);
        let ready = Arc::clone(&ready);
        tasks.push(tokio::spawn(async move {
            let _lease = service
                .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
                .await
                .unwrap();
            ready.wait().await;
            std::future::pending::<()>().await;
        }));
    }
    ready.wait().await;
    assert_eq!(service.capacity_summary_now().await.used_slots, 8);
    for task in tasks {
        task.abort();
        let _ = task.await;
    }

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if service.capacity_summary_now().await.used_slots == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled account leases should release all slots");
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
