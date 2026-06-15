use chrono::Utc;

use codex_proxy_rs::{
    codex::accounts::cookies::{jar::CookieJar, repository::CookieRepository},
    codex::tasks::cookie_cleanup::CookieCleanupScheduler,
    platform::crypto::SecretBox,
    platform::storage::db::connect_sqlite,
};

#[test]
fn cookie_jar_captures_and_replays_account_scoped_cookies() {
    let mut jar = CookieJar::default();
    jar.capture_set_cookie(
        "acct_a",
        "cf_clearance=abc; Domain=chatgpt.com; Path=/; HttpOnly",
    );
    jar.capture_set_cookie(
        "acct_b",
        "cf_clearance=def; Domain=chatgpt.com; Path=/; HttpOnly",
    );

    assert_eq!(
        jar.cookie_header("acct_a", "chatgpt.com"),
        Some("cf_clearance=abc".to_string())
    );
    assert_eq!(
        jar.cookie_header("acct_b", "chatgpt.com"),
        Some("cf_clearance=def".to_string())
    );
}

#[tokio::test]
async fn cookie_repository_encrypts_and_replays_account_scoped_cookies() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_account(&pool, "acct_a").await;
    seed_account(&pool, "acct_b").await;
    let repo = CookieRepository::new(pool.clone(), SecretBox::new([9u8; 32]));

    repo.capture_set_cookie(
        "acct_a",
        "cf_clearance=secret-cookie; Domain=.chatgpt.com; Path=/",
    )
    .await
    .unwrap();

    let stored: (String,) = sqlx::query_as(
        "select value_cipher from account_cookies where account_id = ? and name = ?",
    )
    .bind("acct_a")
    .bind("cf_clearance")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.0.starts_with("v1:"));
    assert!(!stored.0.contains("secret-cookie"));
    assert_eq!(
        repo.cookie_header("acct_a", "chatgpt.com").await.unwrap(),
        Some("cf_clearance=secret-cookie".to_string())
    );
    assert_eq!(
        repo.cookie_header("acct_b", "chatgpt.com").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn cookie_repository_should_scope_cookie_replay_by_domain_and_path() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("path-scoped-cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_account(&pool, "acct_a").await;
    let repo = CookieRepository::new(pool, SecretBox::new([11u8; 32]));

    repo.capture_set_cookie("acct_a", "cf_clearance=root; Domain=.chatgpt.com; Path=/")
        .await
        .unwrap();
    repo.capture_set_cookie("acct_a", "cf_clearance=api; Domain=.chatgpt.com; Path=/api")
        .await
        .unwrap();

    assert_eq!(
        repo.cookie_header_for_request("acct_a", "chatgpt.com", "/codex/responses")
            .await
            .unwrap(),
        Some("cf_clearance=root".to_string())
    );
    assert_eq!(
        repo.cookie_header_for_request("acct_a", "chatgpt.com", "/api/codex/usage")
            .await
            .unwrap(),
        Some("cf_clearance=api; cf_clearance=root".to_string())
    );
}

#[tokio::test]
async fn cookie_repository_should_only_auto_capture_cf_clearance() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("capturable-cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_account(&pool, "acct_a").await;
    let repo = CookieRepository::new(pool.clone(), SecretBox::new([13u8; 32]));

    repo.capture_set_cookie("acct_a", "__cf_bm=bot-session; Domain=.chatgpt.com; Path=/")
        .await
        .unwrap();

    let stored_count: (i64,) =
        sqlx::query_as("select count(*) from account_cookies where account_id = ?")
            .bind("acct_a")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_count.0, 0);
    assert_eq!(
        repo.cookie_header("acct_a", "chatgpt.com").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn cookie_repository_manual_cookie_header_should_allow_non_capturable_cookies() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("manual-cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_account(&pool, "acct_a").await;
    let repo = CookieRepository::new(pool, SecretBox::new([17u8; 32]));

    let stored = repo
        .set_cookie_header("acct_a", "cf_clearance=clear; __cf_bm=bm")
        .await
        .unwrap();

    assert_eq!(stored, 2);
    assert_eq!(
        repo.cookie_header("acct_a", "chatgpt.com").await.unwrap(),
        Some("__cf_bm=bm; cf_clearance=clear".to_string())
    );
}

#[tokio::test]
async fn cookie_repository_should_not_replay_expired_cookies() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("expired-cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_account(&pool, "acct_a").await;
    let repo = CookieRepository::new(pool, SecretBox::new([19u8; 32]));

    repo.capture_set_cookie(
        "acct_a",
        "cf_clearance=expired; Domain=.chatgpt.com; Path=/; Expires=Wed, 21 Oct 2015 07:28:00 GMT",
    )
    .await
    .unwrap();

    assert_eq!(
        repo.cookie_header("acct_a", "chatgpt.com").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn cookie_repository_cleanup_expired_cookies_should_delete_only_expired_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("cleanup-expired-cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_account(&pool, "acct_a").await;
    let repo = CookieRepository::new(pool.clone(), SecretBox::new([29u8; 32]));

    repo.capture_set_cookie(
        "acct_a",
        "cf_clearance=expired; Domain=.chatgpt.com; Path=/; Expires=Wed, 21 Oct 2015 07:28:00 GMT",
    )
    .await
    .unwrap();
    repo.set_cookie_header("acct_a", "__cf_bm=active")
        .await
        .unwrap();

    let deleted = repo.cleanup_expired(Utc::now()).await.unwrap();
    let stored_count: (i64,) =
        sqlx::query_as("select count(*) from account_cookies where account_id = ?")
            .bind("acct_a")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(deleted, 1);
    assert_eq!(stored_count.0, 1);
    assert_eq!(
        repo.cookie_header("acct_a", "chatgpt.com").await.unwrap(),
        Some("__cf_bm=active".to_string())
    );
}

#[tokio::test]
async fn cookie_cleanup_scheduler_should_delete_expired_cookie_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("scheduled-cookie-cleanup.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_account(&pool, "acct_a").await;
    let repo = CookieRepository::new(pool.clone(), SecretBox::new([31u8; 32]));

    repo.capture_set_cookie(
        "acct_a",
        "cf_clearance=expired; Domain=.chatgpt.com; Path=/; Max-Age=0",
    )
    .await
    .unwrap();

    let scheduler = CookieCleanupScheduler::new(repo);
    let deleted = scheduler.cleanup_once().await.unwrap();
    let stored_count: (i64,) = sqlx::query_as("select count(*) from account_cookies")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(deleted, 1);
    assert_eq!(stored_count.0, 0);
}

#[tokio::test]
async fn cookie_repository_should_treat_max_age_as_higher_priority_than_expires() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("max-age-cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_account(&pool, "acct_a").await;
    let repo = CookieRepository::new(pool, SecretBox::new([23u8; 32]));

    repo.capture_set_cookie(
        "acct_a",
        "cf_clearance=expired; Domain=.chatgpt.com; Path=/; Max-Age=0; Expires=Wed, 21 Oct 2999 07:28:00 GMT",
    )
    .await
    .unwrap();

    assert_eq!(
        repo.cookie_header("acct_a", "chatgpt.com").await.unwrap(),
        None
    );
}

async fn seed_account(pool: &sqlx::SqlitePool, id: &str) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into accounts (id, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind("v1:test:test")
    .bind("active")
    .bind(&now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}
