use chrono::{Duration, Utc};
use codex_proxy_rs::fleet::{
    account::AccountStatus,
    cookies::PgCookieStore,
    store::{NewAccount, PgAccountStore},
};
use secrecy::SecretString;
use sqlx::PgPool;

use crate::support::storage::init_test_db;

#[tokio::test]
async fn cookie_store_should_cleanup_expired_cookies() {
    let (pool, _dir) = cookie_store_parts("cookies-cleanup").await;
    seed_account(&pool, "acct_a").await;
    let store = PgCookieStore::new(pool.clone());

    let now = Utc::now();
    let past = now - Duration::hours(2);
    let future = now + Duration::hours(2);

    sqlx::query(
        "insert into account_cookies (id, account_id, domain, name, value, path, expires_at, updated_at) values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind("cookie_expired")
    .bind("acct_a")
    .bind(".chatgpt.com")
    .bind("cf_clearance")
    .bind("old_cipher")
    .bind("/")
    .bind(past)
    .bind(past)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "insert into account_cookies (id, account_id, domain, name, value, path, expires_at, updated_at) values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind("cookie_valid")
    .bind("acct_a")
    .bind(".chatgpt.com")
    .bind("cf_clearance")
    .bind("new_cipher")
    .bind("/valid")
    .bind(future)
    .bind(now)
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

#[tokio::test]
async fn cookie_store_should_persist_account_cleanup_deadline() {
    let (pool, _dir) = cookie_store_parts("cookies-cleanup-deadline").await;
    seed_account(&pool, "acct_deadline").await;
    let store = PgCookieStore::new(pool.clone());
    store
        .capture_set_cookie(
            "acct_deadline",
            "cf_clearance=keep-until-deadline; Domain=.chatgpt.com; Path=/",
        )
        .await
        .unwrap();
    let deadline = Utc::now() + Duration::seconds(30);

    let updated = store
        .expire_account_cookies_at("acct_deadline", deadline)
        .await
        .unwrap();
    let stored_deadline: (Option<chrono::DateTime<Utc>>,) =
        sqlx::query_as("select expires_at from account_cookies where account_id = $1")
            .bind("acct_deadline")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(updated, 1);
    assert_eq!(
        stored_deadline.0.map(|value| value.timestamp_micros()),
        Some(deadline.timestamp_micros())
    );
    assert_eq!(
        store
            .cookie_header("acct_deadline", "chatgpt.com")
            .await
            .unwrap()
            .as_deref(),
        Some("cf_clearance=keep-until-deadline")
    );
    assert_eq!(
        store
            .cleanup_expired(deadline + Duration::seconds(1))
            .await
            .unwrap(),
        1
    );
}

async fn cookie_store_parts(name: &str) -> (PgPool, crate::support::storage::TestDatabaseGuard) {
    init_test_db(name).await
}

async fn seed_account(pool: &PgPool, id: &str) {
    PgAccountStore::new(pool.clone())
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
