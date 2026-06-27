use chrono::{Duration, Utc};
use codex_proxy_rs::{
    infra::{crypto::SecretBox, database::connect_sqlite},
    upstream::accounts::{
        cookies::SqliteCookieStore,
        model::AccountStatus,
        store::{NewAccount, SqliteAccountStore},
    },
};
use secrecy::SecretString;
use sqlx::SqlitePool;

#[tokio::test]
async fn cookie_store_should_cleanup_expired_cookies() {
    let (pool, secret_box, _dir) = sqlite_cookie_store_parts("cookies-cleanup.sqlite").await;
    seed_account(&pool, "acct_a").await;
    let store = SqliteCookieStore::new(pool.clone(), secret_box);

    let now = Utc::now();
    let past = now - Duration::hours(2);
    let future = now + Duration::hours(2);

    sqlx::query(
        "insert into account_cookies (id, account_id, domain, name, value_cipher, path, expires_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("cookie_expired")
    .bind("acct_a")
    .bind(".chatgpt.com")
    .bind("cf_clearance")
    .bind("old_cipher")
    .bind("/")
    .bind(past.to_rfc3339())
    .bind(past.to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "insert into account_cookies (id, account_id, domain, name, value_cipher, path, expires_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("cookie_valid")
    .bind("acct_a")
    .bind(".chatgpt.com")
    .bind("cf_clearance")
    .bind("new_cipher")
    .bind("/valid")
    .bind(future.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();

    let deleted = store.cleanup_expired(now).await.unwrap();
    assert_eq!(deleted, 1);

    let remaining: (i64,) = sqlx::query_as("select count(*) from account_cookies")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining.0, 1);
}

async fn sqlite_cookie_store_parts(name: &str) -> (SqlitePool, SecretBox, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    (pool, SecretBox::new([21u8; 32]), dir)
}

async fn seed_account(pool: &SqlitePool, id: &str) {
    SqliteAccountStore::new(pool.clone())
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
            added_at: None,
        })
        .await
        .unwrap();
}
