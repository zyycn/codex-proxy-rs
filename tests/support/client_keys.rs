use codex_proxy_rs::admin::keys::service::SqliteClientKeyStore;
use sqlx::SqlitePool;

pub(crate) async fn insert_client_api_key(pool: &SqlitePool) -> String {
    SqliteClientKeyStore::new(pool.clone())
        .create("test")
        .await
        .unwrap()
        .key
}
