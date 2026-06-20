use chrono::Utc;
use codex_proxy_adapters::sqlite::{accounts::SqliteAccountStore, models::ModelSnapshotRepository};
use codex_proxy_core::accounts::{model::AccountStatus, ports::AccountStore};
use codex_proxy_core::models::model::{BackendModelEntry, ModelPlanSnapshot};
use codex_proxy_platform::crypto::SecretBox;
use secrecy::SecretString;

#[tokio::test]
async fn model_snapshot_repository_should_replace_and_load_plan_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("models.sqlite");
    let pool = codex_proxy_platform::storage::connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    let repo = ModelSnapshotRepository::new(pool);
    let snapshot = ModelPlanSnapshot::from_backend_entries(
        "team",
        vec![BackendModelEntry {
            id: Some("gpt-team".to_string()),
            name: Some("GPT Team".to_string()),
            ..BackendModelEntry::default()
        }],
    );

    repo.replace_plan_snapshot(&snapshot).await.unwrap();
    let loaded = repo.list_plan_snapshots().await.unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].plan_type, "team");
    assert_eq!(loaded[0].models[0].id, "gpt-team");
    assert_eq!(loaded[0].models[0].source, "backend");
}

#[tokio::test]
async fn sqlite_account_store_should_list_pool_accounts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("accounts.sqlite");
    let pool = codex_proxy_platform::storage::connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let secret_box = SecretBox::new([41u8; 32]);
    let now = Utc::now().to_rfc3339();
    let access_token_cipher = secret_box
        .encrypt(&SecretString::new("access-token".to_string().into()))
        .expect("encrypt access token");
    let refresh_token_cipher = secret_box
        .encrypt(&SecretString::new("refresh-token".to_string().into()))
        .expect("encrypt refresh token");
    sqlx::query(
        "insert into accounts (
            id, email, chatgpt_account_id, chatgpt_user_id, label, plan_type, access_token_cipher, refresh_token_cipher,
            access_token_expires_at, status, added_at, updated_at
        ) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
        .bind("acct_1")
        .bind("user@example.com")
        .bind("chatgpt-account")
        .bind(Option::<String>::None)
        .bind("primary")
        .bind("plus")
        .bind(access_token_cipher)
        .bind(Some(refresh_token_cipher))
        .bind(Option::<String>::None)
        .bind(match AccountStatus::Active {
            AccountStatus::Active => "active",
            _ => unreachable!(),
        })
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .expect("insert account");

    let store = SqliteAccountStore::new(pool, secret_box);
    let accounts = store.list_pool_accounts().await.expect("list accounts");

    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, "acct_1");
    assert_eq!(accounts[0].access_token, "access-token");
    assert_eq!(accounts[0].plan_type.as_deref(), Some("plus"));
}
