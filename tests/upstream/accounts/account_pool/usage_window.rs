use super::*;

#[test]
fn reset_usage_should_clear_runtime_counters_and_preserve_window_reset() {
    let now = fixed_time();
    let mut account = crate::support::accounts::test_account("acct_a", AccountStatus::Active);
    account.request_count = 7;
    account.empty_response_count = 2;
    account.image_input_tokens = 11;
    account.image_output_tokens = 13;
    account.image_request_count = 3;
    account.image_request_failed_count = 1;
    account.window_request_count = 5;
    account.window_input_tokens = 19;
    account.window_output_tokens = 23;
    account.window_cached_tokens = 29;
    account.window_image_input_tokens = 31;
    account.window_image_output_tokens = 37;
    account.window_image_request_count = 2;
    account.window_image_request_failed_count = 1;
    account.window_reset_at = Some(now + Duration::seconds(300));
    account.limit_window_seconds = Some(300);
    account.last_used_at = Some("2026-06-12T12:00:00Z".to_string());
    let mut pool = AccountPool::default();
    pool.insert(account);

    assert!(pool.reset_usage("acct_a"));
    assert!(!pool.reset_usage("missing"));

    let acquired = pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap()
        .account;
    assert_eq!(acquired.request_count, 1);
    assert_eq!(acquired.empty_response_count, 0);
    assert_eq!(acquired.image_input_tokens, 0);
    assert_eq!(acquired.image_output_tokens, 0);
    assert_eq!(acquired.image_request_count, 0);
    assert_eq!(acquired.image_request_failed_count, 0);
    assert_eq!(acquired.window_request_count, 1);
    assert_eq!(acquired.window_input_tokens, 0);
    assert_eq!(acquired.window_output_tokens, 0);
    assert_eq!(acquired.window_cached_tokens, 0);
    assert_eq!(acquired.window_image_input_tokens, 0);
    assert_eq!(acquired.window_image_output_tokens, 0);
    assert_eq!(acquired.window_image_request_count, 0);
    assert_eq!(acquired.window_image_request_failed_count, 0);
    assert_eq!(acquired.window_reset_at, Some(now + Duration::seconds(300)));
    assert_ne!(
        acquired.last_used_at.as_deref(),
        Some("2026-06-12T12:00:00Z")
    );
}

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
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();
    let second = pool
        .acquire_with(AccountAcquireRequest::new(
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

    pool.acquire_with(AccountAcquireRequest::new("gpt-5.5", now));
    let summary = pool.capacity_summary(now);

    assert_eq!(summary.total_slots, 2);
    assert_eq!(summary.used_slots, 1);
    assert_eq!(summary.available_slots, 1);
}

#[test]
fn least_used_should_compare_request_count_when_only_one_account_has_window_reset() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::LeastUsed,
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
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "reset_unknown");
}

#[test]
fn acquire_should_mark_runtime_usage_window() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    let mut account = crate::support::accounts::test_account("acct_a", AccountStatus::Active);
    account.limit_window_seconds = Some(60);
    pool.insert(account);

    let acquired = pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.request_count, 1);
    assert_eq!(acquired.account.window_request_count, 1);
    assert_eq!(acquired.account.window_started_at, Some(now));
    assert_eq!(
        acquired.account.window_reset_at,
        Some(now + Duration::seconds(60))
    );
    assert_eq!(
        acquired.account.last_used_at.as_deref(),
        Some(now.to_rfc3339().as_str())
    );
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
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.window_input_tokens, 7);
    assert_eq!(acquired.account.window_output_tokens, 4);
    assert_eq!(acquired.account.window_cached_tokens, 2);
}
