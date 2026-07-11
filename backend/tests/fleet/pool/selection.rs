use super::*;

#[test]
fn account_pool_should_respect_max_concurrent_slots_per_account() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));

    let first = acquire_account(&mut pool, "gpt-5.5").unwrap();
    let second = acquire_account(&mut pool, "gpt-5.5").unwrap();
    let third = acquire_account(&mut pool, "gpt-5.5");

    assert_ne!(first.id, second.id);
    assert!(third.is_none());

    pool.release(&first);
    assert_eq!(acquire_account(&mut pool, "gpt-5.5").unwrap().id, first.id);
}

#[test]
fn account_pool_should_rotate_round_robin_across_candidates() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        rotation_strategy: RotationStrategy::RoundRobin,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_c",
        AccountStatus::Active,
    ));

    assert_eq!(acquire_account(&mut pool, "gpt-5.5").unwrap().id, "acct_a");
    assert_eq!(acquire_account(&mut pool, "gpt-5.5").unwrap().id, "acct_c");
    assert_eq!(acquire_account(&mut pool, "gpt-5.5").unwrap().id, "acct_b");
    assert!(acquire_account(&mut pool, "gpt-5.5").is_none());
}

#[test]
fn round_robin_should_use_account_insert_order() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::RoundRobin,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_c",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));

    let acquired = acquire_account(&mut pool, "gpt-5.5").unwrap();

    assert_eq!(acquired.id, "acct_c");
}

#[test]
fn round_robin_should_keep_ts_cursor_when_candidates_recover() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        rotation_strategy: RotationStrategy::RoundRobin,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_c",
        AccountStatus::Active,
    ));

    let first = acquire_account(&mut pool, "gpt-5.5").unwrap();
    let second = acquire_account(&mut pool, "gpt-5.5").unwrap();
    pool.release(&first);
    pool.release(&second);
    let third = acquire_account(&mut pool, "gpt-5.5").unwrap();

    assert_eq!(third.id, "acct_c");
}

#[test]
fn sticky_should_use_account_insert_order_when_last_used_ties() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::Sticky,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));

    let acquired = acquire_account(&mut pool, "gpt-5.5").unwrap();

    assert_eq!(acquired.id, "acct_a");
}

#[test]
fn sticky_should_prefer_most_recently_used_account() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::Sticky,
        ..AccountPoolOptions::default()
    });
    let mut older = crate::support::accounts::test_account("older", AccountStatus::Active);
    older.last_used_at = Some((now - Duration::minutes(10)).to_rfc3339());
    let mut newer = crate::support::accounts::test_account("newer", AccountStatus::Active);
    newer.last_used_at = Some((now - Duration::minutes(1)).to_rfc3339());
    pool.insert(older);
    pool.insert(newer);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "newer");
}

#[test]
fn acquire_should_skip_accounts_with_expired_token_metadata() {
    let now = fixed_time();
    let mut expired = crate::support::accounts::test_account("expired", AccountStatus::Active);
    expired.access_token_expires_at = Some(now - Duration::seconds(1));
    let mut pool = AccountPool::default();
    pool.insert(expired);

    let acquired = pool.acquire_with(&AccountAcquireRequest::new("gpt-5.5", now));

    assert!(acquired.is_none());
}

#[test]
fn acquire_should_trust_future_token_expiry_metadata_without_parsing_jwt() {
    let now = fixed_time();
    let mut active = crate::support::accounts::test_account("active", AccountStatus::Active);
    active.access_token = test_jwt(-60);
    active.access_token_expires_at = Some(now + Duration::hours(1));
    let mut pool = AccountPool::default();
    pool.insert(active);

    let acquired = pool.acquire_with(&AccountAcquireRequest::new("gpt-5.5", now));

    assert_eq!(acquired.unwrap().account.id, "active");
}

#[test]
fn acquire_should_skip_accounts_with_expired_jwt_when_metadata_is_missing() {
    let now = Utc::now();
    let mut expired = crate::support::accounts::test_account("expired", AccountStatus::Active);
    expired.access_token = test_jwt(-60);
    expired.access_token_expires_at = None;
    let mut pool = AccountPool::default();
    pool.insert(expired);

    let acquired = pool.acquire_with(&AccountAcquireRequest::new("gpt-5.5", now));

    assert!(acquired.is_none());
}

#[tokio::test]
async fn runtime_account_pool_should_persist_expired_status_when_jwt_expiry_is_discovered() {
    let (pool, _dir) = crate::support::storage::init_test_db("runtime-pool-expired-status").await;
    let store = codex_proxy_rs::fleet::store::PgAccountStore::new(pool.clone());
    store
        .insert(codex_proxy_rs::fleet::store::NewAccount {
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
    let runtime_pool = codex_proxy_rs::fleet::pool::AccountPoolService::new(
        std::sync::Arc::new(store),
        std::sync::Arc::new(
            codex_proxy_rs::telemetry::account_usage::store::PgAccountUsageStore::new(pool.clone()),
        ),
        AccountPoolOptions::default(),
        0,
    );
    runtime_pool.restore_from_store().await.unwrap();

    let acquired = runtime_pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await;
    let status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_expired")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(acquired.is_none());
    assert_eq!(status.0, "expired");
}

#[tokio::test]
async fn runtime_account_pool_should_persist_quota_window_reset_when_cooldown_expires() {
    let now = fixed_time();
    let (pool, _dir) =
        crate::support::storage::init_test_db("runtime-pool-quota-window-reset").await;
    let store = codex_proxy_rs::fleet::store::PgAccountStore::new(pool.clone());
    store
        .insert(codex_proxy_rs::fleet::store::NewAccount {
            id: "acct_quota_reset".to_string(),
            email: None,
            account_id: Some("chatgpt-quota-reset".to_string()),
            user_id: None,
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: secrecy::SecretString::new("access-token".to_string().into()),
            refresh_token: None,
            access_token_expires_at: Some(now + Duration::hours(1)),
            status: AccountStatus::QuotaExhausted,
            added_at: None,
        })
        .await
        .unwrap();
    sqlx::query(
        r#"
        update accounts
        set
          quota_limit_reached = true,
          quota_verify_required = false,
          quota_cooldown_until = $1,
          cloudflare_cooldown_until = $2
        where id = $3
        "#,
    )
    .bind(now - Duration::seconds(1))
    .bind(now - Duration::seconds(1))
    .bind("acct_quota_reset")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        insert into account_usage (
          account_id,
          request_count,
          window_request_count,
          window_input_tokens,
          window_output_tokens,
          window_cached_tokens,
          window_started_at,
          window_reset_at,
          limit_window_seconds
        ) values ($1, 9, 7, 11, 13, 17, $2, $3, 60)
        "#,
    )
    .bind("acct_quota_reset")
    .bind(now - Duration::seconds(120))
    .bind(now - Duration::seconds(1))
    .execute(&pool)
    .await
    .unwrap();
    let runtime_pool = codex_proxy_rs::fleet::pool::AccountPoolService::new(
        std::sync::Arc::new(store),
        std::sync::Arc::new(
            codex_proxy_rs::telemetry::account_usage::store::PgAccountUsageStore::new(pool.clone()),
        ),
        AccountPoolOptions::default(),
        0,
    );
    runtime_pool.restore_from_store().await.unwrap();

    let acquired = runtime_pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .await
        .unwrap();
    type StoredQuotaResetState = (
        String,
        bool,
        bool,
        Option<chrono::DateTime<Utc>>,
        Option<chrono::DateTime<Utc>>,
        i64,
        i64,
        i64,
        i64,
        Option<chrono::DateTime<Utc>>,
        Option<i64>,
    );
    let (
        stored_status,
        stored_quota_limit_reached,
        stored_quota_verify_required,
        stored_quota_cooldown_until,
        stored_cloudflare_cooldown_until,
        stored_window_request_count,
        stored_window_input_tokens,
        stored_window_output_tokens,
        stored_window_cached_tokens,
        stored_window_reset_at,
        stored_limit_window_seconds,
    ): StoredQuotaResetState = sqlx::query_as(
        r#"
        select
          a.status,
          a.quota_limit_reached,
          a.quota_verify_required,
          a.quota_cooldown_until,
          a.cloudflare_cooldown_until,
          au.window_request_count,
          au.window_input_tokens,
          au.window_output_tokens,
          au.window_cached_tokens,
          au.window_reset_at,
          au.limit_window_seconds
        from accounts a
        left join account_usage au on au.account_id = a.id
        where a.id = $1
        "#,
    )
    .bind("acct_quota_reset")
    .fetch_one(&pool)
    .await
    .unwrap();
    let persisted_reset_at = stored_window_reset_at;

    assert_eq!(acquired.account.id, "acct_quota_reset");
    assert!(acquired.account.quota_verify_required);
    assert_eq!(stored_status, "active");
    assert!(!stored_quota_limit_reached);
    assert!(stored_quota_verify_required);
    assert!(stored_quota_cooldown_until.is_none());
    assert!(stored_cloudflare_cooldown_until.is_none());
    assert_eq!(
        (
            stored_window_request_count,
            stored_window_input_tokens,
            stored_window_output_tokens,
            stored_window_cached_tokens,
        ),
        (0, 0, 0, 0)
    );
    assert!(persisted_reset_at.is_some_and(|reset_at| reset_at > now));
    assert_eq!(stored_limit_window_seconds, Some(60));
    let usage_record_count: i64 =
        sqlx::query_scalar("select count(*) from usage_records where account_id = $1")
            .bind("acct_quota_reset")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(usage_record_count, 0);
}

#[test]
fn account_pool_should_prefer_configured_tier_priority() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        tier_priority: vec!["team".to_string(), "plus".to_string()],
        ..AccountPoolOptions::default()
    });
    let mut free = crate::support::accounts::test_account("free", AccountStatus::Active);
    free.plan_type = Some("free".to_string());
    let mut team = crate::support::accounts::test_account("team", AccountStatus::Active);
    team.plan_type = Some("team".to_string());
    pool.insert(free);
    pool.insert(team);

    assert_eq!(acquire_account(&mut pool, "gpt-5.5").unwrap().id, "team");
}

#[test]
fn account_pool_should_filter_by_model_plan_allowlist() {
    let mut model_plans = BTreeMap::new();
    model_plans.insert("gpt-5.5".to_string(), vec!["plus".to_string()]);
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        model_plan_allowlist: model_plans,
        fetched_model_plan_types: BTreeSet::from(["free".to_string(), "plus".to_string()]),
        ..AccountPoolOptions::default()
    });
    let mut free = crate::support::accounts::test_account("free", AccountStatus::Active);
    free.plan_type = Some("free".to_string());
    let mut plus = crate::support::accounts::test_account("plus", AccountStatus::Active);
    plus.plan_type = Some("plus".to_string());
    pool.insert(free);
    pool.insert(plus);

    assert_eq!(acquire_account(&mut pool, "gpt-5.5").unwrap().id, "plus");
}

#[test]
fn account_pool_should_keep_unfetched_plan_when_model_allowlist_exists() {
    let mut model_plans = BTreeMap::new();
    model_plans.insert("gpt-5.5".to_string(), vec!["plus".to_string()]);
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        model_plan_allowlist: model_plans,
        fetched_model_plan_types: BTreeSet::from(["plus".to_string()]),
        ..AccountPoolOptions::default()
    });
    let mut free = crate::support::accounts::test_account("free", AccountStatus::Active);
    free.plan_type = Some("free".to_string());
    let mut plus = crate::support::accounts::test_account("plus", AccountStatus::Active);
    plus.plan_type = Some("plus".to_string());
    pool.insert(free);
    pool.insert(plus);

    assert_eq!(acquire_account(&mut pool, "gpt-5.5").unwrap().id, "free");
}

#[test]
fn account_pool_distinct_plan_accounts_should_filter_like_model_refresh() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        ..AccountPoolOptions::default()
    });
    let mut plus_limited =
        crate::support::accounts::test_account("plus-limited", AccountStatus::Active);
    plus_limited.plan_type = Some("plus".to_string());
    plus_limited.quota_limit_reached = true;
    plus_limited.quota_cooldown_until = Some(now + Duration::minutes(10));
    let mut plus_ok = crate::support::accounts::test_account("plus-ok", AccountStatus::Active);
    plus_ok.plan_type = Some("plus".to_string());
    let mut team_cf = crate::support::accounts::test_account("team-cf", AccountStatus::Active);
    team_cf.plan_type = Some("team".to_string());
    team_cf.cloudflare_cooldown_until = Some(now + Duration::minutes(10));
    pool.insert(plus_limited);
    pool.insert(plus_ok);
    pool.insert(team_cf);

    let selected = pool.distinct_plan_accounts(now);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].plan_type, "plus");
    assert_eq!(selected[0].account.id, "plus-ok");
    assert_eq!(
        pool.release_distinct_plan_account(&selected[0])
            .unwrap()
            .model,
        None
    );
}

#[test]
fn account_pool_should_exclude_requested_account_ids() {
    let mut pool = AccountPool::default();
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));

    let acquired = pool
        .acquire_with(
            &AccountAcquireRequest::new("gpt-5.5", fixed_time())
                .with_exclude_account_ids(["acct_a"]),
        )
        .unwrap();

    assert_eq!(acquired.account.id, "acct_b");
}

#[test]
fn account_pool_should_prefer_session_affinity_account_when_available() {
    let mut pool = AccountPool::default();
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));

    let acquired = pool
        .acquire_with(
            &AccountAcquireRequest::new("gpt-5.5", fixed_time())
                .with_preferred_account_id("acct_b"),
        )
        .unwrap();

    assert_eq!(acquired.account.id, "acct_b");
}

#[test]
fn account_pool_should_not_fallback_when_required_account_is_unavailable() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));

    assert!(pool
        .acquire_with(
            &AccountAcquireRequest::new("gpt-5.5", fixed_time()).with_required_account_id("acct_a"),
        )
        .is_some());
    let acquired = pool.acquire_with(
        &AccountAcquireRequest::new("gpt-5.5", fixed_time()).with_required_account_id("acct_a"),
    );

    assert!(acquired.is_none());
}

#[test]
fn account_pool_should_cleanup_stale_slots_before_acquire() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        stale_slot_ttl: Duration::minutes(5),
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));

    assert!(pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .is_some());
    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::minutes(5) + Duration::seconds(1),
        ))
        .unwrap();

    assert_eq!(acquired.account.id, "acct_a");
}

#[test]
fn smart_should_spread_concurrent_requests_by_slot_pressure() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 3,
        rotation_strategy: RotationStrategy::Smart,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_c",
        AccountStatus::Active,
    ));

    let first = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();
    let second = pool
        .acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::milliseconds(1),
        ))
        .unwrap();
    let third = pool
        .acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::milliseconds(2),
        ))
        .unwrap();

    assert_eq!(
        [first.account.id, second.account.id, third.account.id]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
}

#[test]
fn smart_should_balance_released_requests_by_runtime_usage() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::Smart,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));

    let first = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();
    pool.release(&first);
    let second = pool
        .acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::seconds(1),
        ))
        .unwrap();

    assert_ne!(first.account.id, second.account.id);
}

#[test]
fn smart_should_not_let_reset_window_override_lower_request_count() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::Smart,
        ..AccountPoolOptions::default()
    });
    let mut reset_soon =
        crate::support::accounts::test_account("reset_soon", AccountStatus::Active);
    reset_soon.request_count = 10;
    reset_soon.window_request_count = 10;
    reset_soon.window_reset_at = Some(now + Duration::seconds(30));
    let mut quieter = crate::support::accounts::test_account("quieter", AccountStatus::Active);
    quieter.request_count = 2;
    quieter.window_request_count = 2;
    quieter.window_reset_at = Some(now + Duration::seconds(300));
    pool.insert(reset_soon);
    pool.insert(quieter);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "quieter");
}

#[test]
fn smart_should_not_drain_earliest_reset_account_when_other_accounts_are_idle() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::Smart,
        ..AccountPoolOptions::default()
    });
    let mut reset_first =
        crate::support::accounts::test_account("reset_first", AccountStatus::Active);
    reset_first.window_reset_at = Some(now + Duration::seconds(30));
    let mut reset_later =
        crate::support::accounts::test_account("reset_later", AccountStatus::Active);
    reset_later.window_reset_at = Some(now + Duration::minutes(10));
    let mut reset_latest =
        crate::support::accounts::test_account("reset_latest", AccountStatus::Active);
    reset_latest.window_reset_at = Some(now + Duration::minutes(20));
    pool.insert(reset_first);
    pool.insert(reset_later);
    pool.insert(reset_latest);

    let mut selected_ids = Vec::new();
    for offset in 0..4 {
        let acquired = pool
            .acquire_with(&AccountAcquireRequest::new(
                "gpt-5.5",
                now + Duration::seconds(offset),
            ))
            .unwrap();
        selected_ids.push(acquired.account.id.clone());
        pool.release(&acquired);
    }

    assert_eq!(
        selected_ids
            .iter()
            .take(3)
            .cloned()
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    assert!(selected_ids.windows(2).all(|pair| pair[0] != pair[1]));
}

#[test]
fn smart_should_use_window_token_load_when_request_counts_tie() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::Smart,
        ..AccountPoolOptions::default()
    });
    let mut heavy = crate::support::accounts::test_account("heavy", AccountStatus::Active);
    heavy.request_count = 5;
    heavy.window_request_count = 3;
    heavy.window_input_tokens = 1_000_000;
    heavy.window_output_tokens = 500_000;
    let mut light = crate::support::accounts::test_account("light", AccountStatus::Active);
    light.request_count = 5;
    light.window_request_count = 3;
    light.window_input_tokens = 8_000;
    light.window_output_tokens = 2_000;
    pool.insert(heavy);
    pool.insert(light);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "light");
}

#[test]
fn smart_should_not_double_count_cached_tokens_when_request_counts_tie() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::Smart,
        ..AccountPoolOptions::default()
    });
    let mut cache_heavy =
        crate::support::accounts::test_account("cache_heavy", AccountStatus::Active);
    cache_heavy.request_count = 5;
    cache_heavy.window_request_count = 3;
    cache_heavy.window_input_tokens = 10_000;
    cache_heavy.window_output_tokens = 1_000;
    cache_heavy.window_cached_tokens = 100_000;
    let mut heavier_actual =
        crate::support::accounts::test_account("heavier_actual", AccountStatus::Active);
    heavier_actual.request_count = 5;
    heavier_actual.window_request_count = 3;
    heavier_actual.window_input_tokens = 20_000;
    heavier_actual.window_output_tokens = 1_000;
    pool.insert(cache_heavy);
    pool.insert(heavier_actual);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "cache_heavy");
}

#[test]
fn smart_should_use_cached_tokens_as_tie_break_when_actual_token_load_ties() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::Smart,
        ..AccountPoolOptions::default()
    });
    let mut cache_heavy =
        crate::support::accounts::test_account("cache_heavy", AccountStatus::Active);
    cache_heavy.request_count = 5;
    cache_heavy.window_request_count = 3;
    cache_heavy.window_input_tokens = 10_000;
    cache_heavy.window_output_tokens = 1_000;
    cache_heavy.window_cached_tokens = 100_000;
    let mut cache_light =
        crate::support::accounts::test_account("cache_light", AccountStatus::Active);
    cache_light.request_count = 5;
    cache_light.window_request_count = 3;
    cache_light.window_input_tokens = 10_000;
    cache_light.window_output_tokens = 1_000;
    cache_light.window_cached_tokens = 1_000;
    pool.insert(cache_heavy);
    pool.insert(cache_light);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "cache_light");
}

#[test]
fn account_pool_should_rotate_tied_quota_reset_priority_accounts() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        rotation_strategy: RotationStrategy::QuotaResetPriority,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Active,
    ));

    let first = acquire_account(&mut pool, "gpt-5.5").unwrap();
    pool.release(&first);
    let second = acquire_account(&mut pool, "gpt-5.5").unwrap();

    assert_ne!(first.id, second.id);
}

#[test]
fn quota_reset_priority_should_prefer_lru_before_tie_rotation() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        rotation_strategy: RotationStrategy::QuotaResetPriority,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "seed",
        AccountStatus::Active,
    ));
    let mut older = crate::support::accounts::test_account("older", AccountStatus::Active);
    older.last_used_at = Some((now - Duration::minutes(10)).to_rfc3339());
    let mut newer = crate::support::accounts::test_account("newer", AccountStatus::Active);
    newer.last_used_at = Some((now - Duration::minutes(1)).to_rfc3339());
    pool.insert(older);
    pool.insert(newer);

    let seed = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();
    assert_eq!(seed.account.id, "seed");
    pool.release(&seed);
    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::seconds(1),
        ))
        .unwrap();

    assert_eq!(acquired.account.id, "older");
}

#[test]
fn quota_reset_priority_should_prefer_lower_runtime_request_count() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::QuotaResetPriority,
        ..AccountPoolOptions::default()
    });
    let mut busy = crate::support::accounts::test_account("busy", AccountStatus::Active);
    busy.request_count = 10;
    let mut quiet = crate::support::accounts::test_account("quiet", AccountStatus::Active);
    quiet.request_count = 2;
    pool.insert(busy);
    pool.insert(quiet);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "quiet");
}

#[test]
fn quota_reset_priority_should_not_penalize_missing_window_reset() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::QuotaResetPriority,
        ..AccountPoolOptions::default()
    });
    let mut fresh = crate::support::accounts::test_account("fresh", AccountStatus::Active);
    fresh.request_count = 1;
    let mut known_window =
        crate::support::accounts::test_account("known_window", AccountStatus::Active);
    known_window.request_count = 5;
    known_window.window_reset_at = Some(now + Duration::days(1));
    pool.insert(known_window);
    pool.insert(fresh);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "fresh");
}

#[test]
fn quota_reset_priority_should_fall_through_to_request_count_without_window_resets() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::QuotaResetPriority,
        ..AccountPoolOptions::default()
    });
    let mut busier = crate::support::accounts::test_account("busier", AccountStatus::Active);
    busier.request_count = 3;
    let mut quieter = crate::support::accounts::test_account("quieter", AccountStatus::Active);
    quieter.request_count = 1;
    pool.insert(busier);
    pool.insert(quieter);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "quieter");
}

#[test]
fn quota_reset_priority_should_sort_quota_limited_accounts_by_reset_when_not_skipping() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        skip_quota_limited: false,
        rotation_strategy: RotationStrategy::QuotaResetPriority,
        ..AccountPoolOptions::default()
    });
    let mut later = crate::support::accounts::test_account("later", AccountStatus::Active);
    later.quota_limit_reached = true;
    later.window_reset_at = Some(now + Duration::days(3));
    let mut sooner = crate::support::accounts::test_account("sooner", AccountStatus::Active);
    sooner.quota_limit_reached = true;
    sooner.window_reset_at = Some(now + Duration::days(1));
    pool.insert(later);
    pool.insert(sooner);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "sooner");
}

#[test]
fn quota_reset_priority_should_not_mutate_candidate_order() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::QuotaResetPriority,
        ..AccountPoolOptions::default()
    });
    let mut first = crate::support::accounts::test_account("first", AccountStatus::Active);
    first.request_count = 5;
    let mut second = crate::support::accounts::test_account("second", AccountStatus::Active);
    second.request_count = 3;
    let mut third = crate::support::accounts::test_account("third", AccountStatus::Active);
    third.request_count = 1;
    pool.insert(first);
    pool.insert(second);
    pool.insert(third);

    let selected = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();
    pool.release(&selected);
    pool.set_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::RoundRobin,
        ..AccountPoolOptions::default()
    });
    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::seconds(1),
        ))
        .unwrap();

    assert_eq!(acquired.account.id, "first");
}
