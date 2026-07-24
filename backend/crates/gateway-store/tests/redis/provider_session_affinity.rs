use std::time::Duration;

use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::provider_ports::{ProviderSessionAffinityKey, ProviderSessionAffinityPort};
use gateway_core::routing::ProviderKind;
use gateway_store::redis::RedisProviderSessionAffinityRepository;
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[tokio::test]
async fn session_affinity_should_round_trip_without_exposing_the_raw_session_key() {
    let Some((repository, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let key = ProviderSessionAffinityKey::try_new("session-secret-value").expect("affinity key");
    let account = ProviderAccountId::new("acct_first").expect("account");

    repository
        .bind(&provider, &key, &account, Duration::from_secs(60))
        .await
        .expect("bind affinity");

    assert_eq!(
        repository
            .load(&provider, &key)
            .await
            .expect("load affinity"),
        Some(account)
    );
    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list affinity keys");
    assert_eq!(keys.len(), 1);
    assert!(!keys[0].contains("session-secret-value"));
}

#[tokio::test]
async fn session_affinity_should_overwrite_the_previous_account() {
    let Some((repository, _connection, _namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let key = ProviderSessionAffinityKey::try_new("overwrite-session").expect("affinity key");
    let first = ProviderAccountId::new("acct_first").expect("first account");
    let second = ProviderAccountId::new("acct_second").expect("second account");

    repository
        .bind(&provider, &key, &first, Duration::from_secs(60))
        .await
        .expect("bind first affinity");
    repository
        .bind(&provider, &key, &second, Duration::from_secs(60))
        .await
        .expect("overwrite affinity");

    assert_eq!(
        repository
            .load(&provider, &key)
            .await
            .expect("load affinity"),
        Some(second)
    );
}

#[tokio::test]
async fn session_affinity_should_apply_ttl_and_support_explicit_clear() {
    let Some((repository, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let key = ProviderSessionAffinityKey::try_new("ttl-session").expect("affinity key");
    let account = ProviderAccountId::new("acct_ttl").expect("account");

    repository
        .bind(&provider, &key, &account, Duration::from_secs(60))
        .await
        .expect("bind affinity");
    let redis_key = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list affinity keys")
        .into_iter()
        .next()
        .expect("affinity key exists");
    let ttl = redis::cmd("PTTL")
        .arg(redis_key)
        .query_async::<i64>(&mut connection)
        .await
        .expect("read affinity TTL");
    assert!((1..=60_000).contains(&ttl));

    assert!(
        repository
            .clear(&provider, &key)
            .await
            .expect("clear affinity")
    );
    assert_eq!(
        repository
            .load(&provider, &key)
            .await
            .expect("load cleared affinity"),
        None
    );
}

async fn affinity_repository() -> Option<(
    RedisProviderSessionAffinityRepository,
    ConnectionManager,
    String,
)> {
    let redis_url = std::env::var("CPR_TEST_REDIS_URL").ok()?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-affinity-test-{}", Uuid::new_v4());
    let repository = RedisProviderSessionAffinityRepository::new(connection.clone(), &namespace)
        .expect("valid test namespace");
    Some((repository, connection, namespace))
}
