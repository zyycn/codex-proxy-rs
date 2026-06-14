use std::collections::BTreeMap;

use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::codex::accounts::{
    model::{Account, AccountStatus},
    pool::{AccountAcquireRequest, AccountPool, AccountPoolOptions, RotationStrategy},
};

use crate::support::admin_accounts::test_jwt;

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
fn account_pool_should_skip_accounts_with_cached_quota_limit() {
    let mut limited = Account::test("limited", AccountStatus::Active);
    limited.quota_limit_reached = true;
    let mut pool = AccountPool::default();
    pool.insert(limited);
    pool.insert(Account::test("usable", AccountStatus::Active));

    assert_eq!(pool.acquire("gpt-5.5").unwrap().id, "usable");
}

#[test]
fn account_pool_should_reuse_quota_limited_accounts_after_cooldown() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    pool.insert(Account::test("limited", AccountStatus::Active));
    pool.mark_quota_limited_until("limited", now + Duration::seconds(30));

    assert!(pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .is_none());
    assert_eq!(
        pool.acquire_with(AccountAcquireRequest::new(
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
    pool.insert(Account::test("limited", AccountStatus::Active));

    pool.mark_quota_limited_until("limited", now + Duration::seconds(180));
    pool.mark_quota_limited_until("limited", now + Duration::seconds(60));

    assert!(pool
        .acquire_with(AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::seconds(90)
        ))
        .is_none());
}

#[test]
fn account_pool_should_not_replace_known_window_length_with_cooldown_seconds() {
    let now = Utc::now();
    let mut account = Account::test("limited", AccountStatus::Active);
    account.limit_window_seconds = Some(300);
    let mut pool = AccountPool::default();
    pool.insert(account);

    pool.mark_quota_limited_until("limited", now + Duration::seconds(60));
    let acquired = pool
        .acquire_with(AccountAcquireRequest::new(
            "gpt-5.5",
            now + Duration::seconds(61),
        ))
        .unwrap();

    assert_eq!(acquired.account.limit_window_seconds, Some(300));
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
    expired.access_token = test_jwt(None, None, None, None, -60);
    expired.access_token_expires_at = None;
    let mut pool = AccountPool::default();
    pool.insert(expired);

    let acquired = pool.acquire_with(AccountAcquireRequest::new("gpt-5.5", now));

    assert!(acquired.is_none());
}

#[test]
fn acquire_should_refresh_expired_cooldowns_before_selecting_account() {
    let now = fixed_time();
    let mut account = Account::test("acct_a", AccountStatus::Active);
    account.quota_limit_reached = true;
    account.quota_cooldown_until = Some(now - Duration::seconds(1));
    account.cloudflare_cooldown_until = Some(now - Duration::seconds(1));
    account.window_request_count = 7;
    account.window_reset_at = Some(now - Duration::seconds(1));
    account.limit_window_seconds = Some(60);
    let mut pool = AccountPool::default();
    pool.insert(account);

    let acquired = pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert!(!acquired.account.quota_limit_reached);
    assert!(acquired.account.quota_cooldown_until.is_none());
    assert!(acquired.account.cloudflare_cooldown_until.is_none());
    assert_eq!(acquired.account.window_request_count, 1);
    assert!(acquired
        .account
        .window_reset_at
        .is_some_and(|reset| reset > now));
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
fn account_pool_should_skip_accounts_in_cloudflare_cooldown() {
    let now = fixed_time();
    let mut cooling = Account::test("cooling", AccountStatus::Active);
    cooling.cloudflare_cooldown_until = Some(now + Duration::seconds(30));
    let mut pool = AccountPool::default();
    pool.insert(cooling);
    pool.insert(Account::test("usable", AccountStatus::Active));

    let acquired = pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "usable");
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
fn account_pool_should_return_previous_slot_time_for_request_staggering() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 2,
        ..AccountPoolOptions::default()
    });
    pool.insert(Account::test("acct_a", AccountStatus::Active));

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
    pool.insert(Account::test("acct_a", AccountStatus::Active));
    pool.insert(Account::test("acct_b", AccountStatus::Disabled));

    pool.acquire_with(AccountAcquireRequest::new("gpt-5.5", now));
    let summary = pool.capacity_summary(now);

    assert_eq!(summary.total_slots, 2);
    assert_eq!(summary.used_slots, 1);
    assert_eq!(summary.available_slots, 1);
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
fn least_used_should_deprioritize_quota_limited_accounts_when_skip_is_disabled() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        skip_quota_limited: false,
        rotation_strategy: RotationStrategy::LeastUsed,
        ..AccountPoolOptions::default()
    });
    let mut limited = Account::test("limited", AccountStatus::Active);
    limited.quota_limit_reached = true;
    let mut usable = Account::test("usable", AccountStatus::Active);
    usable.request_count = 100;
    pool.insert(limited);
    pool.insert(usable);

    let acquired = pool.acquire("gpt-5.5").unwrap();

    assert_eq!(acquired.id, "usable");
}

#[test]
fn least_used_should_prefer_earlier_rate_limit_window_reset() {
    let now = fixed_time();
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        rotation_strategy: RotationStrategy::LeastUsed,
        ..AccountPoolOptions::default()
    });
    let mut soon = Account::test("soon", AccountStatus::Active);
    soon.window_reset_at = Some(now + Duration::seconds(30));
    let mut later = Account::test("later", AccountStatus::Active);
    later.window_reset_at = Some(now + Duration::seconds(300));
    pool.insert(later);
    pool.insert(soon);

    let acquired = pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.id, "soon");
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

#[test]
fn acquire_should_mark_runtime_usage_window() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    let mut account = Account::test("acct_a", AccountStatus::Active);
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
fn sync_rate_limit_window_should_reset_window_counters_when_drift_crosses_threshold() {
    let now = fixed_time();
    let mut account = Account::test("acct_a", AccountStatus::Active);
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
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
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

#[test]
fn record_window_token_usage_should_accumulate_runtime_window_tokens() {
    let now = fixed_time();
    let mut pool = AccountPool::default();
    pool.insert(Account::test("acct_a", AccountStatus::Active));

    pool.record_window_token_usage("acct_a", 7, 4, 2);
    let acquired = pool
        .acquire_with(AccountAcquireRequest::new("gpt-5.5", now))
        .unwrap();

    assert_eq!(acquired.account.window_input_tokens, 7);
    assert_eq!(acquired.account.window_output_tokens, 4);
    assert_eq!(acquired.account.window_cached_tokens, 2);
}

fn fixed_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 11, 8, 0, 0).unwrap()
}
