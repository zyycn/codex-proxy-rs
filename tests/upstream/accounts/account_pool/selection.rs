use super::*;

#[test]
fn account_pool_should_respect_max_concurrent_slots_per_account() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        ..AccountPoolOptions::default()
    });
    pool.insert(Account::test("acct_a", AccountStatus::Active));
    pool.insert(Account::test("acct_b", AccountStatus::Active));

    let first = pool.acquire("gpt-5.5").unwrap();
    let second = pool.acquire("gpt-5.5").unwrap();
    let third = pool.acquire("gpt-5.5");

    assert_ne!(first.id, second.id);
    assert!(third.is_none());

    pool.release(&first.id);
    assert_eq!(pool.acquire("gpt-5.5").unwrap().id, first.id);
}

#[test]
fn account_pool_should_rotate_round_robin_across_candidates() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        rotation_strategy: RotationStrategy::RoundRobin,
        ..AccountPoolOptions::default()
    });
    pool.insert(Account::test("acct_a", AccountStatus::Active));
    pool.insert(Account::test("acct_b", AccountStatus::Active));
    pool.insert(Account::test("acct_c", AccountStatus::Active));

    assert_eq!(pool.acquire("gpt-5.5").unwrap().id, "acct_a");
    assert_eq!(pool.acquire("gpt-5.5").unwrap().id, "acct_c");
    assert_eq!(pool.acquire("gpt-5.5").unwrap().id, "acct_b");
    assert!(pool.acquire("gpt-5.5").is_none());
}

#[test]
fn acquire_should_skip_accounts_with_expired_token_metadata() {
    let now = fixed_time();
    let mut expired = Account::test("expired", AccountStatus::Active);
    expired.access_token_expires_at = Some(now - Duration::seconds(1));
    let mut pool = AccountPool::default();
    pool.insert(expired);

    let acquired = pool.acquire_with(AccountAcquireRequest::new("gpt-5.5", now));

    assert!(acquired.is_none());
}

#[test]
fn acquire_should_skip_accounts_with_expired_jwt_when_metadata_is_missing() {
    let now = Utc::now();
    let mut expired = Account::test("expired", AccountStatus::Active);
    expired.access_token = test_jwt(-60);
    expired.access_token_expires_at = None;
    let mut pool = AccountPool::default();
    pool.insert(expired);

    let acquired = pool.acquire_with(AccountAcquireRequest::new("gpt-5.5", now));

    assert!(acquired.is_none());
}

#[tokio::test]
async fn runtime_account_pool_should_persist_expired_status_when_jwt_expiry_is_discovered() {
    let (pool, _dir) =
        crate::support::sqlite::init_test_db("runtime-pool-expired-status.sqlite").await;
    let store = codex_proxy_rs::upstream::accounts::store::SqliteAccountStore::new(
        pool.clone(),
        codex_proxy_rs::infra::crypto::SecretBox::new([91u8; 32]),
    );
    store
        .insert(codex_proxy_rs::upstream::accounts::store::NewAccount {
            id: "acct_expired".to_string(),
            email: None,
            account_id: Some("chatgpt-expired".to_string()),
            user_id: None,
            label: None,
            plan_type: Some("free".to_string()),
            access_token: secrecy::SecretString::new(test_jwt(-60).into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        })
        .await
        .unwrap();
    let runtime_pool = codex_proxy_rs::upstream::accounts::pool::RuntimeAccountPoolService::new(
        std::sync::Arc::new(store),
        AccountPoolOptions::default(),
        0,
    );
    runtime_pool.restore_from_repository().await.unwrap();

    let acquired = runtime_pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await;
    let status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_expired")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(acquired.is_none());
    assert_eq!(status.0, "expired");
}

#[test]
fn account_pool_should_prefer_configured_tier_priority() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        tier_priority: vec!["team".to_string(), "plus".to_string()],
        ..AccountPoolOptions::default()
    });
    let mut free = Account::test("free", AccountStatus::Active);
    free.plan_type = Some("free".to_string());
    let mut team = Account::test("team", AccountStatus::Active);
    team.plan_type = Some("team".to_string());
    pool.insert(free);
    pool.insert(team);

    assert_eq!(pool.acquire("gpt-5.5").unwrap().id, "team");
}

#[test]
fn account_pool_should_filter_by_model_plan_allowlist() {
    let mut model_plans = BTreeMap::new();
    model_plans.insert("gpt-5.5".to_string(), vec!["plus".to_string()]);
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        model_plan_allowlist: model_plans,
        ..AccountPoolOptions::default()
    });
    let mut free = Account::test("free", AccountStatus::Active);
    free.plan_type = Some("free".to_string());
    let mut plus = Account::test("plus", AccountStatus::Active);
    plus.plan_type = Some("plus".to_string());
    pool.insert(free);
    pool.insert(plus);

    assert_eq!(pool.acquire("gpt-5.5").unwrap().id, "plus");
}

#[test]
fn account_pool_should_exclude_requested_account_ids() {
    let mut pool = AccountPool::default();
    pool.insert(Account::test("acct_a", AccountStatus::Active));
    pool.insert(Account::test("acct_b", AccountStatus::Active));

    let acquired = pool
        .acquire_with(
            AccountAcquireRequest::new("gpt-5.5", fixed_time())
                .with_exclude_account_ids(["acct_a"]),
        )
        .unwrap();

    assert_eq!(acquired.account.id, "acct_b");
}

#[test]
fn account_pool_should_prefer_session_affinity_account_when_available() {
    let mut pool = AccountPool::default();
    pool.insert(Account::test("acct_a", AccountStatus::Active));
    pool.insert(Account::test("acct_b", AccountStatus::Active));

    let acquired = pool
        .acquire_with(
            AccountAcquireRequest::new("gpt-5.5", fixed_time()).with_preferred_account_id("acct_b"),
        )
        .unwrap();

    assert_eq!(acquired.account.id, "acct_b");
}

#[test]
fn account_pool_should_cleanup_stale_slots_before_acquire() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        stale_slot_ttl: Duration::minutes(5),
        ..AccountPoolOptions::default()
    });
    pool.insert(Account::test("acct_a", AccountStatus::Active));

    assert!(pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .is_some());
    let acquired = pool
        .acquire_with(AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::minutes(5) + Duration::seconds(1),
        ))
        .unwrap();

    assert_eq!(acquired.account.id, "acct_a");
}

#[test]
fn account_pool_should_rotate_tied_least_used_accounts() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        rotation_strategy: RotationStrategy::LeastUsed,
        ..AccountPoolOptions::default()
    });
    pool.insert(Account::test("acct_a", AccountStatus::Active));
    pool.insert(Account::test("acct_b", AccountStatus::Active));

    let first = pool.acquire("gpt-5.5").unwrap();
    pool.release(&first.id);
    let second = pool.acquire("gpt-5.5").unwrap();

    assert_ne!(first.id, second.id);
}

#[test]
fn least_used_should_prefer_lower_runtime_request_count() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::LeastUsed,
        ..AccountPoolOptions::default()
    });
    let mut busy = Account::test("busy", AccountStatus::Active);
    busy.request_count = 10;
    let mut quiet = Account::test("quiet", AccountStatus::Active);
    quiet.request_count = 2;
    pool.insert(busy);
    pool.insert(quiet);

    let acquired = pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "quiet");
}
