use super::*;

#[test]
fn account_pool_should_skip_accounts_with_cached_quota_limit() {
    let mut limited = crate::support::accounts::test_account("limited", AccountStatus::Active);
    limited.quota_limit_reached = true;
    let mut pool = AccountPool::default();
    pool.insert(limited);
    pool.insert(crate::support::accounts::test_account(
        "usable",
        AccountStatus::Active,
    ));

    assert_eq!(acquire_account(&mut pool, "gpt-5.5").unwrap().id, "usable");
}

#[test]
fn account_pool_should_mark_quota_state_as_quota_exhausted_status() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    pool.insert(crate::support::accounts::test_account(
        "limited",
        AccountStatus::Active,
    ));

    pool.apply_quota_state("limited", true, Some(now + Duration::seconds(60)));

    let limited = pool.get("limited").unwrap();
    assert_eq!(limited.status, AccountStatus::QuotaExhausted);
    assert!(limited.quota_limit_reached);
    assert!(pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .is_none());

    pool.apply_quota_state("limited", false, None);

    let restored = pool.get("limited").unwrap();
    assert_eq!(restored.status, AccountStatus::Active);
    assert!(!restored.quota_limit_reached);
}

#[test]
fn account_pool_should_reuse_quota_limited_accounts_after_cooldown() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    pool.insert(crate::support::accounts::test_account(
        "limited",
        AccountStatus::Active,
    ));
    pool.mark_quota_limited_until("limited", now + Duration::seconds(30));

    assert!(pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .is_none());
    assert_eq!(
        pool.acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::seconds(31)
        ))
        .unwrap()
        .account
        .id,
        "limited"
    );
}

#[test]
fn account_pool_should_not_shorten_existing_quota_cooldown() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    pool.insert(crate::support::accounts::test_account(
        "limited",
        AccountStatus::Active,
    ));

    pool.mark_quota_limited_until("limited", now + Duration::seconds(180));
    pool.mark_quota_limited_until("limited", now + Duration::seconds(60));

    assert!(pool
        .acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::seconds(90)
        ))
        .is_none());
}

#[test]
fn account_pool_should_not_replace_known_window_length_with_cooldown_seconds() {
    let now = Utc::now();
    let mut account = crate::support::accounts::test_account("limited", AccountStatus::Active);
    account.limit_window_seconds = Some(300);
    let mut pool = AccountPool::default();
    pool.insert(account);

    pool.mark_quota_limited_until("limited", now + Duration::seconds(60));
    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::seconds(61),
        ))
        .unwrap();

    assert_eq!(acquired.account.limit_window_seconds, Some(300));
}

#[test]
fn acquire_should_refresh_expired_cooldowns_before_selecting_account() {
    let now = fixed_time();
    let mut account =
        crate::support::accounts::test_account("acct_a", AccountStatus::QuotaExhausted);
    account.quota_limit_reached = true;
    account.quota_cooldown_until = Some(now - Duration::seconds(1));
    account.cloudflare_cooldown_until = Some(now - Duration::seconds(1));
    account.window_request_count = 7;
    account.window_reset_at = Some(now - Duration::seconds(1));
    account.limit_window_seconds = Some(60);
    let mut pool = AccountPool::default();
    pool.insert(account);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert!(!acquired.account.quota_limit_reached);
    assert!(acquired.account.quota_verify_required);
    assert_eq!(acquired.account.status, AccountStatus::Active);
    assert!(acquired.account.quota_cooldown_until.is_none());
    assert!(acquired.account.cloudflare_cooldown_until.is_none());
    assert_eq!(acquired.account.window_request_count, 1);
    assert!(acquired
        .account
        .window_reset_at
        .is_some_and(|reset| reset > now));
}

#[test]
fn account_pool_should_skip_accounts_in_cloudflare_cooldown() {
    let now = fixed_time();
    let mut cooling = crate::support::accounts::test_account("cooling", AccountStatus::Active);
    cooling.cloudflare_cooldown_until = Some(now + Duration::seconds(30));
    let mut pool = AccountPool::default();
    pool.insert(cooling);
    pool.insert(crate::support::accounts::test_account(
        "usable",
        AccountStatus::Active,
    ));

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "usable");
}

#[test]
fn least_used_should_deprioritize_quota_limited_accounts_when_skip_is_disabled() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        skip_quota_limited: false,
        rotation_strategy: RotationStrategy::LeastUsed,
        ..AccountPoolOptions::default()
    });
    let mut limited = crate::support::accounts::test_account("limited", AccountStatus::Active);
    limited.quota_limit_reached = true;
    let mut usable = crate::support::accounts::test_account("usable", AccountStatus::Active);
    usable.request_count = 100;
    pool.insert(limited);
    pool.insert(usable);

    let acquired = acquire_account(&mut pool, "gpt-5.5").unwrap();

    assert_eq!(acquired.id, "usable");
}

#[test]
fn least_used_should_prefer_earlier_rate_limit_window_reset() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::LeastUsed,
        ..AccountPoolOptions::default()
    });
    let mut soon = crate::support::accounts::test_account("soon", AccountStatus::Active);
    soon.window_reset_at = Some(now + Duration::seconds(30));
    let mut later = crate::support::accounts::test_account("later", AccountStatus::Active);
    later.window_reset_at = Some(now + Duration::seconds(300));
    pool.insert(later);
    pool.insert(soon);

    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "soon");
}

#[test]
fn sync_rate_limit_window_should_reset_window_counters_when_drift_crosses_threshold() {
    let now = fixed_time();
    let mut account = crate::support::accounts::test_account("acct_a", AccountStatus::Active);
    account.window_request_count = 5;
    account.window_input_tokens = 100;
    account.window_output_tokens = 40;
    account.window_cached_tokens = 20;
    account.window_reset_at = Some(now + Duration::seconds(60));
    account.limit_window_seconds = Some(60);
    let mut pool = AccountPool::default();
    pool.insert(account);

    pool.sync_rate_limit_window("acct_a", now + Duration::seconds(121), Some(60));
    let acquired = pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.window_request_count, 1);
    assert_eq!(acquired.account.window_input_tokens, 0);
    assert_eq!(acquired.account.window_output_tokens, 0);
    assert_eq!(acquired.account.window_cached_tokens, 0);
    assert_eq!(
        acquired.account.window_reset_at,
        Some(now + Duration::seconds(121))
    );
}
