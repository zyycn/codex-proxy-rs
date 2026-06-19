use chrono::{Duration, Utc};
use codex_proxy_adapters::sqlite::{
    accounts::{NewAccount, SqliteAccountStore},
    cookies::SqliteCookieStore,
};
use codex_proxy_core::accounts::model::AccountStatus;
use codex_proxy_platform::{crypto::SecretBox, storage::connect_sqlite};
use secrecy::SecretString;
use sqlx::SqlitePool;

#[tokio::test]
async fn cookie_store_should_capture_cf_clearance_and_replay_by_domain_and_path() {
    let (pool, secret_box, _dir) = sqlite_cookie_store_parts("captured-cookies.sqlite").await;
    seed_account(&pool, &secret_box, "acct_a").await;
    let store = SqliteCookieStore::new(pool, secret_box);

    store
        .capture_set_cookie("acct_a", "cf_clearance=root; Domain=.chatgpt.com; Path=/")
        .await
        .unwrap();
    store
        .capture_set_cookie(
            "acct_a",
            "cf_clearance=codex; Domain=.chatgpt.com; Path=/codex",
        )
        .await
        .unwrap();

    assert_eq!(
        store
            .cookie_header_for_request("acct_a", "chatgpt.com", "/codex/responses")
            .await
            .unwrap(),
        Some("cf_clearance=codex; cf_clearance=root".to_string())
    );
    assert_eq!(
        store
            .cookie_header_for_request("acct_a", "chatgpt.com", "/api/codex/usage")
            .await
            .unwrap(),
        Some("cf_clearance=root".to_string())
    );
}

#[tokio::test]
async fn cookie_store_should_ignore_non_capturable_set_cookie_headers() {
    let (pool, secret_box, _dir) = sqlite_cookie_store_parts("ignored-cookies.sqlite").await;
    seed_account(&pool, &secret_box, "acct_a").await;
    let store = SqliteCookieStore::new(pool.clone(), secret_box);

    store
        .capture_set_cookie("acct_a", "__cf_bm=bot-session; Domain=.chatgpt.com; Path=/")
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
        store.cookie_header("acct_a", "chatgpt.com").await.unwrap(),
        None
    );
}

async fn sqlite_cookie_store_parts(name: &str) -> (SqlitePool, SecretBox, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    (pool, SecretBox::new([21u8; 32]), dir)
}

async fn seed_account(pool: &SqlitePool, secret_box: &SecretBox, id: &str) {
    SqliteAccountStore::new(pool.clone(), secret_box.clone())
        .insert(NewAccount {
            id: id.to_string(),
            email: Some(format!("{id}@example.com")),
            account_id: Some(format!("chatgpt-{id}")),
            user_id: Some(format!("user-{id}")),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(format!("access-{id}").into()),
            refresh_token: None,
            access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
            status: AccountStatus::Active,
        })
        .await
        .unwrap();
}
