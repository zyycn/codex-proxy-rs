use chrono::Utc;

use codex_proxy_rs::{
    codex::accounts::cookies::{jar::CookieJar, repository::CookieRepository},
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
        "__cf_bm=secret-cookie; Domain=.chatgpt.com; Path=/",
    )
    .await
    .unwrap();

    let stored: (String,) = sqlx::query_as(
        "select value_cipher from account_cookies where account_id = ? and name = ?",
    )
    .bind("acct_a")
    .bind("__cf_bm")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.0.starts_with("v1:"));
    assert!(!stored.0.contains("secret-cookie"));
    assert_eq!(
        repo.cookie_header("acct_a", "chatgpt.com").await.unwrap(),
        Some("__cf_bm=secret-cookie".to_string())
    );
    assert_eq!(
        repo.cookie_header("acct_b", "chatgpt.com").await.unwrap(),
        None
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
