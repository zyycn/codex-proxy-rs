use codex_proxy_rs::models::{
    store::{ModelSnapshotStore, RedisModelSnapshotStore},
    types::{BackendModelEntry, ModelPlanSnapshot},
};

use crate::support::storage::create_test_redis;

#[tokio::test]
async fn model_snapshot_store_should_replace_and_load_plan_snapshots() {
    let redis = create_test_redis("model-snapshots").await;
    let repo = RedisModelSnapshotStore::new(redis);
    let snapshot = ModelPlanSnapshot::from_backend_entries(
        "team",
        vec![BackendModelEntry {
            id: Some("gpt-team".to_string()),
            name: Some("GPT Team".to_string()),
            ..BackendModelEntry::default()
        }],
    );

    ModelSnapshotStore::replace_plan_snapshot(&repo, &snapshot)
        .await
        .unwrap();
    let loaded = ModelSnapshotStore::list_plan_snapshots(&repo)
        .await
        .unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].plan_type, "team");
    assert_eq!(loaded[0].models[0].id, "gpt-team");
    assert_eq!(loaded[0].models[0].source, "backend");
}
