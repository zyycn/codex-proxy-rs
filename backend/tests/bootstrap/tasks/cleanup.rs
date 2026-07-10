use super::*;

#[tokio::test]
async fn cookie_cleanup_task_should_delete_only_expired_cookie_rows() {
    let (pool, _guard) = init_test_db("cookie-cleanup").await;
    let store = PgCookieStore::new(pool.clone());
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

    let deleted = codex_proxy_rs::bootstrap::tasks::cookie_cleanup::CookieCleanupTask::new(store)
        .cleanup_once_at(now)
        .await
        .expect("cookie cleanup should succeed");

    let remaining = cookie_ids(&pool).await;

    assert_eq!((deleted, remaining), (1, vec!["active-cookie".to_string()]));
}
