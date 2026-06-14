use chrono::{Duration, Utc};

use codex_proxy_rs::{
    admin::auth::repository::AdminAuthRepository,
    platform::{identity::admin_session::hash_admin_password, storage::db::connect_sqlite},
};

#[tokio::test]
async fn admin_auth_repository_should_create_and_load_default_admin_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AdminAuthRepository::new(pool);
    let password_hash = hash_admin_password("admin-password").unwrap();

    let created = repo.ensure_default_admin(&password_hash).await.unwrap();
    let skipped = repo.ensure_default_admin("different-hash").await.unwrap();
    let admin = repo.load_first_admin().await.unwrap().unwrap();

    assert!(created);
    assert!(!skipped);
    assert_eq!(admin.password_hash, password_hash);
}

#[tokio::test]
async fn admin_auth_repository_should_create_validate_and_cleanup_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-sessions.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AdminAuthRepository::new(pool);
    let password_hash = hash_admin_password("admin-password").unwrap();
    repo.ensure_default_admin(&password_hash).await.unwrap();
    let admin = repo.load_first_admin().await.unwrap().unwrap();
    let future_session = "sess_future";
    let expired_session = "sess_expired";

    repo.create_session(future_session, &admin.id, Utc::now() + Duration::minutes(5))
        .await
        .unwrap();
    repo.create_session(
        expired_session,
        &admin.id,
        Utc::now() - Duration::minutes(5),
    )
    .await
    .unwrap();

    assert!(repo.validate_session(future_session).await.unwrap());
    assert!(!repo.validate_session(expired_session).await.unwrap());

    let deleted = repo.cleanup_expired_sessions(Utc::now()).await.unwrap();

    assert_eq!(deleted, 1);
    assert!(repo.validate_session(future_session).await.unwrap());
}
