use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use codex_proxy_rs::fleet::{
    account::{Account, AccountStatus},
    pool::{AccountAcquireRequest, AccountPoolOptions, AccountPoolService, RotationStrategy},
    store::{AccountStore, AccountStoreResult, PgAccountStore},
    usage::{
        AccountUsageDelta, AccountUsageSnapshot, AccountUsageStore, AccountUsageStoreError,
        AccountUsageWindow,
    },
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
async fn upstream_status_should_exclude_account_before_database_write_completes() {
    let store = Arc::new(BlockingAccountStore::new());
    let service = AccountPoolService::new(
        Arc::clone(&store) as Arc<dyn AccountStore>,
        Arc::new(NoopAccountUsageStore),
        AccountPoolOptions::default(),
        0,
    );
    service.restore_from_store().await.unwrap();

    let updated = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        service.set_status_immediately("acct_primary", AccountStatus::Banned),
    )
    .await
    .expect("runtime invalidation must not wait for PostgreSQL");
    store.wait_until_write_started().await;
    let acquired = service
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await
        .expect("secondary account should remain available");

    assert!(updated);
    assert_eq!(acquired.account.id, "acct_secondary");

    acquired.release_without_usage().await;
    store.release_write();
    store
        .wait_for_write(PersistedAccountState::Status(AccountStatus::Banned))
        .await;
}

#[tokio::test]
async fn upstream_quota_should_exclude_account_before_database_write_completes() {
    let store = Arc::new(BlockingAccountStore::new());
    let service = AccountPoolService::new(
        Arc::clone(&store) as Arc<dyn AccountStore>,
        Arc::new(NoopAccountUsageStore),
        AccountPoolOptions::default(),
        0,
    );
    service.restore_from_store().await.unwrap();
    let cooldown_until = Utc::now() + chrono::Duration::minutes(5);

    let updated = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        service.mark_quota_limited_until_immediately("acct_primary", cooldown_until),
    )
    .await
    .expect("runtime quota invalidation must not wait for PostgreSQL");
    store.wait_until_write_started().await;
    let acquired = service
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await
        .expect("secondary account should remain available");

    assert!(updated);
    assert_eq!(acquired.account.id, "acct_secondary");

    acquired.release_without_usage().await;
    store.release_write();
    store
        .wait_for_write(PersistedAccountState::QuotaLimitedUntil(cooldown_until))
        .await;
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum PersistedAccountState {
    Status(AccountStatus),
    QuotaLimitedUntil(DateTime<Utc>),
}

struct BlockingAccountStore {
    accounts: Vec<Account>,
    write_started: tokio::sync::Semaphore,
    release_write: tokio::sync::Semaphore,
    persisted: tokio::sync::Mutex<Vec<PersistedAccountState>>,
}

impl BlockingAccountStore {
    fn new() -> Self {
        Self {
            accounts: vec![
                crate::support::accounts::test_account("acct_primary", AccountStatus::Active),
                crate::support::accounts::test_account("acct_secondary", AccountStatus::Active),
            ],
            write_started: tokio::sync::Semaphore::new(0),
            release_write: tokio::sync::Semaphore::new(0),
            persisted: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    async fn persist(&self, state: PersistedAccountState) {
        self.write_started.add_permits(1);
        self.release_write
            .acquire()
            .await
            .expect("test persistence gate should remain open")
            .forget();
        self.persisted.lock().await.push(state);
    }

    async fn wait_until_write_started(&self) {
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.write_started.acquire(),
        )
        .await
        .expect("deferred persistence should start")
        .expect("test persistence gate should remain open")
        .forget();
    }

    fn release_write(&self) {
        self.release_write.add_permits(1);
    }

    async fn wait_for_write(&self, expected: PersistedAccountState) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if self.persisted.lock().await.contains(&expected) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("deferred account state should be persisted");
    }
}

#[async_trait::async_trait]
impl AccountStore for BlockingAccountStore {
    async fn list_pool_accounts(&self) -> AccountStoreResult<Vec<Account>> {
        Ok(self.accounts.clone())
    }

    async fn mark_quota_limited_until(
        &self,
        _account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        self.persist(PersistedAccountState::QuotaLimitedUntil(cooldown_until))
            .await;
        Ok(true)
    }

    async fn set_cloudflare_cooldown_until(
        &self,
        _account_id: &str,
        _cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        Ok(false)
    }

    async fn set_status(
        &self,
        _account_id: &str,
        status: AccountStatus,
    ) -> AccountStoreResult<bool> {
        self.persist(PersistedAccountState::Status(status)).await;
        Ok(true)
    }

    async fn get_quota_json(&self, _account_id: &str) -> AccountStoreResult<Option<String>> {
        Ok(None)
    }

    async fn apply_quota_snapshot(
        &self,
        _account_id: &str,
        _quota_json: &str,
        _limit_reached: bool,
        _cooldown_until: Option<DateTime<Utc>>,
    ) -> AccountStoreResult<bool> {
        Ok(false)
    }

    async fn sync_runtime_account_state(&self, _account: &Account) -> AccountStoreResult<bool> {
        Ok(false)
    }
}

struct NoopAccountUsageStore;

#[async_trait::async_trait]
impl AccountUsageStore for NoopAccountUsageStore {
    async fn snapshots(
        &self,
        _account_ids: &[String],
    ) -> Result<HashMap<String, AccountUsageSnapshot>, AccountUsageStoreError> {
        Ok(HashMap::new())
    }

    async fn record_usage_delta(
        &self,
        _account_id: &str,
        _usage: AccountUsageDelta,
    ) -> Result<(), AccountUsageStoreError> {
        Ok(())
    }

    async fn sync_runtime_window(
        &self,
        _account_id: &str,
        _window: AccountUsageWindow,
    ) -> Result<(), AccountUsageStoreError> {
        Ok(())
    }

    async fn sync_rate_limit_window(
        &self,
        _account_id: &str,
        _reset_at: DateTime<Utc>,
        _limit_window_seconds: Option<u64>,
    ) -> Result<(), AccountUsageStoreError> {
        Ok(())
    }
}
