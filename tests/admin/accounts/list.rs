use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use tower::ServiceExt;

use codex_proxy_rs::{
    platform::crypto::SecretBox, platform::storage::db::connect_sqlite, runtime::build_router,
    runtime::state::AppState,
};

use crate::support::{admin_accounts::test_config, response_json, seed_admin_session};

#[tokio::test]
async fn admin_accounts_list_should_not_decrypt_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_corrupt")
    .bind("user@example.com")
    .bind("not-a-secret-box-cipher")
    .bind("active")
    .bind(&now)
    .bind(&now)
    .execute(&pool)
    .await
    .unwrap();
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool,
        SecretBox::new([13u8; 32]),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"][0]["id"], "acct_corrupt");
}
