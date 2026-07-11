use super::*;

#[test]
fn account_pool_should_return_previous_slot_time_for_request_staggering() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 2,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));

    let first = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();
    let second = pool
        .acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::milliseconds(250),
        ))
        .unwrap();

    assert!(first.previous_slot_at.is_none());
    assert_eq!(second.previous_slot_at, Some(now));
}

#[test]
fn account_pool_should_report_capacity_summary() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 2,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));
    pool.insert(crate::support::accounts::test_account(
        "acct_b",
        AccountStatus::Disabled,
    ));

    pool.acquire_with(&AccountAcquireRequest::new("gpt-5.5", now));
    let summary = pool.capacity_summary(now);

    assert_eq!(summary.total_slots, 2);
    assert_eq!(summary.used_slots, 1);
    assert_eq!(summary.available_slots, 1);
}

#[test]
fn capacity_summary_should_not_exclude_cloudflare_cooldown_accounts() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 2,
        ..AccountPoolOptions::default()
    });
    let mut account = crate::support::accounts::test_account("acct_cf", AccountStatus::Active);
    account.cloudflare_cooldown_until = Some(now + Duration::minutes(10));
    pool.insert(account);

    let summary = pool.capacity_summary(now);

    assert_eq!(summary.total_slots, 2);
    assert_eq!(summary.available_slots, 2);
}

#[test]
fn quota_reset_priority_should_compare_request_count_when_only_one_account_has_window_reset() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::QuotaResetPriority,
        ..AccountPoolOptions::default()
    });
    let mut reset_known =
        crate::support::accounts::test_account("reset_known", AccountStatus::Active);
    reset_known.window_reset_at = Some(now + Duration::seconds(30));
    reset_known.request_count = 10;
    let mut reset_unknown =
        crate::support::accounts::test_account("reset_unknown", AccountStatus::Active);
    reset_unknown.request_count = 2;
    pool.insert(reset_known);
    pool.insert(reset_unknown);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "reset_unknown");
}

#[test]
fn release_should_remove_the_exact_concurrent_slot() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 2,
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));

    let first = pool
        .acquire_with(&AccountAcquireRequest::new("model-first", now))
        .unwrap();
    let second = pool
        .acquire_with(&AccountAcquireRequest::new(
            "model-second",
            now + Duration::milliseconds(1),
        ))
        .unwrap();

    assert_eq!(
        pool.release(&second).unwrap().model.as_deref(),
        Some("model-second")
    );
    assert_eq!(
        pool.release(&first).unwrap().model.as_deref(),
        Some("model-first")
    );
}

#[test]
fn release_should_mark_runtime_usage_window() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    let mut account = crate::support::accounts::test_account("acct_a", AccountStatus::Active);
    account.limit_window_seconds = Some(60);
    pool.insert(account);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.request_count, 0);
    assert_eq!(acquired.account.window_request_count, 0);

    pool.release(&acquired);
    let released = pool.get(&acquired.account.id).unwrap();

    assert_eq!(released.request_count, 1);
    assert_eq!(released.window_request_count, 1);
    assert!(released.window_started_at.is_some());
    assert!(released.window_reset_at.is_some());
    assert!(released.last_used_at.is_some());
}

#[test]
fn release_should_start_usage_window_without_known_reset() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    pool.release(&acquired);
    let released = pool.get(&acquired.account.id).unwrap();

    assert!(released.window_started_at.is_some());
    assert!(released.window_reset_at.is_none());
}

#[test]
fn release_should_mark_usage_after_stale_slot_cleanup() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        stale_slot_ttl: Duration::minutes(5),
        ..AccountPoolOptions::default()
    });
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    let summary = pool.capacity_summary(now + Duration::minutes(6));
    pool.release(&acquired);
    let released = pool.get(&acquired.account.id).unwrap();

    assert_eq!(summary.used_slots, 0);
    assert_eq!(released.request_count, 1);
    assert_eq!(released.window_request_count, 1);
}

#[test]
fn record_window_token_usage_should_accumulate_runtime_window_tokens() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    pool.insert(crate::support::accounts::test_account(
        "acct_a",
        AccountStatus::Active,
    ));

    pool.record_window_token_usage(
        "acct_a",
        AccountWindowUsageDelta {
            input_tokens: 7,
            output_tokens: 4,
            cached_tokens: 2,
            ..AccountWindowUsageDelta::default()
        },
    );
    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.window_input_tokens, 7);
    assert_eq!(acquired.account.window_output_tokens, 4);
    assert_eq!(acquired.account.window_cached_tokens, 2);
}
