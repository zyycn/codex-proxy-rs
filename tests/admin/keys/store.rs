use codex_proxy_rs::{
    admin::keys::service::{ClientKeyStore, RuntimeClientKeyStore, SqliteClientKeyStore},
    infra::database::connect_sqlite,
};

#[tokio::test]
async fn client_key_store_should_create_list_disable_enable_and_delete_keys() {
    let (store, _dir) = client_key_store("client-keys.sqlite").await;

    let created = store.create("cursor").await.unwrap();
    assert_eq!(created.name, "cursor");
    assert!(created.key.starts_with("sk_"));

    let key: (String,) = sqlx::query_as("select key from client_api_keys where id = ?")
        .bind(&created.id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(key.0, created.key);

    let first_page = store.list(None, 10).await.unwrap();
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.items[0].name, "cursor");
    assert_eq!(first_page.items[0].key, created.key);
    assert!(first_page.items[0].last_used_at.is_none());

    assert!(store.set_enabled(&created.id, false).await.unwrap());
    assert!(store.set_enabled(&created.id, true).await.unwrap());
    assert!(store.delete(&created.id).await.unwrap());
}

#[tokio::test]
async fn runtime_client_key_store_should_verify_from_memory_and_defer_last_used_flush() {
    let (store, _dir) = client_key_store("client-keys-runtime.sqlite").await;
    let created = store.create("runtime").await.unwrap();
    let runtime = RuntimeClientKeyStore::new(store.clone());
    runtime.reload_from_store().await.unwrap();

    assert!(runtime.verify_and_touch(&created.key).await.unwrap());

    let before_flush = store.get(&created.id).await.unwrap().unwrap();
    assert!(before_flush.last_used_at.is_none());

    runtime.flush_pending_last_used().await;

    let after_flush = store.get(&created.id).await.unwrap().unwrap();
    assert!(after_flush.last_used_at.is_some());
}

#[tokio::test]
async fn runtime_client_key_store_should_not_accept_disabled_keys_after_reload() {
    let (store, _dir) = client_key_store("client-keys-runtime-disabled.sqlite").await;
    let created = store.create("runtime-disabled").await.unwrap();
    assert!(store.set_enabled(&created.id, false).await.unwrap());
    let runtime = RuntimeClientKeyStore::new(store);
    runtime.reload_from_store().await.unwrap();

    assert!(!runtime.verify_and_touch(&created.key).await.unwrap());
}

#[tokio::test]
async fn client_key_store_should_page_keys_by_created_at_desc() {
    let (store, _dir) = client_key_store("client-keys-page.sqlite").await;

    let key_a = store.create("a").await.unwrap();
    let key_b = store.create("b").await.unwrap();
    let key_c = store.create("c").await.unwrap();
    sqlx::query("update client_api_keys set created_at = ? where id = ?")
        .bind("2026-06-11T00:00:00Z")
        .bind(&key_a.id)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("update client_api_keys set created_at = ? where id = ?")
        .bind("2026-06-11T00:01:00Z")
        .bind(&key_b.id)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("update client_api_keys set created_at = ? where id = ?")
        .bind("2026-06-11T00:02:00Z")
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

async fn client_key_store(db_name: &str) -> (SqliteClientKeyStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    (SqliteClientKeyStore::new(pool), dir)
}
