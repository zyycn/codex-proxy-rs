use std::collections::BTreeMap;

use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::accounts::{
    model::{Account, AccountStatus},
    pool::{AccountAcquireRequest, AccountPool, AccountPoolOptions, RotationStrategy},
};

#[test]
fn account_pool_should_respect_max_concurrent_slots_per_account() {
    let mut pool = AccountPool::with_options(AccountPoolOptions {
        max_concurrent_per_account: 1,
        ..AccountPoolOptions::default()
    });
    pool.insert(Account::test("acct_a", AccountStatus::Active));
    pool.insert(Account::test("acct_b", AccountStatus::Active));

    let first = pool.acquire("gpt-5.4").unwrap();
    let second = pool.acquire("gpt-5.4").unwrap();
    let third = pool.acquire("gpt-5.4");

    assert_ne!(first.id, second.id);
    assert!(third.is_none());

    pool.release(&first.id);
    assert_eq!(pool.acquire("gpt-5.4").unwrap().id, first.id);
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

    assert_eq!(pool.acquire("gpt-5.4").unwrap().id, "acct_a");
    assert_eq!(pool.acquire("gpt-5.4").unwrap().id, "acct_c");
    assert_eq!(pool.acquire("gpt-5.4").unwrap().id, "acct_b");
    assert!(pool.acquire("gpt-5.4").is_none());
}

#[test]
fn account_pool_should_skip_accounts_with_cached_quota_limit() {
    let mut limited = Account::test("limited", AccountStatus::Active);
    limited.quota_limit_reached = true;
    let mut pool = AccountPool::default();
    pool.insert(limited);
    pool.insert(Account::test("usable", AccountStatus::Active));

    assert_eq!(pool.acquire("gpt-5.4").unwrap().id, "usable");
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

    assert_eq!(pool.acquire("gpt-5.4").unwrap().id, "team");
}

#[test]
fn account_pool_should_filter_by_model_plan_allowlist() {
    let mut model_plans = BTreeMap::new();
    model_plans.insert("gpt-5.4".to_string(), vec!["plus".to_string()]);
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

    assert_eq!(pool.acquire("gpt-5.4").unwrap().id, "plus");
}

#[test]
fn account_pool_should_exclude_requested_account_ids() {
    let mut pool = AccountPool::default();
    pool.insert(Account::test("acct_a", AccountStatus::Active));
    pool.insert(Account::test("acct_b", AccountStatus::Active));

    let acquired = pool
        .acquire_with(
            AccountAcquireRequest::new("gpt-5.4", fixed_time())
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
            AccountAcquireRequest::new("gpt-5.4", fixed_time()).with_preferred_account_id("acct_b"),
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
        .acquire_with(AccountAcquireRequest::new("gpt-5.4", now))
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
        .acquire_with(AccountAcquireRequest::new("gpt-5.4", now))
        .is_some());
    let acquired = pool
        .acquire_with(AccountAcquireRequest::new(
            "gpt-5.4",
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
        .acquire_with(AccountAcquireRequest::new("gpt-5.4", now))
        .unwrap();
    let second = pool
        .acquire_with(AccountAcquireRequest::new(
            "gpt-5.4",
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

    pool.acquire_with(AccountAcquireRequest::new("gpt-5.4", now));
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

    let first = pool.acquire("gpt-5.4").unwrap();
    pool.release(&first.id);
    let second = pool.acquire("gpt-5.4").unwrap();

    assert_ne!(first.id, second.id);
}

fn fixed_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 11, 8, 0, 0).unwrap()
}
