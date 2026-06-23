use codex_proxy_rs::infra::database::connect_sqlite;
use codex_proxy_rs::upstream::models::ModelSnapshotStore as _;
use codex_proxy_rs::upstream::models::SqliteModelSnapshotStore;
use codex_proxy_rs::upstream::models::{BackendModelEntry, ModelPlanSnapshot};

#[tokio::test]
async fn runtime_state_should_expose_backend_model_snapshot_through_model_service() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("runtime-models.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let snapshot_store = SqliteModelSnapshotStore::new(pool.clone());
    snapshot_store
        .replace_plan_snapshot(&ModelPlanSnapshot::from_backend_entries(
            "plus",
            vec![BackendModelEntry {
                id: Some("gpt-6".to_string()),
                name: Some("GPT-6".to_string()),
                ..BackendModelEntry::default()
            }],
        ))
        .await
        .expect("replace snapshot");

    let loaded = snapshot_store
        .list_plan_snapshots()
        .await
        .expect("list snapshots");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].models[0].id, "gpt-6");
}
