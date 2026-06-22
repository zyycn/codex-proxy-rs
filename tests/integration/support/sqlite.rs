use codex_proxy_rs::infra::database::connect_sqlite;
use sqlx::SqlitePool;

pub async fn init_test_db(db_name: &str) -> (SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    (pool, dir)
}
