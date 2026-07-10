use chrono::{Duration, Utc};
use codex_proxy_rs::fleet::refresh::RedisRefreshLeaseStore;

use crate::support::storage::create_test_redis;

#[tokio::test]
async fn redis_refresh_lease_acquire_renew_and_compare_release_are_atomic() {
    let leases = RedisRefreshLeaseStore::new(create_test_redis("refresh-lease-atomic").await);
    let now = Utc::now();

    assert!(leases
        .try_acquire("acct-lease", "owner-a", now + Duration::minutes(5), now)
        .await
        .unwrap());
    assert!(!leases
        .try_acquire("acct-lease", "owner-b", now + Duration::minutes(5), now)
        .await
        .unwrap());
    assert!(leases
        .try_acquire("acct-lease", "owner-a", now + Duration::minutes(10), now,)
        .await
        .unwrap());
    assert!(!leases.release("acct-lease", "owner-b").await.unwrap());
    assert!(leases.release("acct-lease", "owner-a").await.unwrap());
    assert!(leases
        .try_acquire("acct-lease", "owner-b", now + Duration::minutes(5), now)
        .await
        .unwrap());
}

#[tokio::test]
async fn redis_refresh_lease_lists_only_existing_ttl_keys() {
    let leases = RedisRefreshLeaseStore::new(create_test_redis("refresh-lease-list").await);
    let now = Utc::now();
    assert!(leases
        .try_acquire("acct-active", "owner", now + Duration::minutes(5), now)
        .await
        .unwrap());
    assert!(!leases
        .try_acquire("acct-expired", "owner", now - Duration::seconds(1), now)
        .await
        .unwrap());

    let ids = vec![
        "acct-active".to_string(),
        "acct-expired".to_string(),
        "acct-missing".to_string(),
    ];
    let active = leases.active_account_ids(&ids, now).await.unwrap();
    assert_eq!(
        active,
        std::collections::HashSet::from(["acct-active".to_string()])
    );
}
