use chrono::{DateTime, Duration, Utc};
use codex_proxy_rs::{
    infra::database::connect_sqlite,
    upstream::accounts::{
        model::{AccountStatus, AccountUsageDelta},
        store::{AccountClaimsUpdate, AccountStore, NewAccount, SqliteAccountStore},
    },
};
use secrecy::{ExposeSecret, SecretString};

#[tokio::test]
async fn account_repository_should_store_plain_tokens_and_load_secret_wrappers() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 4).await;
    let repo = SqliteAccountStore::new(pool.clone());
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

    let stored: (String, String) =
        sqlx::query_as("select access_token, refresh_token from accounts where id = ?")
            .bind("acct_a")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.0, "access-secret");
    assert_eq!(stored.1, "refresh-secret");

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
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 5).await;
    let repo = SqliteAccountStore::new(pool.clone());

    seed_repo_account(&pool, "acct_a", "2026-06-11T00:00:00Z").await;
    seed_repo_account(&pool, "acct_b", "2026-06-11T00:01:00Z").await;
    seed_repo_account(&pool, "acct_c", "2026-06-11T00:02:00Z").await;

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
async fn account_repository_should_list_metadata_without_exposing_tokens() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 35).await;
    let repo = SqliteAccountStore::new(pool.clone());
    sqlx::query(
        "insert into accounts (id, email, access_token, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_plain")
    .bind("user@example.com")
    .bind("plain-access-token")
    .bind("active")
    .bind("2026-06-11T00:00:00Z")
    .bind("2026-06-11T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let page = repo.list_metadata(None, 10).await.unwrap();

    assert_eq!(page.items[0].id, "acct_plain");
    assert_eq!(page.items[0].email.as_deref(), Some("user@example.com"));
    assert_eq!(page.items[0].status, AccountStatus::Active);
}

#[tokio::test]
async fn account_repository_should_list_pool_accounts_with_tokens_and_plan() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 39).await;
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into accounts (
            id, email, chatgpt_account_id, chatgpt_user_id, label, plan_type, access_token, refresh_token,
            access_token_expires_at, status, added_at, updated_at
        ) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_1")
    .bind("user@example.com")
    .bind("chatgpt-account")
    .bind(Option::<String>::None)
    .bind("primary")
    .bind("plus")
    .bind("access-token")
    .bind(Some("refresh-token"))
    .bind(Option::<String>::None)
    .bind("active")
    .bind(&now)
    .bind(&now)
    .execute(&pool)
    .await
    .expect("insert account");

    let store = SqliteAccountStore::new(pool);
    let accounts = AccountStore::list_pool_accounts(&store)
        .await
        .expect("list accounts");

    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, "acct_1");
    assert_eq!(accounts[0].access_token, "access-token");
    assert_eq!(accounts[0].plan_type.as_deref(), Some("plus"));
}

#[tokio::test]
async fn account_repository_should_update_status_and_label_without_rewriting_tokens() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 6).await;
    let repo = SqliteAccountStore::new(pool.clone());

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
    let before: (String,) = sqlx::query_as("select access_token from accounts where id = ?")
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

    let after: (String,) = sqlx::query_as("select access_token from accounts where id = ?")
        .bind("acct_a")
        .fetch_one(&pool)
        .await
        .unwrap();
    let updated_at: (String,) = sqlx::query_as("select updated_at from accounts where id = ?")
        .bind("acct_a")
        .fetch_one(&pool)
        .await
        .unwrap();
    let loaded = repo.get("acct_a").await.unwrap().unwrap();
    assert_eq!(after.0, before.0);
    assert_eq!(loaded.status, AccountStatus::Disabled);
    assert_eq!(loaded.label.as_deref(), Some("work"));
    assert!(updated_at.0.parse::<DateTime<Utc>>().is_ok());
}

#[tokio::test]
async fn account_repository_should_not_reactivate_disabled_or_banned_accounts_from_claims_update() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 11).await;
    let repo = SqliteAccountStore::new(pool);
    seed_new_account(&repo, "acct_disabled").await;
    seed_new_account(&repo, "acct_banned").await;
    repo.set_status("acct_disabled", AccountStatus::Disabled)
        .await
        .expect("disabled status should persist");
    repo.set_status("acct_banned", AccountStatus::Banned)
        .await
        .expect("banned status should persist");

    for account_id in ["acct_disabled", "acct_banned"] {
        assert!(repo
            .update_from_claims(
                account_id,
                AccountClaimsUpdate {
                    email: Some(format!("{account_id}@example.com")),
                    account_id: Some(format!("chatgpt-{account_id}")),
                    user_id: Some(format!("user-{account_id}")),
                    plan_type: Some("plus".to_string()),
                    access_token: SecretString::new(format!("new-access-{account_id}").into()),
                    refresh_token: Some(SecretString::new(
                        format!("new-refresh-{account_id}").into(),
                    )),
                    access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
                    next_refresh_at: Some(Utc::now() + Duration::minutes(30)),
                    status: AccountStatus::Active,
                },
            )
            .await
            .expect("claims update should persist"));
    }

    let disabled = repo
        .get("acct_disabled")
        .await
        .expect("account should load")
        .expect("account should exist");
    let banned = repo
        .get("acct_banned")
        .await
        .expect("account should load")
        .expect("account should exist");

    assert_eq!(disabled.status, AccountStatus::Disabled);
    assert_eq!(
        disabled.access_token.expose_secret(),
        "new-access-acct_disabled"
    );
    assert_eq!(banned.status, AccountStatus::Banned);
    assert_eq!(
        banned.access_token.expose_secret(),
        "new-access-acct_banned"
    );
}

#[tokio::test]
async fn account_repository_should_accumulate_usage_counters() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 7).await;
    let repo = SqliteAccountStore::new(pool.clone());
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

    let usage: (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        r"
        select request_count, input_tokens, output_tokens, cached_tokens,
               reasoning_tokens, total_tokens, empty_response_count,
               window_started_at, last_used_at
        from account_usage
        where account_id = ?
        ",
    )
    .bind("acct_a")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage.0, 2);
    assert_eq!(usage.1, 18);
    assert_eq!(usage.2, 6);
    assert_eq!(usage.3, 4);
    assert_eq!(usage.4, 3);
    assert_eq!(usage.5, 24);
    assert_eq!(usage.6, 1);
    assert!(usage.7.is_some());
    assert!(usage.8.is_some());
}

#[tokio::test]
async fn account_repository_should_restore_window_usage_into_runtime_pool_accounts() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 33).await;
    let repo = SqliteAccountStore::new(pool.clone());
    let window_started_at = Utc::now() - Duration::minutes(2);
    let window_reset_at = Utc::now() + Duration::minutes(3);

    seed_repo_account(&pool, "acct_a", "2026-06-11T00:00:00Z").await;
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
async fn account_repository_should_restore_window_from_quota_json_when_usage_window_is_missing() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 36).await;
    let repo = SqliteAccountStore::new(pool.clone());
    let reset_at = DateTime::<Utc>::from_timestamp(1_806_364_800, 0).unwrap();

    seed_repo_account(&pool, "acct_a", "2026-06-11T00:00:00Z").await;
    sqlx::query("update accounts set quota_json = ? where id = ?")
        .bind(
            r#"{
                "snapshots": [{
                    "source": "core",
                    "blocked": false,
                    "primary": {
                        "used_percent": 11,
                        "remaining_percent": 89,
                        "reset_at": 1806364800,
                        "window_minutes": 43200,
                        "limit_reached": false
                    },
                    "secondary": null
                }],
                "monthly_limit": {
                    "key": "core-monthly",
                    "source": "rate_limit",
                    "used_percent": 11,
                    "remaining_percent": 89,
                    "reset_at": 1806364800,
                    "window_minutes": 43200,
                    "limit_reached": false
                }
            }"#,
        )
        .bind("acct_a")
        .execute(&pool)
        .await
        .unwrap();

    let account = AccountStore::list_pool_accounts(&repo)
        .await
        .unwrap()
        .remove(0);

    assert_eq!(account.window_reset_at, Some(reset_at));
    assert_eq!(account.limit_window_seconds, Some(2_592_000));
}

#[tokio::test]
async fn account_repository_should_restore_quota_verify_required_into_runtime_pool_accounts() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 34).await;
    let repo = SqliteAccountStore::new(pool.clone());

    seed_repo_account(&pool, "acct_a", "2026-06-11T00:00:00Z").await;
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
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 36).await;
    let repo = SqliteAccountStore::new(pool.clone());
    seed_repo_account(&pool, "acct_a", "2026-06-11T00:00:00Z").await;

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

#[tokio::test]
async fn account_repository_should_map_quota_snapshot_to_account_status() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 37).await;
    let repo = SqliteAccountStore::new(pool.clone());
    seed_repo_account(&pool, "acct_a", "2026-06-11T00:00:00Z").await;
    let reset_at = Utc::now() + Duration::minutes(5);

    let limited = repo
        .apply_quota_snapshot(
            "acct_a",
            r#"{"plan_type":"free","monthly_limit":{"limit_reached":true}}"#,
            true,
            Some(reset_at),
        )
        .await
        .unwrap();
    let limited_status: (String, i64, Option<String>) = sqlx::query_as(
        "select status, quota_limit_reached, quota_cooldown_until from accounts where id = ?",
    )
    .bind("acct_a")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(limited);
    assert_eq!(limited_status.0, "quota_exhausted");
    assert_eq!(limited_status.1, 1);
    assert!(limited_status.2.is_some());

    let restored = repo
        .apply_quota_snapshot(
            "acct_a",
            r#"{"plan_type":"free","monthly_limit":{"limit_reached":false}}"#,
            false,
            None,
        )
        .await
        .unwrap();
    let restored_status: (String, i64, Option<String>) = sqlx::query_as(
        "select status, quota_limit_reached, quota_cooldown_until from accounts where id = ?",
    )
    .bind("acct_a")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(restored);
    assert_eq!(restored_status.0, "active");
    assert_eq!(restored_status.1, 0);
    assert!(restored_status.2.is_none());
}

#[tokio::test]
async fn account_repository_runtime_state_sync_should_not_clear_newer_future_quota_cooldown() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 42).await;
    let repo = SqliteAccountStore::new(pool.clone());
    seed_repo_account(&pool, "acct_a", "2026-06-11T00:00:00Z").await;
    let future_cooldown = Utc::now() + Duration::hours(1);
    sqlx::query(
        "update accounts set quota_limit_reached = 1, quota_verify_required = 0, quota_cooldown_until = ? where id = ?",
    )
    .bind(future_cooldown.to_rfc3339())
    .bind("acct_a")
    .execute(&pool)
    .await
    .unwrap();
    let mut stale_account = AccountStore::list_pool_accounts(&repo)
        .await
        .unwrap()
        .into_iter()
        .find(|account| account.id == "acct_a")
        .unwrap();
    stale_account.quota_limit_reached = false;
    stale_account.quota_verify_required = true;
    stale_account.quota_cooldown_until = None;

    repo.sync_runtime_account_state(&stale_account, false)
        .await
        .unwrap();
    let stored: (i64, i64, Option<String>) = sqlx::query_as(
        "select quota_limit_reached, quota_verify_required, quota_cooldown_until from accounts where id = ?",
    )
    .bind("acct_a")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(stored.0, 1);
    assert_eq!(stored.1, 0);
    assert_eq!(stored.2, Some(future_cooldown.to_rfc3339()));
}

#[tokio::test]
async fn account_repository_runtime_state_sync_should_not_regress_newer_usage_window() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 43).await;
    let repo = SqliteAccountStore::new(pool.clone());
    seed_repo_account(&pool, "acct_a", "2026-06-11T00:00:00Z").await;
    let now = Utc::now();
    let future_reset = now + Duration::hours(1);
    let stale_reset = now + Duration::minutes(1);
    sqlx::query(
        r#"
        insert into account_usage (
          account_id,
          window_request_count,
          window_input_tokens,
          window_reset_at,
          limit_window_seconds
        ) values (?, 5, 17, ?, 3600)
        "#,
    )
    .bind("acct_a")
    .bind(future_reset.to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();
    let mut stale_account = AccountStore::list_pool_accounts(&repo)
        .await
        .unwrap()
        .into_iter()
        .find(|account| account.id == "acct_a")
        .unwrap();
    stale_account.window_request_count = 0;
    stale_account.window_input_tokens = 0;
    stale_account.window_reset_at = Some(stale_reset);
    stale_account.window_started_at = Some(now);
    stale_account.limit_window_seconds = Some(60);

    repo.sync_runtime_account_state(&stale_account, true)
        .await
        .unwrap();
    let stored: (i64, i64, Option<String>, Option<i64>) = sqlx::query_as(
        "select window_request_count, window_input_tokens, window_reset_at, limit_window_seconds from account_usage where account_id = ?",
    )
    .bind("acct_a")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(stored.0, 5);
    assert_eq!(stored.1, 17);
    assert_eq!(stored.2, Some(future_reset.to_rfc3339()));
    assert_eq!(stored.3, Some(3600));
}

#[tokio::test]
async fn account_repository_should_not_override_disabled_status_from_quota_snapshot() {
    let (pool, _dir) = sqlite_account_store_parts("accounts.sqlite", 38).await;
    let repo = SqliteAccountStore::new(pool.clone());
    seed_repo_account(&pool, "acct_a", "2026-06-11T00:00:00Z").await;
    repo.set_status("acct_a", AccountStatus::Disabled)
        .await
        .unwrap();

    repo.apply_quota_snapshot(
        "acct_a",
        r#"{"plan_type":"free","monthly_limit":{"limit_reached":true}}"#,
        true,
        Some(Utc::now() + Duration::minutes(5)),
    )
    .await
    .unwrap();
    let status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_a")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status.0, "disabled");
}

async fn sqlite_account_store_parts(
    db_name: &str,
    _key_byte: u8,
) -> (sqlx::SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    (pool, dir)
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

async fn seed_repo_account(pool: &sqlx::SqlitePool, id: &str, added_at: &str) {
    sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at) values (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(format!("access-{id}"))
    .bind("active")
    .bind(added_at)
    .bind(added_at)
    .execute(pool)
    .await
    .unwrap();
}
