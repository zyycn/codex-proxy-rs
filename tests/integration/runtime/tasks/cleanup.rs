use super::*;

#[tokio::test]
async fn cookie_cleanup_task_should_delete_only_expired_cookie_rows() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("cookies.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let secret_box = SecretBox::new([7u8; 32]);
    let store = SqliteCookieStore::new(pool.clone(), secret_box);
    insert_account(&pool, "acct-cookie").await;
    let now = Utc::now();
    insert_cookie(
        &pool,
        "expired-cookie",
        "acct-cookie",
        "old",
        now - Duration::minutes(1),
    )
    .await;
    insert_cookie(
        &pool,
        "active-cookie",
        "acct-cookie",
        "fresh",
        now + Duration::minutes(10),
    )
    .await;

    let deleted = codex_proxy_rs::runtime::tasks::cookie_cleanup::CookieCleanupTask::new(store)
        .cleanup_once_at(now)
        .await
        .expect("cookie cleanup should succeed");

    let remaining = cookie_ids(&pool).await;

    assert_eq!((deleted, remaining), (1, vec!["active-cookie".to_string()]));
}

#[tokio::test]
async fn session_cleanup_task_should_delete_only_expired_sessions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("sessions.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAdminSessionStore::new(pool.clone());
    let now = Utc::now();
    insert_admin_user(&pool, "admin").await;
    insert_admin_session(
        &pool,
        "expired-session",
        "admin",
        now - Duration::minutes(1),
    )
    .await;
    insert_admin_session(
        &pool,
        "active-session",
        "admin",
        now + Duration::minutes(10),
    )
    .await;

    let deleted =
        codex_proxy_rs::runtime::tasks::session_cleanup::SessionCleanupTask::new(store, 3600)
            .cleanup_once_at(now)
            .await
            .expect("session cleanup should succeed");

    let remaining = admin_session_ids(&pool).await;

    assert_eq!(
        (deleted, remaining),
        (1, vec!["active-session".to_string()])
    );
}

#[tokio::test]
async fn session_affinity_cleanup_task_should_delete_only_expired_affinities() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("session-affinity-cleanup.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteSessionAffinityStore::new(pool.clone());
    let now = Utc::now();
    insert_account(&pool, "expired-account").await;
    insert_account(&pool, "active-account").await;
    store
        .upsert(
            "expired-response",
            &session_affinity_entry("expired-account", now - Duration::hours(2)),
            Duration::hours(1),
        )
        .await
        .expect("expired affinity should be inserted");
    store
        .upsert(
            "active-response",
            &session_affinity_entry("active-account", now),
            Duration::hours(1),
        )
        .await
        .expect("active affinity should be inserted");

    let deleted =
        codex_proxy_rs::runtime::tasks::session_affinity_cleanup::SessionAffinityCleanupTask::new(
            store, 3600,
        )
        .cleanup_once_at(now)
        .await
        .expect("session affinity cleanup should succeed");

    let remaining = session_affinity_response_ids(&pool).await;

    assert_eq!(
        (deleted, remaining),
        (1, vec!["active-response".to_string()])
    );
}
