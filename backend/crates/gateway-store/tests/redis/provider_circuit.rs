use std::{num::NonZeroU32, time::Duration};

use gateway_store::redis::{
    ProviderCircuitDecision, ProviderCircuitPolicy, ProviderCircuitRepository,
    RedisProviderCircuitRepository,
};
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[test]
fn provider_circuit_default_has_positive_threshold() {
    assert!(ProviderCircuitPolicy::default().failure_threshold.get() > 0);
}

#[tokio::test]
async fn provider_circuit_opens_at_threshold_and_success_resets_it() {
    let Some((repository, mut connection, namespace)) = repository(2).await else {
        return;
    };
    let instance = "instance-circuit-primary";

    assert_eq!(
        repository
            .provider_circuit_decision(instance)
            .await
            .expect("read empty provider circuit"),
        ProviderCircuitDecision::Allow
    );
    let first = repository
        .observe_provider_failure(instance)
        .await
        .expect("record first provider failure");
    assert_eq!(first.failure_count, 1);
    assert!(first.open_until.is_none());

    let second = repository
        .observe_provider_failure(instance)
        .await
        .expect("record threshold provider failure");
    assert_eq!(second.failure_count, 2);
    assert!(second.open_until.is_some());
    assert!(matches!(
        repository
            .provider_circuit_decision(instance)
            .await
            .expect("read open provider circuit"),
        ProviderCircuitDecision::BlockedUntil(_)
    ));

    repository
        .observe_provider_success(instance)
        .await
        .expect("reset provider circuit after success");
    assert_eq!(
        repository
            .provider_circuit_decision(instance)
            .await
            .expect("read reset provider circuit"),
        ProviderCircuitDecision::Allow
    );

    delete_namespace_keys(&mut connection, &namespace).await;
}

#[tokio::test]
async fn provider_circuit_keeps_instances_isolated() {
    let Some((repository, mut connection, namespace)) = repository(1).await else {
        return;
    };

    repository
        .observe_provider_failure("instance-circuit-failed")
        .await
        .expect("open failed instance circuit");
    assert!(matches!(
        repository
            .provider_circuit_decision("instance-circuit-failed")
            .await
            .expect("read failed instance circuit"),
        ProviderCircuitDecision::BlockedUntil(_)
    ));
    assert_eq!(
        repository
            .provider_circuit_decision("instance-circuit-healthy")
            .await
            .expect("read isolated healthy circuit"),
        ProviderCircuitDecision::Allow
    );

    delete_namespace_keys(&mut connection, &namespace).await;
}

#[tokio::test]
async fn provider_circuit_reopens_after_redis_time_deadline() {
    let Some((repository, mut connection, namespace)) =
        repository_with_policy(1, Duration::from_millis(40)).await
    else {
        return;
    };
    let instance = "instance-circuit-deadline";

    repository
        .observe_provider_failure(instance)
        .await
        .expect("open short provider circuit");
    assert!(matches!(
        repository
            .provider_circuit_decision(instance)
            .await
            .expect("read short open circuit"),
        ProviderCircuitDecision::BlockedUntil(_)
    ));
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        repository
            .provider_circuit_decision(instance)
            .await
            .expect("read circuit after Redis deadline"),
        ProviderCircuitDecision::Allow
    );

    delete_namespace_keys(&mut connection, &namespace).await;
}

async fn repository(
    threshold: u32,
) -> Option<(RedisProviderCircuitRepository, ConnectionManager, String)> {
    repository_with_policy(threshold, Duration::from_secs(30)).await
}

async fn repository_with_policy(
    threshold: u32,
    open_duration: Duration,
) -> Option<(RedisProviderCircuitRepository, ConnectionManager, String)> {
    let redis_url = std::env::var("CPR_TEST_REDIS_URL").ok()?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-circuit-test-{}", Uuid::new_v4());
    let policy = ProviderCircuitPolicy {
        failure_threshold: NonZeroU32::new(threshold).expect("positive threshold"),
        open_duration,
    };
    let repository = RedisProviderCircuitRepository::new(connection.clone(), &namespace, policy)
        .expect("valid provider circuit repository");
    Some((repository, connection, namespace))
}

async fn delete_namespace_keys(connection: &mut ConnectionManager, namespace: &str) {
    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(connection)
        .await
        .expect("list isolated provider circuit keys");
    if !keys.is_empty() {
        redis::cmd("DEL")
            .arg(keys)
            .query_async::<i64>(connection)
            .await
            .expect("delete isolated provider circuit keys");
    }
}
