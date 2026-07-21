use std::{sync::Arc, time::Duration};

use gateway_core::{
    engine::credential::OpaqueProviderData,
    provider_ports::{
        NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingFlowPort, OAuthPendingPutOutcome,
        OAuthPendingTakeOutcome,
    },
    routing::ProviderKind,
};
use gateway_store::redis::RedisOAuthPendingFlowRepository;
use redis::aio::ConnectionManager;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

#[tokio::test]
async fn codex_pending_flow_wrong_owner_does_not_consume_it() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider kind");
    let flow =
        OAuthPendingBinding::try_new(format!("flow-{}", Uuid::new_v4())).expect("flow binding");
    let owner = OAuthPendingBinding::try_new("owner-a").expect("owner binding");
    let other_owner = OAuthPendingBinding::try_new("owner-b").expect("other owner binding");
    let mut payload = serde_json::Map::new();
    payload.insert(
        "state".to_owned(),
        serde_json::Value::String("opaque".to_owned()),
    );
    let payload = OpaqueProviderData::new(payload);

    assert_eq!(
        repository
            .put_if_absent(
                NewOAuthPendingFlow::try_new(
                    provider.clone(),
                    flow.clone(),
                    owner.clone(),
                    Duration::from_secs(60),
                    payload,
                )
                .expect("pending flow"),
            )
            .await
            .expect("store pending flow"),
        OAuthPendingPutOutcome::Stored
    );
    let fingerprint = scoped_fingerprint(&provider, &flow);
    let key = format!("{namespace}:oauth-pending:v1:openai:{fingerprint}");
    let mut fields: Vec<String> = redis::cmd("HKEYS")
        .arg(&key)
        .query_async(&mut connection)
        .await
        .expect("read pending fields");
    fields.sort();
    assert_eq!(
        fields,
        [
            "expires_at_epoch_seconds",
            "owner_fingerprint",
            "provider_payload",
        ]
    );
    let expires_at: i64 = redis::cmd("HGET")
        .arg(&key)
        .arg("expires_at_epoch_seconds")
        .query_async(&mut connection)
        .await
        .expect("read pending expiry");
    assert!(expires_at > chrono::Utc::now().timestamp());
    let ttl: i64 = redis::cmd("PTTL")
        .arg(&key)
        .query_async(&mut connection)
        .await
        .expect("read pending TTL");
    assert!(ttl > 0 && ttl <= 60_000);
    assert_eq!(
        repository
            .take_if_owner(&provider, &flow, &other_owner)
            .await
            .expect("reject wrong owner"),
        OAuthPendingTakeOutcome::OwnerMismatch
    );
    assert!(matches!(
        repository
            .take_if_owner(&provider, &flow, &owner)
            .await
            .expect("consume right owner"),
        OAuthPendingTakeOutcome::Taken(_)
    ));
    assert_eq!(
        repository
            .take_if_owner(&provider, &flow, &owner)
            .await
            .expect("flow is one shot"),
        OAuthPendingTakeOutcome::NotFound
    );
}

#[tokio::test]
async fn same_raw_flow_is_scoped_by_provider_kind() {
    let Some((repository, _connection, _namespace)) = repository().await else {
        return;
    };
    let flow =
        OAuthPendingBinding::try_new(format!("flow-{}", Uuid::new_v4())).expect("flow binding");
    let owner = OAuthPendingBinding::try_new("owner").expect("owner binding");
    for provider_name in ["openai", "xai"] {
        let provider = ProviderKind::new(provider_name).expect("provider kind");
        assert_eq!(
            repository
                .put_if_absent(
                    NewOAuthPendingFlow::try_new(
                        provider,
                        flow.clone(),
                        owner.clone(),
                        Duration::from_secs(60),
                        OpaqueProviderData::new(serde_json::Map::new()),
                    )
                    .expect("pending flow"),
                )
                .await
                .expect("store provider-scoped pending flow"),
            OAuthPendingPutOutcome::Stored
        );
    }
}

async fn repository() -> Option<(
    Arc<RedisOAuthPendingFlowRepository>,
    ConnectionManager,
    String,
)> {
    let redis_url = std::env::var("CPR_TEST_REDIS_URL").ok()?;
    let client = redis::Client::open(redis_url).ok()?;
    let connection: ConnectionManager = client.get_connection_manager().await.ok()?;
    let namespace = format!("gateway-store-test-{}", Uuid::new_v4());
    let repository = RedisOAuthPendingFlowRepository::new(connection.clone(), &namespace).ok()?;
    Some((Arc::new(repository), connection, namespace))
}

fn scoped_fingerprint(provider: &ProviderKind, flow: &OAuthPendingBinding) -> String {
    let mut digest = Sha256::new();
    digest.update(provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(flow.expose_to_store().as_bytes());
    hex::encode(digest.finalize())
}
