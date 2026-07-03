use codex_proxy_rs::upstream::models::{
    backend_entry::BackendModelEntry,
    snapshot::ModelPlanSnapshot,
    snapshot_store::{ModelSnapshotStore, SqliteModelSnapshotStore},
};

#[tokio::test]
async fn model_snapshot_repository_should_replace_and_load_plan_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("models.sqlite");
    let pool =
        codex_proxy_rs::infra::database::connect_sqlite(&format!("sqlite://{}", db.display()))
            .await
            .unwrap();
    let repo = SqliteModelSnapshotStore::new(pool);
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
