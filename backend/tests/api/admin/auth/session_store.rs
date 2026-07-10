use chrono::Duration;
use codex_proxy_rs::{
    auth::store::{PgAdminUserStore, RedisAdminSessionStore},
    infra::identity::hash_admin_password,
};

use crate::support::storage::{create_test_redis, init_test_db};

#[tokio::test]
async fn postgres_admin_user_store_creates_default_admin_once() {
    let (pool, _guard) = init_test_db("admin-user-store").await;
    let store = PgAdminUserStore::new(pool);
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
async fn redis_admin_session_store_hashes_tokens_and_uses_ttl() {
    let redis = create_test_redis("admin-session-store").await;
    let store = RedisAdminSessionStore::new(redis.clone());
    store
        .create_session("session-secret", "admin_1", Duration::minutes(5))
        .await
        .unwrap();

    assert!(store.validate_session("session-secret").await.unwrap());
    assert!(!store.validate_session("wrong-secret").await.unwrap());

    let mut connection = redis.manager();
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(redis.key("admin:session:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(keys.len(), 1);
    assert!(!keys[0].contains("session-secret"));
    let ttl: i64 = redis::cmd("TTL")
        .arg(&keys[0])
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!((1..=300).contains(&ttl));

    assert!(store.delete_session("session-secret").await.unwrap());
    assert!(!store.validate_session("session-secret").await.unwrap());
}
