use codex_proxy_rs::storage::db::connect_sqlite;

#[tokio::test]
async fn sqlite_schema_creates_accounts_and_event_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let row: (i64,) = sqlx::query_as(
        "select count(*) from sqlite_master where type = 'table' and name in ('accounts', 'client_api_keys', 'event_logs', 'model_plan_snapshots')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, 4);
}

#[tokio::test]
async fn connect_sqlite_creates_parent_directory_for_file_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nested").join("startup.sqlite");
    let url = format!("sqlite://{}", db.display());

    let pool = connect_sqlite(&url).await.unwrap();
    pool.close().await;

    assert!(db.exists());
}
