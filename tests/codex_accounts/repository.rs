use chrono::{Duration, Utc};
use secrecy::{ExposeSecret, SecretString};

use codex_proxy_rs::{
    codex::accounts::{
        model::AccountStatus,
        repository::{AccountRepository, NewAccount, TokenUpdate, UsageDelta},
    },
    platform::crypto::SecretBox,
    platform::storage::db::connect_sqlite,
};

#[tokio::test]
async fn account_repository_should_encrypt_tokens_and_load_decrypted_account() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AccountRepository::new(pool.clone(), SecretBox::new([4u8; 32]));
    let expires_at = Utc::now() + Duration::hours(1);

    repo.insert(NewAccount {
        id: "acct_a".to_string(),
        email: Some("user@example.com".to_string()),
        account_id: Some("chatgpt-account".to_string()),
        user_id: Some("chatgpt-user".to_string()),
        label: Some("primary".to_string()),
        plan_type: Some("plus".to_string()),
        access_token: SecretString::new("access-secret".to_string().into()),
        refresh_token: Some(SecretString::new("refresh-secret".to_string().into())),
        access_token_expires_at: Some(expires_at),
        status: AccountStatus::Active,
    })
    .await
    .unwrap();

    let stored: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_a")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.0.starts_with("v1:"));
    assert!(!stored.0.contains("access-secret"));
    assert!(stored.1.starts_with("v1:"));
    assert!(!stored.1.contains("refresh-secret"));

    let loaded = repo.get("acct_a").await.unwrap().unwrap();
    assert_eq!(loaded.access_token.expose_secret(), "access-secret");
    assert_eq!(
        loaded.refresh_token.unwrap().expose_secret(),
        "refresh-secret"
    );
    assert_eq!(loaded.email.as_deref(), Some("user@example.com"));
    assert_eq!(loaded.status, AccountStatus::Active);
}

#[tokio::test]
async fn account_repository_should_cursor_page_accounts_by_added_at_desc() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([5u8; 32]);
    let repo = AccountRepository::new(pool.clone(), secret_box.clone());

    seed_repo_account(&pool, &secret_box, "acct_a", "2026-06-11T00:00:00Z").await;
    seed_repo_account(&pool, &secret_box, "acct_b", "2026-06-11T00:01:00Z").await;
    seed_repo_account(&pool, &secret_box, "acct_c", "2026-06-11T00:02:00Z").await;

    let first_page = repo.list(None, 2).await.unwrap();
    assert_eq!(
        first_page
            .items
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["acct_c", "acct_b"]
    );

    let second_page = repo.list(first_page.next_cursor, 2).await.unwrap();
    assert_eq!(
        second_page
            .items
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["acct_a"]
    );
}

#[tokio::test]
async fn account_repository_should_update_status_and_label_without_rewriting_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AccountRepository::new(pool.clone(), SecretBox::new([6u8; 32]));

    repo.insert(NewAccount {
        id: "acct_a".to_string(),
        email: None,
        account_id: None,
        user_id: None,
        label: None,
        plan_type: None,
        access_token: SecretString::new("access-secret".to_string().into()),
        refresh_token: None,
        access_token_expires_at: None,
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    let before: (String,) = sqlx::query_as("select access_token_cipher from accounts where id = ?")
        .bind("acct_a")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(repo
        .set_status("acct_a", AccountStatus::Disabled)
        .await
        .unwrap());
    assert!(repo
        .set_label("acct_a", Some("work".to_string()))
        .await
        .unwrap());

    let after: (String,) = sqlx::query_as("select access_token_cipher from accounts where id = ?")
        .bind("acct_a")
        .fetch_one(&pool)
        .await
        .unwrap();
    let loaded = repo.get("acct_a").await.unwrap().unwrap();
    assert_eq!(after.0, before.0);
    assert_eq!(loaded.status, AccountStatus::Disabled);
    assert_eq!(loaded.label.as_deref(), Some("work"));
}

#[tokio::test]
async fn account_repository_should_accumulate_usage_counters() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AccountRepository::new(pool.clone(), SecretBox::new([7u8; 32]));

    repo.insert(NewAccount {
        id: "acct_a".to_string(),
        email: None,
        account_id: None,
        user_id: None,
        label: None,
        plan_type: None,
        access_token: SecretString::new("access-secret".to_string().into()),
        refresh_token: None,
        access_token_expires_at: None,
        status: AccountStatus::Active,
    })
    .await
    .unwrap();

    repo.record_usage(
        "acct_a",
        UsageDelta {
            input_tokens: 10,
            output_tokens: 4,
            cached_tokens: 3,
        },
    )
    .await
    .unwrap();
    repo.record_usage(
        "acct_a",
        UsageDelta {
            input_tokens: 8,
            output_tokens: 2,
            cached_tokens: 1,
        },
    )
    .await
    .unwrap();

    let usage = repo.get_usage("acct_a").await.unwrap().unwrap();
    assert_eq!(usage.request_count, 2);
    assert_eq!(usage.input_tokens, 18);
    assert_eq!(usage.output_tokens, 6);
    assert_eq!(usage.cached_tokens, 4);
    assert!(usage.last_used_at.is_some());
}

#[tokio::test]
async fn account_repository_should_load_usage_count_into_runtime_pool_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AccountRepository::new(pool.clone(), SecretBox::new([17u8; 32]));

    repo.insert(NewAccount {
        id: "acct_a".to_string(),
        email: None,
        account_id: None,
        user_id: None,
        label: None,
        plan_type: None,
        access_token: SecretString::new("access-secret".to_string().into()),
        refresh_token: None,
        access_token_expires_at: None,
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    repo.record_usage(
        "acct_a",
        UsageDelta {
            input_tokens: 1,
            output_tokens: 2,
            cached_tokens: 3,
        },
    )
    .await
    .unwrap();

    let account = repo.list_pool_accounts().await.unwrap().remove(0);

    assert_eq!(account.request_count, 1);
}

#[tokio::test]
async fn account_repository_should_exclude_refresh_lease_until_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AccountRepository::new(pool.clone(), SecretBox::new([18u8; 32]));
    let now = Utc::now();
    repo.insert(NewAccount {
        id: "acct_a".to_string(),
        email: None,
        account_id: None,
        user_id: None,
        label: None,
        plan_type: None,
        access_token: SecretString::new("access-secret".to_string().into()),
        refresh_token: None,
        access_token_expires_at: None,
        status: AccountStatus::Active,
    })
    .await
    .unwrap();

    assert!(repo
        .try_acquire_refresh_lease("acct_a", "owner-a", now + Duration::minutes(5))
        .await
        .unwrap());
    assert!(!repo
        .try_acquire_refresh_lease("acct_a", "owner-b", now + Duration::minutes(5))
        .await
        .unwrap());
    sqlx::query("update account_refresh_leases set expires_at = ? where account_id = ?")
        .bind((now - Duration::seconds(1)).to_rfc3339())
        .bind("acct_a")
        .execute(&pool)
        .await
        .unwrap();
    assert!(repo
        .try_acquire_refresh_lease("acct_a", "owner-b", now + Duration::minutes(5))
        .await
        .unwrap());
}

#[tokio::test]
async fn account_repository_should_preserve_refresh_token_when_update_omits_one() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = AccountRepository::new(pool, SecretBox::new([8u8; 32]));

    repo.insert(NewAccount {
        id: "acct_a".to_string(),
        email: None,
        account_id: None,
        user_id: None,
        label: None,
        plan_type: None,
        access_token: SecretString::new("old-access".to_string().into()),
        refresh_token: Some(SecretString::new("old-refresh".to_string().into())),
        access_token_expires_at: None,
        status: AccountStatus::Active,
    })
    .await
    .unwrap();

    assert!(repo
        .update_tokens(
            "acct_a",
            TokenUpdate {
                access_token: SecretString::new("new-access".to_string().into()),
                refresh_token: None,
                access_token_expires_at: Some(Utc::now() + Duration::hours(2)),
            },
        )
        .await
        .unwrap());

    let loaded = repo.get("acct_a").await.unwrap().unwrap();
    assert_eq!(loaded.access_token.expose_secret(), "new-access");
    assert_eq!(loaded.refresh_token.unwrap().expose_secret(), "old-refresh");
    assert_eq!(loaded.status, AccountStatus::Active);
    assert!(loaded.access_token_expires_at.is_some());
}

async fn seed_repo_account(
    pool: &sqlx::SqlitePool,
    secret_box: &SecretBox,
    id: &str,
    added_at: &str,
) {
    let token = SecretString::new(format!("access-{id}").into());
    let cipher = secret_box.encrypt(&token).unwrap();
    sqlx::query(
        "insert into accounts (id, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(cipher)
    .bind("active")
    .bind(added_at)
    .bind(added_at)
    .execute(pool)
    .await
    .unwrap();
}
