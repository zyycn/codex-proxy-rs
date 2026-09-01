use std::time::{Duration, SystemTime};

use gateway_core::account::OpaqueProviderData;
use gateway_core::provider_ports::{
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use gateway_store::redis::RedisProviderArtifactProfileRepository;
use redis::aio::ConnectionManager;
use serde_json::{Map, Value};
use uuid::Uuid;

#[test]
fn artifact_profile_adapter_implements_provider_port() {
    fn assert_port<T: ProviderArtifactProfileCachePort>() {}
    assert_port::<RedisProviderArtifactProfileRepository>();
}

#[tokio::test]
async fn artifact_profile_uses_one_expiring_key_and_rejects_rollback_or_conflict() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider kind");
    let current = profile(provider.clone(), 6_570, "0.148.0-alpha.9");
    assert!(
        repository
            .replace_if_newer(current.clone(), Duration::from_secs(60))
            .await
            .expect("store current profile")
    );
    assert_eq!(
        repository.read(&provider).await.expect("read profile"),
        Some(current.clone())
    );

    assert!(
        !repository
            .replace_if_newer(
                profile(provider.clone(), 6_415, "0.147.0-alpha.6.6"),
                Duration::from_secs(60),
            )
            .await
            .expect("reject older profile")
    );
    let conflict = repository
        .replace_if_newer(
            profile(provider.clone(), 6_570, "0.148.0-alpha.10"),
            Duration::from_secs(60),
        )
        .await
        .expect_err("same build with different Core must conflict");
    assert_eq!(conflict.kind(), ProviderStoreErrorKind::Conflict);
    assert_eq!(
        repository
            .read(&provider)
            .await
            .expect("read retained profile"),
        Some(current)
    );

    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list isolated keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        format!("{namespace}:provider:openai:artifact-profile:v1")
    );
    let ttl = redis::cmd("PTTL")
        .arg(&keys[0])
        .query_async::<i64>(&mut connection)
        .await
        .expect("read profile TTL");
    assert!((1..=60_000).contains(&ttl));
}

fn profile(
    provider: ProviderKind,
    artifact_sequence: u64,
    codex_version: &str,
) -> ProviderArtifactProfile {
    let mut fields = Map::new();
    fields.insert("schema_version".to_owned(), Value::from(1));
    fields.insert(
        "codex_version".to_owned(),
        Value::String(codex_version.to_owned()),
    );
    ProviderArtifactProfile::new(
        provider,
        artifact_sequence,
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        OpaqueProviderData::new(fields),
    )
}

async fn repository() -> Option<(
    RedisProviderArtifactProfileRepository,
    ConnectionManager,
    String,
)> {
    let redis_url = crate::support::test_env("CPR_TEST_REDIS_URL")?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-test-{}", Uuid::new_v4());
    let repository = RedisProviderArtifactProfileRepository::new(connection.clone(), &namespace)
        .expect("valid test namespace");
    Some((repository, connection, namespace))
}
