use codex_proxy_rs::keys::{
    manage::KeyManageService, service::KeyVerifier, store::PgClientKeyStore,
};

use crate::support::storage::init_test_db;

#[tokio::test]
async fn client_key_store_should_create_list_disable_enable_and_delete_keys() {
    let (store, _dir) = client_key_store("client-keys").await;

    let created = KeyManageService::new(store.clone())
        .create("cursor")
        .await
        .unwrap();
    assert_eq!(created.name, "cursor");
    assert!(created.key.starts_with("sk_"));

    let stored_key: (String,) = sqlx::query_as("select key from client_api_keys where id = $1")
        .bind(&created.id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(stored_key.0, created.key);

    let first_page = store.list(None, 10).await.unwrap();
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.items[0].name, "cursor");
    assert_eq!(first_page.items[0].prefix, created.prefix);
    assert_eq!(first_page.items[0].key, created.key);
    assert!(first_page.items[0].last_used_at.is_none());

    assert!(store.set_enabled(&created.id, false).await.unwrap());
    assert!(store.set_enabled(&created.id, true).await.unwrap());
    assert!(store.delete(&created.id).await.unwrap());
}

#[tokio::test]
async fn client_key_service_should_verify_by_unique_key_and_defer_last_used_flush() {
    let (store, _dir) = client_key_store("client-keys-runtime").await;
    let created = KeyManageService::new(store.clone())
        .create("runtime")
        .await
        .unwrap();
    let runtime = KeyVerifier::new(store.clone());

    assert_eq!(
        runtime.verify(&created.key).await.unwrap().as_deref(),
        Some(created.id.as_str())
    );

    let before_flush = store.get(&created.id).await.unwrap().unwrap();
    assert!(before_flush.last_used_at.is_none());

    runtime.flush_pending_last_used().await;

    let after_flush = store.get(&created.id).await.unwrap().unwrap();
    assert!(after_flush.last_used_at.is_some());
}

#[tokio::test]
async fn client_key_service_should_not_accept_disabled_keys() {
    let (store, _dir) = client_key_store("client-keys-runtime-disabled").await;
    let created = KeyManageService::new(store.clone())
        .create("runtime-disabled")
        .await
        .unwrap();
    assert!(store.set_enabled(&created.id, false).await.unwrap());
    let runtime = KeyVerifier::new(store);

    assert!(runtime.verify(&created.key).await.unwrap().is_none());
}

#[tokio::test]
async fn client_key_store_should_page_keys_by_created_at_desc() {
    let (store, _dir) = client_key_store("client-keys-page").await;

    let admin = KeyManageService::new(store.clone());
    let key_a = admin.create("a").await.unwrap();
    let key_b = admin.create("b").await.unwrap();
    let key_c = admin.create("c").await.unwrap();
    sqlx::query("update client_api_keys set created_at = $1 where id = $2")
        .bind(crate::support::storage::timestamp("2026-06-11T00:00:00Z"))
        .bind(&key_a.id)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("update client_api_keys set created_at = $1 where id = $2")
        .bind(crate::support::storage::timestamp("2026-06-11T00:01:00Z"))
        .bind(&key_b.id)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("update client_api_keys set created_at = $1 where id = $2")
        .bind(crate::support::storage::timestamp("2026-06-11T00:02:00Z"))
        .bind(&key_c.id)
        .execute(store.pool())
        .await
        .unwrap();

    let first_page = store.list(None, 2).await.unwrap();
    assert_eq!(
        first_page
            .items
            .iter()
            .map(|key| key.name.as_str())
            .collect::<Vec<_>>(),
        ["c", "b"]
    );

    let second_page = store.list(first_page.next_cursor, 2).await.unwrap();
    assert_eq!(
        second_page
            .items
            .iter()
            .map(|key| key.name.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
}

async fn client_key_store(db_name: &str) -> (PgClientKeyStore, tempfile::TempDir) {
    let (pool, dir) = init_test_db(db_name).await;
    (PgClientKeyStore::new(pool), dir)
}
