use codex_proxy_rs::accounts::{
    model::{Account, AccountStatus},
    pool::AccountPool,
};

#[test]
fn account_pool_skips_expired_disabled_banned_and_quota_exhausted_accounts() {
    let mut pool = AccountPool::default();
    pool.insert(Account::test("active", AccountStatus::Active));
    pool.insert(Account::test("expired", AccountStatus::Expired));
    pool.insert(Account::test("disabled", AccountStatus::Disabled));
    pool.insert(Account::test("banned", AccountStatus::Banned));
    pool.insert(Account::test("quota", AccountStatus::QuotaExhausted));

    let acquired = pool.acquire("gpt-5.5").unwrap();
    assert_eq!(acquired.id, "active");
}
