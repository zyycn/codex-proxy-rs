use std::time::Duration;

use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::provider_ports::{ProviderSessionAffinityKey, ProviderSessionExclusionPort};
use gateway_core::routing::ProviderKind;
use gateway_store::redis::RedisProviderSessionExclusionRepository;
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[tokio::test]
async fn session_exclusion_should_record_each_failed_account_for_one_hour() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let session = ProviderSessionAffinityKey::try_new("cyber-policy-session").expect("session");
    let first = ProviderAccountId::new("acct_first").expect("first account");
    let second = ProviderAccountId::new("acct_second").expect("second account");

    repository
        .record_failure(&provider, &session, &first, Duration::from_secs(60 * 60))
        .await
        .expect("record first failure");
    let state = repository
        .record_failure(&provider, &session, &second, Duration::from_secs(60 * 60))
        .await
        .expect("record second failure");

    assert_eq!(state.excluded_accounts().len(), 2);
    assert!(state.excluded_accounts().contains(&first));
    assert!(state.excluded_accounts().contains(&second));
    let keys = namespace_keys(&mut connection, &namespace).await;
    assert_eq!(keys.len(), 1);
    assert!(!keys[0].contains("cyber-policy-session"));
    let ttl = redis::cmd("PTTL")
        .arg(&keys[0])
        .query_async::<i64>(&mut connection)
        .await
        .expect("read exclusion TTL");
    assert!((3_590_000..=3_600_000).contains(&ttl));
}

#[tokio::test]
async fn session_exclusion_should_clear_only_the_state_observed_by_the_successful_request() {
    let Some((repository, _connection, _namespace)) = repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let session = ProviderSessionAffinityKey::try_new("cyber-policy-cas").expect("session");
    let first = ProviderAccountId::new("acct_first").expect("first account");
    let second = ProviderAccountId::new("acct_second").expect("second account");

    let observed = repository
        .record_failure(&provider, &session, &first, Duration::from_secs(60 * 60))
        .await
        .expect("record observed failure");
    let current = repository
        .record_failure(&provider, &session, &second, Duration::from_secs(60 * 60))
        .await
        .expect("record concurrent failure");

    assert!(
        !repository
            .clear(&provider, &session, observed.revision())
            .await
            .expect("compare-and-swap clear")
    );
    assert_eq!(
        repository
            .load(&provider, &session)
            .await
            .expect("load retained state"),
        Some(current.clone())
    );
    assert!(
        repository
            .clear(&provider, &session, current.revision())
            .await
            .expect("clear current state")
    );
    assert_eq!(
        repository
            .load(&provider, &session)
            .await
            .expect("load cleared state"),
        None
    );
}

async fn repository() -> Option<(
    RedisProviderSessionExclusionRepository,
    ConnectionManager,
    String,
)> {
    let redis_url = std::env::var("CPR_TEST_REDIS_URL").ok()?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-session-exclusion-test-{}", Uuid::new_v4());
    let repository = RedisProviderSessionExclusionRepository::new(connection.clone(), &namespace)
        .expect("valid test namespace");
    Some((repository, connection, namespace))
}

async fn namespace_keys(connection: &mut ConnectionManager, namespace: &str) -> Vec<String> {
    redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(connection)
        .await
        .expect("list isolated session exclusion keys")
}
