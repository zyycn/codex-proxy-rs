use gateway_core::engine::continuation::{
    NativeContinuationPin, NativeContinuationPort, NativeContinuationScope, PreviousResponseId,
};
use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::operation::ProviderSessionState;
use gateway_core::routing::ProviderKind;
use gateway_store::redis::RedisNativeContinuationRepository;
use redis::aio::ConnectionManager;
use serde_json::{Map, Value};
use uuid::Uuid;

#[tokio::test]
async fn native_continuation_round_trips_opaque_state_without_exposing_response_id_in_keys() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let previous_response_id =
        PreviousResponseId::new(format!("resp_{}\0client-visible-secret", "x".repeat(257)));
    let state = ProviderSessionState::new(
        "openai",
        Map::from_iter([(
            "turn_state".to_owned(),
            Value::String("opaque-turn".to_owned()),
        )]),
    )
    .expect("provider session state");

    repository
        .record(
            NativeContinuationPin::new(
                previous_response_id.clone(),
                previous_response_id.clone(),
                ProviderKind::new("openai").expect("provider"),
                ProviderAccountId::new("acct_primary").expect("account"),
            )
            .with_scope(NativeContinuationScope::Persisted)
            .with_session_state(state.clone()),
        )
        .await
        .expect("record continuation");

    let resolved = repository
        .resolve(&previous_response_id)
        .await
        .expect("resolve continuation")
        .expect("stored continuation");
    assert_eq!(
        resolved.upstream_response_id().as_str(),
        previous_response_id.as_str()
    );
    assert_eq!(resolved.account().as_str(), "acct_primary");
    assert_eq!(resolved.session_state(), Some(&state));

    let keys = namespace_keys(&mut connection, &namespace).await;
    assert!(
        keys.iter()
            .all(|key| !key.contains("client-visible-secret"))
    );
    let entry_key = keys
        .iter()
        .find(|key| key.contains(":entry:{"))
        .expect("continuation entry key");
    let ttl = redis::cmd("PTTL")
        .arg(entry_key)
        .query_async::<i64>(&mut connection)
        .await
        .expect("read continuation TTL");
    assert!((14_000_000..=14_400_000).contains(&ttl));

    delete_namespace_keys(&mut connection, &namespace).await;
}

#[tokio::test]
async fn empty_opaque_continuation_is_a_store_miss_without_a_redis_record() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let response_id = PreviousResponseId::new("");

    assert!(
        repository
            .resolve(&response_id)
            .await
            .expect("resolve")
            .is_none()
    );
    repository
        .record(NativeContinuationPin::new(
            response_id.clone(),
            response_id,
            ProviderKind::new("openai").expect("provider"),
            ProviderAccountId::new("acct_primary").expect("account"),
        ))
        .await
        .expect("empty opaque handle is not a store error");
    assert!(namespace_keys(&mut connection, &namespace).await.is_empty());
}

#[tokio::test]
async fn native_continuation_rejects_provider_state_for_a_different_provider() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp-mismatched-state"),
        PreviousResponseId::new("resp-upstream-handle"),
        ProviderKind::new("openai").expect("provider"),
        ProviderAccountId::new("acct_primary").expect("account"),
    )
    .with_session_state(
        ProviderSessionState::new("xai", Map::new()).expect("provider session state"),
    );

    assert!(repository.record(pin).await.is_err());
    assert!(namespace_keys(&mut connection, &namespace).await.is_empty());
}

#[tokio::test]
async fn native_continuation_record_should_bound_index_cleanup_work() {
    const STALE_ENTRY_COUNT: usize = 100;
    const CLEANUP_BATCH: usize = 64;

    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    repository
        .record(pin("resp-cleanup-anchor"))
        .await
        .expect("record cleanup anchor");
    let index_key = namespace_keys(&mut connection, &namespace)
        .await
        .into_iter()
        .find(|key| key.ends_with(":global:index"))
        .expect("continuation index key");
    let mut seed = redis::cmd("ZADD");
    seed.arg(&index_key);
    for index in 0..STALE_ENTRY_COUNT {
        seed.arg(0).arg(format!("stale-{index}"));
    }
    assert_eq!(
        seed.query_async::<i64>(&mut connection)
            .await
            .expect("seed stale continuation index entries"),
        i64::try_from(STALE_ENTRY_COUNT).expect("stale entry count fits i64")
    );

    repository
        .record(pin("resp-cleanup-trigger"))
        .await
        .expect("record continuation with bounded cleanup");

    assert_eq!(
        redis::cmd("ZCARD")
            .arg(index_key)
            .query_async::<usize>(&mut connection)
            .await
            .expect("count continuation index entries"),
        STALE_ENTRY_COUNT + 2 - CLEANUP_BATCH
    );
    delete_namespace_keys(&mut connection, &namespace).await;
}

async fn repository() -> Option<(RedisNativeContinuationRepository, ConnectionManager, String)> {
    let redis_url = crate::support::test_env("CPR_TEST_REDIS_URL")?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-native-continuation-test-{}", Uuid::new_v4());
    let repository = RedisNativeContinuationRepository::new(connection.clone(), &namespace)
        .expect("valid test namespace");
    Some((repository, connection, namespace))
}

fn pin(response_id: &str) -> NativeContinuationPin {
    NativeContinuationPin::new(
        PreviousResponseId::new(response_id),
        PreviousResponseId::new(response_id),
        ProviderKind::new("openai").expect("provider"),
        ProviderAccountId::new("acct_primary").expect("account"),
    )
}

async fn namespace_keys(connection: &mut ConnectionManager, namespace: &str) -> Vec<String> {
    redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(connection)
        .await
        .expect("list isolated native continuation keys")
}

async fn delete_namespace_keys(connection: &mut ConnectionManager, namespace: &str) {
    let keys = namespace_keys(connection, namespace).await;
    if !keys.is_empty() {
        redis::cmd("DEL")
            .arg(keys)
            .query_async::<i64>(connection)
            .await
            .expect("delete isolated native continuation keys");
    }
}
