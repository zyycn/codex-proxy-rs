//! 验证运行时快照读取当前 Key 策略并保护密钥诊断输出

use gateway_core::policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits};
use gateway_store::postgres::{
    ClientApiKeySnapshot, PgRuntimeSnapshotRepository, RuntimeSnapshotRepository,
};

use super::TestDatabase;

#[test]
fn client_key_snapshot_debug_redacts_plaintext_key() {
    let policy = ClientApiKeySnapshot {
        name_snapshot: None,
        request_profiles: Default::default(),
        id: ClientApiKeyId::new("key-1").expect("client key ID"),
        plaintext_key: PlaintextClientApiKey::new("sk_snapshot_secret").expect("plaintext key"),
        group_ids: Vec::new(),
        limits: RateLimits {
            max_concurrency: 3,
            requests_per_minute: 60,
        },
    };
    assert!(!format!("{policy:?}").contains("sk_snapshot_secret"));
}

#[tokio::test]
async fn runtime_snapshot_loads_enabled_plaintext_key_without_debug_exposure() {
    let Some(database) = TestDatabase::create("client_snapshot").await else {
        return;
    };
    let plaintext = format!("sk_{}", "s".repeat(43));
    sqlx::query(
        "insert into client_api_keys (
           id, name, key, enabled, max_concurrency, requests_per_minute, created_at, updated_at
         ) values ('key_snapshot', 'snapshot', $1, true, 2, 60, now(), now())",
    )
    .bind(&plaintext)
    .execute(&database.pool)
    .await
    .expect("seed client API key");
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .expect("load runtime snapshot");
    assert_eq!(snapshot.client_api_keys.len(), 1);
    assert_eq!(
        snapshot.client_api_keys[0].name_snapshot.as_deref(),
        Some("snapshot")
    );
    assert!(snapshot.client_api_keys[0].group_ids.is_empty());
    assert_eq!(snapshot.client_api_keys[0].limits.max_concurrency, 2);
    assert_eq!(snapshot.client_api_keys[0].limits.requests_per_minute, 60);
    assert_eq!(
        snapshot.client_api_keys[0].plaintext_key.expose_for_auth(),
        plaintext
    );
    assert!(!format!("{snapshot:?}").contains(&plaintext));
    database.close().await;
}
