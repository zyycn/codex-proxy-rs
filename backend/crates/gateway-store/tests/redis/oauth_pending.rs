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
use uuid::Uuid;

#[tokio::test]
async fn codex_pending_flow_wrong_owner_does_not_consume_it() {
    let Some(repository) = repository().await else {
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

async fn repository() -> Option<Arc<RedisOAuthPendingFlowRepository>> {
    let redis_url = std::env::var("CPR_TEST_REDIS_URL").ok()?;
    let client = redis::Client::open(redis_url).ok()?;
    let connection: ConnectionManager = client.get_connection_manager().await.ok()?;
    RedisOAuthPendingFlowRepository::new(
        connection,
        &format!("gateway-store-test-{}", Uuid::new_v4()),
    )
    .ok()
    .map(Arc::new)
}
