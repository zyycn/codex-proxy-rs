use gateway_admin::model::{
    MutationActor, MutationContext,
    pricing::{PricingChange, UpdatePricing},
};
use gateway_core::metering::ModelPriceOverride;
use gateway_store::postgres::{PgRuntimeSnapshotRepository, RuntimeSnapshotRepository};
use serde_json::json;

use super::TestDatabase;

#[tokio::test]
async fn concurrent_price_edits_preserve_other_models_and_sync_preserves_manual_prices() {
    let Some(redis_url) = crate::support::test_env("CPR_TEST_REDIS_URL") else {
        return;
    };
    let Some(database) = TestDatabase::create("pricing").await else {
        return;
    };
    let mut database_url = url::Url::parse(
        &crate::support::test_env("CPR_TEST_DATABASE_URL").expect("test database URL"),
    )
    .unwrap();
    database_url
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={}", database.schema));
    let connection = |mut url: url::Url| {
        let password = url.password().expect("test password").to_owned();
        url.set_password(None).unwrap();
        json!({"url":url.as_str(), "password":password})
    };
    let mut config: gateway_store::StoreConfig = serde_json::from_value(json!({
        "database":connection(database_url),
        "redis":connection(url::Url::parse(&redis_url).unwrap()),
    }))
    .unwrap();
    let runtime = tempfile::tempdir().unwrap();
    config.resolve_and_validate(runtime.path()).unwrap();
    let bundle = gateway_store::initialize(config).await.unwrap();
    let settings = bundle.admin_ports().settings();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "pricing-test".to_owned(),
    };
    let pricing: ModelPriceOverride = serde_json::from_value(json!({
        "multiplierBps":12500,
        "bands":{"standard":{"input":"2","output":"10","cacheRead":"0","cacheWrite":"1"}}
    }))
    .unwrap();
    let update = |model: &str, change| UpdatePricing {
        provider: "openai".to_owned(),
        models: vec![model.to_owned()],
        change,
    };
    let (first, second) = tokio::join!(
        settings.update_pricing(
            update("model-a", PricingChange::Replace(pricing.clone())),
            &context
        ),
        settings.update_pricing(
            update("model-b", PricingChange::Replace(pricing.clone())),
            &context
        ),
    );
    assert_ne!(first.unwrap(), second.unwrap());
    let stored = settings.load_pricing().await.unwrap();
    assert_eq!(stored.overrides["openai"].len(), 2);

    let mut source = stored.overrides.clone();
    source
        .get_mut("openai")
        .unwrap()
        .get_mut("model-a")
        .unwrap()
        .bands
        .get_mut("standard")
        .unwrap()
        .input = "9".to_owned().try_into().unwrap();
    settings
        .sync_pricing(source.clone(), &context)
        .await
        .unwrap();
    let stored = settings.load_pricing().await.unwrap();
    assert_eq!(stored.overrides["openai"]["model-a"], pricing);
    assert!(stored.synced_at.is_some());
    for _ in 0..2 {
        settings
            .update_pricing(
                update("model-a", PricingChange::Multiplier(20000)),
                &context,
            )
            .await
            .unwrap();
    }
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone());
    let frozen = snapshot.load_runtime_snapshot().await.unwrap();
    assert_eq!(
        frozen.settings.pricing["openai"]["model-a"].multiplier_bps,
        20000
    );
    assert_eq!(
        frozen.settings.pricing["openai"]["model-a"].bands,
        pricing.bands
    );

    settings
        .update_pricing(update("model-a", PricingChange::Reset), &context)
        .await
        .unwrap();
    let restored = snapshot.load_runtime_snapshot().await.unwrap();
    assert_eq!(
        restored.settings.pricing["openai"]["model-a"],
        source["openai"]["model-a"]
    );
    assert_eq!(restored.settings.pricing["openai"]["model-b"], pricing);
    assert_eq!(
        frozen.settings.pricing["openai"]["model-a"].multiplier_bps,
        20000
    );
    let audits: i64 = sqlx::query_scalar("select count(*) from admin_audit_events where action in ('pricing.update', 'pricing.sync')")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(audits, 6);
    drop(settings);
    drop(bundle);
    database.close().await;
}
