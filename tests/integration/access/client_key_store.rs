use codex_proxy_rs::{
    access::client_keys::{ClientKeyStore, SqliteClientKeyStore},
    infra::{database::connect_sqlite, identity::ApiKeyHasher},
};

#[tokio::test]
async fn client_key_store_should_create_list_disable_enable_and_delete_keys() {
    let (store, _dir) = client_key_store("client-keys.sqlite", 10).await;

    let created = store.create("cursor").await.unwrap();
    assert_eq!(created.name, "cursor");
    assert!(created.plaintext.starts_with("cpr_"));
    assert!(store.verify_and_touch(&created.plaintext).await.unwrap());

    let first_page = store.list(None, 10).await.unwrap();
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.items[0].name, "cursor");
    assert!(first_page.items[0].last_used_at.is_some());

    assert!(store.set_enabled(&created.id, false).await.unwrap());
    assert!(!store.verify_and_touch(&created.plaintext).await.unwrap());

    assert!(store.set_enabled(&created.id, true).await.unwrap());
    assert!(store.verify_and_touch(&created.plaintext).await.unwrap());

    assert!(store.delete(&created.id).await.unwrap());
    assert!(!store.verify_and_touch(&created.plaintext).await.unwrap());
}

#[tokio::test]
async fn client_key_store_should_page_keys_by_created_at_desc() {
    let (store, _dir) = client_key_store("client-keys-page.sqlite", 11).await;

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

async fn client_key_store(
    db_name: &str,
    key_byte: u8,
) -> (SqliteClientKeyStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    (
        SqliteClientKeyStore::new(pool, ApiKeyHasher::new([key_byte; 32])),
        dir,
    )
}
