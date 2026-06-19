use chrono::{Duration, Utc};
use codex_proxy_adapters::sqlite::admin_sessions::SqliteAdminSessionStore;
use codex_proxy_platform::{identity::hash_admin_password, storage::connect_sqlite};

#[tokio::test]
async fn admin_session_store_should_create_and_load_default_admin_once() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("admin-auth.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAdminSessionStore::new(pool);
    let password_hash = hash_admin_password("admin-password").expect("password hash");

    let created = store
        .ensure_default_admin(&password_hash)
        .await
        .expect("default admin should be created");
    let skipped = store
        .ensure_default_admin("different-hash")
        .await
        .expect("second default admin should be skipped");
    let admin = store
        .load_first_admin()
        .await
        .expect("default admin should load")
        .expect("default admin should exist");

    assert!(created);
    assert!(!skipped);
    assert_eq!(admin.password_hash, password_hash);
}

#[tokio::test]
async fn admin_session_store_should_create_validate_and_cleanup_sessions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("admin-sessions.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAdminSessionStore::new(pool);
    let password_hash = hash_admin_password("admin-password").expect("password hash");
    store
        .ensure_default_admin(&password_hash)
        .await
        .expect("default admin should be created");
    let admin = store
        .load_first_admin()
        .await
        .expect("admin should load")
        .expect("admin should exist");
    let now = Utc::now();

    store
        .create_session("sess_future", &admin.id, now + Duration::minutes(5))
        .await
        .expect("future session should be inserted");
    store
        .create_session("sess_expired", &admin.id, now - Duration::minutes(5))
        .await
        .expect("expired session should be inserted");

    assert!(store
        .validate_session("sess_future")
        .await
        .expect("future session should validate"));
    assert!(!store
        .validate_session("sess_expired")
        .await
        .expect("expired session should not validate"));

    let deleted = store
        .cleanup_expired_sessions(now)
        .await
        .expect("expired session should be deleted");

    assert_eq!(deleted, 1);
    assert!(store
        .validate_session("sess_future")
        .await
        .expect("future session should remain"));
}
