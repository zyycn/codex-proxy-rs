use codex_proxy_rs::keys::{manage::KeyManageService, store::PgClientKeyStore};
use sqlx::PgPool;

pub(crate) async fn insert_client_api_key(pool: &PgPool) -> String {
    KeyManageService::new(PgClientKeyStore::new(pool.clone()))
        .create("test")
        .await
        .unwrap()
        .key
}
