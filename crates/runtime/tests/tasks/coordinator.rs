use super::*;

#[tokio::test]
async fn start_background_tasks_should_register_migrated_runtime_tasks() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("background-tasks.sqlite");
    let database_url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&database_url).await.expect("sqlite pool");
    let state = codex_proxy_runtime::state::AppState::with_pool_and_secret_box(
        test_config(database_url),
        pool,
        SecretBox::new([9u8; 32]),
    );

    let coordinator = codex_proxy_runtime::tasks::coordinator::start_background_tasks(&state).await;
    let task_names = coordinator.task_names();

    assert_eq!(
        task_names,
        [
            "cookie_cleanup",
            "session_cleanup",
            "session_affinity_cleanup",
            "model_refresh",
            "token_refresh",
            "quota_refresh",
            "fingerprint_update"
        ]
    );

    coordinator.shutdown().await;
}
