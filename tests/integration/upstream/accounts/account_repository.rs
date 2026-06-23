use chrono::{Duration, Utc};
use codex_proxy_rs::{
    infra::{crypto::SecretBox, database::connect_sqlite},
    upstream::accounts::{
        model::{AccountStatus, AccountUsageDelta},
        store::{AccountStore, NewAccount, SqliteAccountStore},
    },
};
use secrecy::{ExposeSecret, SecretString};

#[tokio::test]
async fn account_repository_should_encrypt_tokens_and_load_decrypted_account() {
    let (pool, secret_box, _dir) = sqlite_account_store_parts("accounts.sqlite", 4).await;
    let repo = SqliteAccountStore::new(pool.clone(), secret_box);
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
        added_at: None,
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
    let (pool, secret_box, _dir) = sqlite_account_store_parts("accounts.sqlite", 5).await;
    let repo = SqliteAccountStore::new(pool.clone(), secret_box.clone());

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
async fn account_repository_should_list_metadata_without_decrypting_tokens() {
    let (pool, secret_box, _dir) = sqlite_account_store_parts("accounts.sqlite", 35).await;
    let repo = SqliteAccountStore::new(pool.clone(), secret_box);
    sqlx::query(
        "insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_corrupt")
    .bind("user@example.com")
    .bind("not-a-secret-box-cipher")
    .bind("active")
    .bind("2026-06-11T00:00:00Z")
    .bind("2026-06-11T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let page = repo.list_metadata(None, 10).await.unwrap();

    assert_eq!(page.items[0].id, "acct_corrupt");
    assert_eq!(page.items[0].email.as_deref(), Some("user@example.com"));
    assert_eq!(page.items[0].status, AccountStatus::Active);
}

#[tokio::test]
async fn account_repository_should_update_status_and_label_without_rewriting_tokens() {
    let (pool, secret_box, _dir) = sqlite_account_store_parts("accounts.sqlite", 6).await;
    let repo = SqliteAccountStore::new(pool.clone(), secret_box);

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
        added_at: None,
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
    let (pool, secret_box, _dir) = sqlite_account_store_parts("accounts.sqlite", 7).await;
    let repo = SqliteAccountStore::new(pool, secret_box);
    seed_new_account(&repo, "acct_a").await;

    AccountStore::record_usage_delta(
        &repo,
        "acct_a",
        AccountUsageDelta {
            input_tokens: 10,
            output_tokens: 4,
            cached_tokens: 3,
            reasoning_tokens: 2,
            total_tokens: 14,
            empty_responses: 0,
            requests: 1,
            ..AccountUsageDelta::default()
        },
    )
    .await
    .unwrap();
    AccountStore::record_usage_delta(
        &repo,
        "acct_a",
        AccountUsageDelta {
            input_tokens: 8,
            output_tokens: 2,
            cached_tokens: 1,
            reasoning_tokens: 1,
            total_tokens: 10,
            empty_responses: 1,
            requests: 1,
            ..AccountUsageDelta::default()
        },
    )
    .await
    .unwrap();

    let usage = repo.get_usage("acct_a").await.unwrap().unwrap();
    assert_eq!(usage.request_count, 2);
    assert_eq!(usage.input_tokens, 18);
    assert_eq!(usage.output_tokens, 6);
    assert_eq!(usage.cached_tokens, 4);
    assert_eq!(usage.reasoning_tokens, 3);
    assert_eq!(usage.total_tokens, 24);
    assert_eq!(usage.empty_response_count, 1);
    assert!(usage.last_used_at.is_some());
}

#[tokio::test]
async fn account_repository_should_restore_window_usage_into_runtime_pool_accounts() {
    let (pool, secret_box, _dir) = sqlite_account_store_parts("accounts.sqlite", 33).await;
    let repo = SqliteAccountStore::new(pool.clone(), secret_box.clone());
    let window_started_at = Utc::now() - Duration::minutes(2);
    let window_reset_at = Utc::now() + Duration::minutes(3);

    seed_repo_account(&pool, &secret_box, "acct_a", "2026-06-11T00:00:00Z").await;
    sqlx::query(
        "insert into account_usage (account_id, request_count, image_input_tokens, image_output_tokens, image_request_count, image_request_failed_count, window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens, window_image_input_tokens, window_image_output_tokens, window_image_request_count, window_image_request_failed_count, window_started_at, window_reset_at, limit_window_seconds) values (?, 9, 31, 9, 2, 1, 7, 11, 13, 17, 19, 5, 1, 1, ?, ?, 300)",
    )
    .bind("acct_a")
    .bind(window_started_at.to_rfc3339())
    .bind(window_reset_at.to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();

    let account = AccountStore::list_pool_accounts(&repo)
        .await
        .unwrap()
        .remove(0);

    assert_eq!(account.window_request_count, 7);
    assert_eq!(account.image_input_tokens, 31);
    assert_eq!(account.image_output_tokens, 9);
    assert_eq!(account.image_request_count, 2);
    assert_eq!(account.image_request_failed_count, 1);
    assert_eq!(account.window_input_tokens, 11);
    assert_eq!(account.window_output_tokens, 13);
    assert_eq!(account.window_cached_tokens, 17);
    assert_eq!(account.window_image_input_tokens, 19);
    assert_eq!(account.window_image_output_tokens, 5);
    assert_eq!(account.window_image_request_count, 1);
    assert_eq!(account.window_image_request_failed_count, 1);
    assert_eq!(account.window_started_at, Some(window_started_at));
    assert_eq!(account.window_reset_at, Some(window_reset_at));
    assert_eq!(account.limit_window_seconds, Some(300));
}

#[tokio::test]
async fn account_repository_should_restore_quota_verify_required_into_runtime_pool_accounts() {
    let (pool, secret_box, _dir) = sqlite_account_store_parts("accounts.sqlite", 34).await;
    let repo = SqliteAccountStore::new(pool.clone(), secret_box.clone());

    seed_repo_account(&pool, &secret_box, "acct_a", "2026-06-11T00:00:00Z").await;
    sqlx::query("update accounts set quota_verify_required = 1 where id = ?")
        .bind("acct_a")
        .execute(&pool)
        .await
        .unwrap();

    let account = AccountStore::list_pool_accounts(&repo)
        .await
        .unwrap()
        .remove(0);

    assert!(account.quota_verify_required);
}

#[tokio::test]
async fn account_repository_should_update_quota_json_and_fetched_at() {
    let (pool, secret_box, _dir) = sqlite_account_store_parts("accounts.sqlite", 36).await;
    let repo = SqliteAccountStore::new(pool.clone(), secret_box.clone());
    seed_repo_account(&pool, &secret_box, "acct_a", "2026-06-11T00:00:00Z").await;

    let updated = repo
        .update_quota_json(
            "acct_a",
            r#"{"plan_type":"free","rate_limit":{"limit_reached":false}}"#,
        )
        .await
        .unwrap();
    let missing_updated = repo
        .update_quota_json("missing", r#"{"rate_limit":null}"#)
        .await
        .unwrap();
    let row: (String, Option<String>, Option<String>) =
        sqlx::query_as("select quota_json, quota_fetched_at, plan_type from accounts where id = ?")
            .bind("acct_a")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(
        (updated, missing_updated, row.0, row.1.is_some(), row.2),
        (
            true,
            false,
            r#"{"plan_type":"free","rate_limit":{"limit_reached":false}}"#.to_string(),
            true,
            Some("free".to_string())
        )
    );
}

async fn sqlite_account_store_parts(
    db_name: &str,
    key_byte: u8,
) -> (sqlx::SqlitePool, SecretBox, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    (pool, SecretBox::new([key_byte; 32]), dir)
}

async fn seed_new_account(repo: &SqliteAccountStore, id: &str) {
    repo.insert(NewAccount {
        id: id.to_string(),
        email: None,
        account_id: None,
        user_id: None,
        label: None,
        plan_type: None,
        access_token: SecretString::new("access-secret".to_string().into()),
        refresh_token: None,
        access_token_expires_at: None,
        status: AccountStatus::Active,
        added_at: None,
    })
    .await
    .unwrap();
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
