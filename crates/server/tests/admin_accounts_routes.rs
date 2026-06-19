use std::{collections::BTreeMap, fs, sync::Arc};

use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{Duration, Utc};
use codex_proxy_adapters::sqlite::accounts::{NewAccount, SqliteAccountStore};
use codex_proxy_core::{
    accounts::{model::AccountStatus, ports::AccountStore},
    auth::{
        oauth::{DeviceCode, OAuthError, RefreshFailure, TokenPair},
        ports::{OAuthClient, TokenRefresher},
    },
};
use codex_proxy_platform::{
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig, WebSocketPoolConfig,
    },
    crypto::SecretBox,
    identity::ApiKeyHasher,
    storage::connect_sqlite,
};
use codex_proxy_runtime::state::AppState;
use codex_proxy_server::router;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn admin_accounts_list_should_not_decrypt_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    sqlx::query(
        "insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_corrupt")
    .bind("user@example.com")
    .bind("not-a-secret-box-cipher")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([63u8; 32]),
        ApiKeyHasher::new([64u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["requestId"], "req_accounts_list");
    assert_eq!(body["data"][0]["id"], "acct_corrupt");
    assert_eq!(body["data"][0]["email"], "user@example.com");
}

#[tokio::test]
async fn admin_usage_stats_should_return_page_and_summary() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    sqlx::query(
        "insert into accounts (id, email, label, plan_type, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_usage")
    .bind("usage@example.com")
    .bind("primary")
    .bind("plus")
    .bind("cipher")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_usage (account_id, request_count, empty_response_count, input_tokens, output_tokens, cached_tokens, image_input_tokens, image_output_tokens, image_request_count, image_request_failed_count, last_used_at) values (?, 3, 1, 21, 8, 5, 7, 2, 1, 0, ?)",
    )
    .bind("acct_usage")
    .bind("2026-06-18T00:10:00Z")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([73u8; 32]),
        ApiKeyHasher::new([74u8; 32]),
    );
    let app = router::router().with_state(state);

    let page_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage-stats?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_page")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let page_status = page_response.status();
    let page_body = response_json(page_response).await;

    assert_eq!(page_status, StatusCode::OK);
    assert_eq!(page_body["data"][0]["accountId"], "acct_usage");
    assert_eq!(page_body["data"][0]["email"], "usage@example.com");
    assert_eq!(page_body["data"][0]["requestCount"], 3);
    assert_eq!(page_body["data"][0]["inputTokens"], 21);

    let summary_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage-stats/summary")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let summary_status = summary_response.status();
    let summary_body = response_json(summary_response).await;

    assert_eq!(summary_status, StatusCode::OK);
    assert_eq!(summary_body["data"]["accountCount"], 1);
    assert_eq!(summary_body["data"]["requestCount"], 3);
    assert_eq!(summary_body["data"]["outputTokens"], 8);
}

#[tokio::test]
async fn admin_usage_stats_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([125u8; 32]),
        ApiKeyHasher::new([126u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage-stats")
                .header("x-request-id", "req_usage_auth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_usage_auth");
}

#[tokio::test]
async fn admin_usage_stats_should_cursor_page_account_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage-cursor.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    seed_usage_account(
        &pool,
        "acct_a",
        "a@example.com",
        "primary",
        "plus",
        3,
        0,
        12,
        5,
        1,
        "2026-06-11T00:00:00Z",
    )
    .await;
    seed_usage_account(
        &pool,
        "acct_b",
        "b@example.com",
        "backup",
        "free",
        2,
        1,
        7,
        3,
        2,
        "2026-06-11T00:01:00Z",
    )
    .await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([127u8; 32]),
        ApiKeyHasher::new([128u8; 32]),
    );
    let app = router::router().with_state(state);

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage-stats?limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_cursor")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let first_status = first_response.status();
    let first_body = response_json(first_response).await;

    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(first_body["code"], 200);
    assert_eq!(first_body["requestId"], "req_usage_cursor");
    assert_eq!(first_body["data"].as_array().unwrap().len(), 1);
    assert_eq!(first_body["data"][0]["accountId"], "acct_b");
    assert_eq!(first_body["data"][0]["requestCount"], 2);
    assert_eq!(first_body["data"][0]["emptyResponseCount"], 1);
    assert_eq!(first_body["data"][0]["inputTokens"], 7);
    assert_eq!(first_body["page"]["limit"], 1);
    let cursor = first_body["page"]["nextCursor"].as_str().unwrap();

    let second_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/usage-stats?limit=1&cursor={cursor}"))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let second_status = second_response.status();
    let second_body = response_json(second_response).await;

    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(second_body["data"][0]["accountId"], "acct_a");
    assert!(second_body["page"]["nextCursor"].is_null());
}

async fn seed_admin_session(pool: &SqlitePool, session_id: &str) {
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
    )
    .bind("admin_1")
    .bind("hash")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind("admin_1")
    .bind("2999-01-01T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

#[expect(
    clippy::too_many_arguments,
    reason = "test fixture keeps usage rows explicit"
)]
async fn seed_usage_account(
    pool: &SqlitePool,
    id: &str,
    email: &str,
    label: &str,
    plan_type: &str,
    request_count: i64,
    empty_response_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    last_used_at: &str,
) {
    sqlx::query(
        "insert into accounts (id, email, label, plan_type, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, 'active', ?, ?)",
    )
    .bind(id)
    .bind(email)
    .bind(label)
    .bind(plan_type)
    .bind("encrypted")
    .bind("2026-06-11T00:00:00Z")
    .bind("2026-06-11T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_usage (account_id, request_count, empty_response_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(request_count)
    .bind(empty_response_count)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cached_tokens)
    .bind(last_used_at)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn admin_accounts_lifecycle_should_update_and_delete_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-lifecycle.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    sqlx::query(
        "insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_lifecycle")
    .bind("life@example.com")
    .bind("cipher")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([75u8; 32]),
        ApiKeyHasher::new([76u8; 32]),
    );
    let app = router::router().with_state(state);

    let label = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/accounts/acct_lifecycle/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"label": "primary"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let label_status = label.status();
    let label_body = response_json(label).await;

    assert_eq!(label_status, StatusCode::OK);
    assert_eq!(label_body["data"]["label"], "primary");

    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/accounts/acct_lifecycle/status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"status": "disabled"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status_code = status.status();
    let status_body = response_json(status).await;

    assert_eq!(status_code, StatusCode::OK);
    assert_eq!(status_body["data"]["status"], "disabled");

    let deleted = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/accounts/acct_lifecycle")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let delete_status = deleted.status();
    let delete_body = response_json(deleted).await;

    assert_eq!(delete_status, StatusCode::OK);
    assert_eq!(delete_body["data"]["deleted"], true);
}

#[tokio::test]
async fn admin_accounts_export_should_return_native_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-export.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([77u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_export".to_string(),
            email: Some("export@example.com".to_string()),
            account_id: Some("chatgpt_export".to_string()),
            user_id: Some("user_export".to_string()),
            label: Some("primary".to_string()),
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-export".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-export".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([78u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/export?ids=acct_export")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["requestId"], "req_accounts_export");
    assert_eq!(body["data"]["sourceFormat"], "native");
    assert_eq!(body["data"]["accounts"][0]["id"], "acct_export");
    assert_eq!(body["data"]["accounts"][0]["token"], "access-export");
    assert_eq!(
        body["data"]["accounts"][0]["refreshToken"],
        "refresh-export"
    );
}

#[tokio::test]
async fn admin_accounts_import_should_store_native_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-import.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([79u8; 32]);
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([80u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_import",
                            "email": "import@example.com",
                            "accountId": "chatgpt_import",
                            "userId": "user_import",
                            "label": "secondary",
                            "planType": "team",
                            "token": "access-import",
                            "refreshToken": "refresh-import",
                            "accessTokenExpiresAt": "2026-06-18T02:00:00Z",
                            "status": "disabled"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = SqliteAccountStore::new(pool, secret_box)
        .get("acct_import")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requestId"], "req_accounts_import");
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    assert_eq!(body["data"]["sourceFormat"], "native");
    assert_eq!(stored.access_token.expose_secret(), "access-import");
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "refresh-import"
    );
    assert_eq!(stored.status, AccountStatus::Disabled);
}

#[tokio::test]
async fn admin_accounts_import_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-import-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([111u8; 32]),
        ApiKeyHasher::new([112u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("x-request-id", "req_accounts_import_auth")
                .body(Body::from(r#"{"accounts":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_accounts_import_auth");
}

#[tokio::test]
async fn admin_accounts_import_should_store_tokens_encrypted_and_list_sanitized_accounts() {
    let (app, _state, pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-import-sanitized.sqlite", 113).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_sanitized")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_imported_sanitized",
                            "email": "user@example.com",
                            "accountId": "chatgpt-account",
                            "userId": "chatgpt-user",
                            "label": "primary",
                            "planType": "plus",
                            "token": "access-secret",
                            "refreshToken": "refresh-secret",
                            "status": "active"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);

    let stored: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_imported_sanitized")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.0.starts_with("v1:"));
    assert!(!stored.0.contains("access-secret"));
    assert!(stored.1.starts_with("v1:"));
    assert!(!stored.1.contains("refresh-secret"));

    let list_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_status = list_response.status();
    let list_body = response_json(list_response).await;

    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_body["data"][0]["id"], "acct_imported_sanitized");
    assert_eq!(list_body["data"][0]["email"], "user@example.com");
    assert!(list_body["data"][0].get("token").is_none());
    assert!(list_body["data"][0].get("refreshToken").is_none());
    assert_eq!(list_body["page"]["limit"], 10);
}

#[tokio::test]
async fn admin_accounts_import_should_reject_non_native_export_shape() {
    let (app, _state, pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-import-external-shape.sqlite", 114).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_external_import")
                .body(Body::from(
                    json!({
                        "type": "external-data",
                        "version": 1,
                        "legacy": [],
                        "accounts": [{
                            "platform": "openai",
                            "type": "oauth",
                            "credentials": {
                                "access_token": "Bearer external-access-secret",
                                "refresh_token": "rt_external",
                                "email": "team@example.com",
                                "chatgpt_account_id": "chatgpt-account",
                                "chatgpt_user_id": "chatgpt-user",
                                "plan_type": "team"
                            }
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "No importable accounts found");
    assert_eq!(stored.0, 0);
}

#[tokio::test]
async fn admin_accounts_import_should_reject_native_payload_with_unknown_account_fields() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-import-unknown-account.sqlite", 115).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_native_unknown_account")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_native_unknown",
                            "token": "native-access-secret",
                            "refreshToken": "native-refresh-secret",
                            "email": "native@example.com",
                            "accountId": "native-account",
                            "userId": "native-user",
                            "label": "Native Unknown",
                            "planType": "plus",
                            "legacyField": "ignored-secret",
                            "status": "active"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "No importable accounts found");
}

#[tokio::test]
async fn admin_accounts_import_should_reject_native_payload_with_unknown_container_fields() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-import-unknown-container.sqlite", 116).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_native_unknown_container")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_native_extra_container",
                            "token": "native-access-secret",
                            "refreshToken": "native-refresh-secret",
                            "email": "native@example.com",
                            "accountId": "native-account",
                            "userId": "native-user",
                            "label": "Native",
                            "planType": "plus",
                            "status": "active"
                        }],
                        "legacyContainer": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "No importable accounts found");
}

#[tokio::test]
async fn admin_accounts_export_should_return_native_accounts_with_tokens_and_filter_ids() {
    let (app, _state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-accounts-export-filter.sqlite", 117).await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_export_a".to_string(),
            email: Some("export-a@example.com".to_string()),
            account_id: Some("chatgpt-export-a".to_string()),
            user_id: Some("user-export-a".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-acct_export_a".to_string().into()),
            refresh_token: Some(SecretString::new(
                "refresh-acct_export_a".to_string().into(),
            )),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box,
        NewAccount {
            id: "acct_export_b".to_string(),
            email: Some("export-b@example.com".to_string()),
            account_id: Some("chatgpt-export-b".to_string()),
            user_id: Some("user-export-b".to_string()),
            label: None,
            plan_type: Some("team".to_string()),
            access_token: SecretString::new("access-acct_export_b".to_string().into()),
            refresh_token: Some(SecretString::new(
                "refresh-acct_export_b".to_string().into(),
            )),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/export?ids=acct_export_a")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["sourceFormat"], "native");
    assert_eq!(body["data"]["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["accounts"][0]["id"], "acct_export_a");
    assert_eq!(body["data"]["accounts"][0]["token"], "access-acct_export_a");
    assert_eq!(
        body["data"]["accounts"][0]["refreshToken"],
        "refresh-acct_export_a"
    );
    assert!(body["data"]["accounts"][0].get("legacyField").is_none());
}

#[tokio::test]
async fn admin_accounts_export_should_reject_unsupported_external_format() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-export-external.sqlite", 118).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/export?format=external")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_export_should_reject_unsupported_full_format() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-export-full.sqlite", 119).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/export?format=full")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_account_cookies_should_store_encrypted_cookie_header() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([85u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_cookies".to_string(),
            email: Some("cookies@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-cookies".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box,
        ApiKeyHasher::new([86u8; 32]),
    );
    let app = router::router().with_state(state);

    let set = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_cookies/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"cookies":"cf_clearance=clear-secret; __cf_bm=bm-secret"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let set_status = set.status();
    let set_body = response_json(set).await;

    assert_eq!(set_status, StatusCode::OK);
    assert_eq!(
        set_body["data"]["cookies"],
        "__cf_bm=bm-secret; cf_clearance=clear-secret"
    );
    let stored = sqlx::query_as::<_, (String, String)>(
        "select name, value_cipher from account_cookies where account_id = ? order by name asc",
    )
    .bind("acct_cookies")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().all(|(_, cipher)| cipher.starts_with("v1:")));
    assert!(stored
        .iter()
        .all(|(_, cipher)| !cipher.contains("clear-secret") && !cipher.contains("bm-secret")));

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let get_status = get.status();
    let get_body = response_json(get).await;

    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(
        get_body["data"]["cookies"],
        "__cf_bm=bm-secret; cf_clearance=clear-secret"
    );

    let delete = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let delete_status = delete.status();
    let delete_body = response_json(delete).await;

    assert_eq!(delete_status, StatusCode::OK);
    assert_eq!(delete_body["data"]["deleted"], true);
}

#[tokio::test]
async fn admin_account_cookies_should_reject_missing_account_and_empty_payload() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-cookies-invalid.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([87u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_cookie_invalid".to_string(),
            email: Some("cookie-invalid@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-cookie-invalid".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([88u8; 32]),
    );
    let app = router::router().with_state(state);

    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/missing/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_cookie_invalid/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"cookies": ""}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_auth_login_start_should_return_pkce_auth_url_and_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-login-start.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([95u8; 32]),
        ApiKeyHasher::new([96u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login-start")
                .header("cookie", "cpr_admin_session=session_1")
                .header("host", "127.0.0.1:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let state = body["data"]["state"].as_str().unwrap();
    let auth_url = body["data"]["authUrl"].as_str().unwrap();
    assert_eq!(state.len(), 32);
    assert!(auth_url.starts_with("https://auth.openai.com/oauth/authorize?"));
    assert!(auth_url.contains("response_type=code"));
    assert!(auth_url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
    assert!(auth_url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
    assert!(auth_url.contains("scope=openid%20profile%20email%20offline_access"));
    assert!(auth_url.contains("code_challenge_method=S256"));
    assert!(auth_url.contains("originator=codex_cli_rs"));
    assert!(auth_url.contains(&format!("state={state}")));
}

#[tokio::test]
async fn admin_auth_status_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-status-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([105u8; 32]),
        ApiKeyHasher::new([106u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/auth/status")
                .header("x-request-id", "req_auth_status_no_session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_auth_status_no_session");
}

#[tokio::test]
async fn admin_auth_status_should_report_user_and_pool_summary_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-status-summary.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([107u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_auth_active".to_string(),
            email: Some("active-auth@example.com".to_string()),
            account_id: Some("auth-active-account".to_string()),
            user_id: Some("auth-active-user".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-auth-active".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-auth-active".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_auth_disabled".to_string(),
            email: Some("disabled-auth@example.com".to_string()),
            account_id: Some("auth-disabled-account".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-auth-disabled".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Disabled,
        },
    )
    .await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([108u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/auth/status")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_auth_status_summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["authenticated"], true);
    assert_eq!(body["data"]["pool"]["total"], 2);
    assert_eq!(body["data"]["pool"]["active"], 1);
    assert_eq!(body["data"]["pool"]["disabled"], 1);
    assert_eq!(body["data"]["user"]["email"], "active-auth@example.com");
    assert!(body["data"]["user"].get("accessToken").is_none());
    assert!(body["data"]["user"].get("token").is_none());
    assert!(body["data"]["user"].get("refreshToken").is_none());
}

#[tokio::test]
async fn admin_auth_logout_should_clear_accounts_usage_cookies_and_runtime_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-logout.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([109u8; 32]);
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([110u8; 32]),
    );
    let app = router::router().with_state(state.clone());
    let imported = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_auth_logout",
                            "email": "logout@example.com",
                            "planType": "plus",
                            "token": "access-auth-logout",
                            "refreshToken": "refresh-auth-logout",
                            "status": "active"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens) values (?, 2, 3, 4, 1)",
    )
    .bind("acct_auth_logout")
    .execute(&pool)
    .await
    .unwrap();
    let set_cookies = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_auth_logout/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"cookies":"cf_clearance=secret"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(set_cookies.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/logout")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_auth_logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["success"], true);
    assert_eq!(body["data"]["deleted"], 1);
    let account_count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    let usage_count: (i64,) = sqlx::query_as("select count(*) from account_usage")
        .fetch_one(&pool)
        .await
        .unwrap();
    let cookie_count: (i64,) = sqlx::query_as("select count(*) from account_cookies")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(account_count.0, 0);
    assert_eq!(usage_count.0, 0);
    assert_eq!(cookie_count.0, 0);
    assert!(state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .is_none());
}

#[tokio::test]
async fn admin_auth_device_login_should_return_openai_device_code() {
    let client = StaticOAuthClient {
        device_response: Ok(DeviceCode {
            device_code: "device-secret".to_string(),
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: "https://auth.openai.com/activate".to_string(),
            verification_uri_complete: "https://auth.openai.com/activate?user_code=ABCD-EFGH"
                .to_string(),
            expires_in: 900,
            interval: 5,
        }),
        poll_response: Err(OAuthError::AuthorizationPending),
        exchange_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_calls: Arc::new(Mutex::new(Vec::new())),
        exchange_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-device-login.sqlite", 111, client)
            .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/device-login")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_device_login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["userCode"], "ABCD-EFGH");
    assert_eq!(
        body["data"]["verificationUri"],
        "https://auth.openai.com/activate"
    );
    assert_eq!(body["data"]["deviceCode"], "device-secret");
    assert_eq!(body["data"]["expiresIn"], 900);
    assert_eq!(body["data"]["interval"], 5);
}

#[tokio::test]
async fn admin_auth_device_poll_should_return_pending_without_importing_account() {
    let poll_calls = Arc::new(Mutex::new(Vec::new()));
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Err(OAuthError::SlowDown),
        exchange_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_calls: poll_calls.clone(),
        exchange_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-device-pending.sqlite", 112, client)
            .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/auth/device-poll/device-secret")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_device_pending")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["pending"], true);
    assert_eq!(body["data"]["success"], false);
    assert_eq!(body["data"]["code"], "slow_down");
    assert_eq!(*poll_calls.lock().await, vec!["device-secret".to_string()]);
    let account_count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(account_count.0, 0);
}

#[tokio::test]
async fn admin_auth_device_poll_should_import_tokens_without_returning_secrets() {
    let token = test_jwt(
        "device-account",
        Some("device-user"),
        Some("device@example.com"),
        Some("plus"),
    );
    let poll_calls = Arc::new(Mutex::new(Vec::new()));
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Ok(TokenPair {
            access_token: token.clone(),
            refresh_token: Some("device-refresh-secret".to_string()),
        }),
        exchange_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_calls: poll_calls.clone(),
        exchange_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, state, pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-device-success.sqlite", 113, client)
            .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/auth/device-poll/device-success")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_device_success")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["success"], true);
    assert_eq!(body["data"]["pending"], false);
    assert!(body["data"].get("accessToken").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    assert_eq!(*poll_calls.lock().await, vec!["device-success".to_string()]);
    let stored = SqliteAccountStore::new(pool.clone(), SecretBox::new([113u8; 32]))
        .get(
            state
                .services
                .account_pool
                .acquire("gpt-5.5", Utc::now())
                .await
                .unwrap()
                .account
                .id
                .as_str(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.email.as_deref(), Some("device@example.com"));
    assert_eq!(stored.account_id.as_deref(), Some("device-account"));
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "device-refresh-secret"
    );
}

#[tokio::test]
async fn admin_auth_login_start_should_use_configured_oauth_authorize_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("admin-auth-login-start-configured-oauth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let mut config = test_config(url);
    config.auth.oauth_client_id = "app_configured_client".to_string();
    config.auth.oauth_auth_endpoint = "https://auth.example.test/oauth/authorize".to_string();
    config.auth.oauth_token_endpoint = "https://auth.example.test/oauth/token".to_string();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool,
        SecretBox::new([114u8; 32]),
        ApiKeyHasher::new([115u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login-start")
                .header("cookie", "cpr_admin_session=session_1")
                .header("host", "127.0.0.1:8080")
                .header("x-request-id", "req_login_start_configured_oauth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let auth_url = body["data"]["authUrl"].as_str().unwrap();

    assert!(auth_url.starts_with("https://auth.example.test/oauth/authorize?"));
    assert!(auth_url.contains("client_id=app_configured_client"));
}

#[tokio::test]
async fn admin_auth_code_relay_should_exchange_code_and_import_account() {
    let token = test_jwt(
        "pkce-account",
        Some("pkce-user"),
        Some("pkce@example.com"),
        Some("plus"),
    );
    let exchange_calls = Arc::new(Mutex::new(Vec::new()));
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Err(OAuthError::AuthorizationPending),
        exchange_response: Ok(TokenPair {
            access_token: token,
            refresh_token: Some("pkce-refresh-secret".to_string()),
        }),
        poll_calls: Arc::new(Mutex::new(Vec::new())),
        exchange_calls: exchange_calls.clone(),
    };
    let (app, state, _pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-code-relay.sqlite", 116, client)
            .await;
    let start = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login-start")
                .header("cookie", "cpr_admin_session=session_1")
                .header("host", "127.0.0.1:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let start_body = response_json(start).await;
    let state_value = start_body["data"]["state"].as_str().unwrap();
    let callback_url =
        format!("http://localhost:1455/auth/callback?code=oauth-code&state={state_value}");

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/code-relay")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_code_relay")
                .body(Body::from(json!({"callbackUrl": callback_url}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["success"], true);
    assert!(body["data"].get("accessToken").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    let calls = exchange_calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].code, "oauth-code");
    assert!(!calls[0].code_verifier.is_empty());
    assert_eq!(calls[0].redirect_uri, "http://localhost:1455/auth/callback");
    drop(calls);
    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.account_id.as_deref(), Some("pkce-account"));
}

#[tokio::test]
async fn admin_auth_code_relay_should_reject_invalid_callback_url() {
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Err(OAuthError::AuthorizationPending),
        exchange_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_calls: Arc::new(Mutex::new(Vec::new())),
        exchange_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, _pool, _dir) = admin_accounts_test_app_with_oauth_client(
        "admin-auth-code-relay-invalid.sqlite",
        117,
        client,
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/code-relay")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_code_relay_invalid")
                .body(Body::from(json!({"callbackUrl": "not a url"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_auth_callback_should_exchange_code_and_redirect_to_return_host() {
    let token = test_jwt(
        "callback-account",
        Some("callback-user"),
        Some("callback@example.com"),
        Some("plus"),
    );
    let exchange_calls = Arc::new(Mutex::new(Vec::new()));
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Err(OAuthError::AuthorizationPending),
        exchange_response: Ok(TokenPair {
            access_token: token,
            refresh_token: Some("callback-refresh-secret".to_string()),
        }),
        poll_calls: Arc::new(Mutex::new(Vec::new())),
        exchange_calls: exchange_calls.clone(),
    };
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-callback.sqlite", 118, client).await;
    let start = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login-start")
                .header("cookie", "cpr_admin_session=session_1")
                .header("host", "codex.local:1455")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let start_body = response_json(start).await;
    let state_value = start_body["data"]["state"].as_str().unwrap();
    let callback_path = format!("/auth/callback?code=callback-code&state={state_value}");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(callback_path)
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers().get("location").unwrap(),
        "http://codex.local:1455/"
    );
    assert_eq!(exchange_calls.lock().await[0].code, "callback-code");
    let count: (i64,) = sqlx::query_as("select count(*) from accounts where account_id = ?")
        .bind("callback-account")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
}

#[tokio::test]
async fn admin_account_health_check_should_skip_account_without_refresh_token() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 1
                }
            }
        })))
        .expect(0)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-health-no-refresh.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([93u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_health".to_string(),
            email: Some("health@example.com".to_string()),
            account_id: Some("chatgpt-health".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-health".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool,
        secret_box,
        ApiKeyHasher::new([94u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/health-check")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_health"],
                        "concurrency": 1,
                        "staggerMs": 500
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["summary"]["total"], 1);
    assert_eq!(body["data"]["summary"]["alive"], 0);
    assert_eq!(body["data"]["summary"]["dead"], 0);
    assert_eq!(body["data"]["summary"]["skipped"], 1);
    assert_eq!(body["data"]["results"][0]["id"], "acct_health");
    assert_eq!(body["data"]["results"][0]["result"], "skipped");
    assert_eq!(body["data"]["results"][0]["status"], "active");
    assert_eq!(body["data"]["results"][0]["error"], "no refresh token");
}

#[tokio::test]
async fn admin_accounts_health_check_should_refresh_oauth_without_touching_codex_backend() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-health-refresh.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([124u8; 32]);
    for (id, refresh_token) in [
        ("acct_health_alive", "refresh-acct_health_alive"),
        ("acct_health_dead", "refresh-acct_health_dead"),
    ] {
        seed_encrypted_account(
            &pool,
            secret_box.clone(),
            NewAccount {
                id: id.to_string(),
                email: Some(format!("{id}@example.com")),
                account_id: Some(format!("old-{id}")),
                user_id: None,
                label: None,
                plan_type: None,
                access_token: SecretString::new(format!("old-access-{id}").into()),
                refresh_token: Some(SecretString::new(refresh_token.to_string().into())),
                access_token_expires_at: None,
                status: AccountStatus::Active,
            },
        )
        .await;
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        config,
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([124u8; 32]),
        HealthCheckTokenRefresher {
            calls: calls.clone(),
        },
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/health-check")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_health_refresh")
                .body(Body::from(
                    json!({
                        "ids": ["acct_health_alive", "acct_health_dead"],
                        "concurrency": 2,
                        "staggerMs": 500
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["summary"]["total"], 2);
    assert_eq!(body["data"]["summary"]["alive"], 1);
    assert_eq!(body["data"]["summary"]["dead"], 1);
    assert_eq!(body["data"]["summary"]["skipped"], 0);
    let mut refresh_calls = calls.lock().await.clone();
    refresh_calls.sort();
    assert_eq!(
        refresh_calls,
        vec![
            "refresh-acct_health_alive".to_string(),
            "refresh-acct_health_dead".to_string(),
        ]
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("new-health-access"));
    assert!(!serialized.contains("new-health-refresh"));
    let dead_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_health_dead")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(dead_status.0, "expired");
}

#[tokio::test]
async fn admin_accounts_health_check_should_reject_unsupported_stagger_ms_field() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-account-health-unsupported-field.sqlite", 125).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/health-check")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"stagger_ms":1000}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40001);
}

#[tokio::test]
async fn admin_account_quota_should_fetch_usage_store_quota_and_not_return_secrets() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-quota"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 25,
                    "reset_at": 1770000400,
                    "limit_window_seconds": 3600
                }
            }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([95u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quota".to_string(),
            email: Some("quota@example.com".to_string()),
            account_id: Some("chatgpt-quota".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quota".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-quota".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool.clone(),
        secret_box,
        ApiKeyHasher::new([96u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_quota/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();

    assert_eq!(status, StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["quota"]["plan_type"], "plus");
    assert_eq!(body["data"]["quota"]["rate_limit"]["remaining_percent"], 75);
    assert_eq!(
        body["data"]["raw"]["rate_limit"]["primary_window"]["used_percent"],
        25
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("access-quota"));
    assert!(!serialized.contains("refresh-quota"));

    let stored: (String,) = sqlx::query_as("select quota_json from accounts where id = ?")
        .bind("acct_quota")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stored.0.contains("\"remaining_percent\":75"));
    assert!(!stored.0.contains("access-quota"));
}

#[tokio::test]
async fn admin_account_quota_should_return_bad_gateway_when_usage_fetch_fails() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-quota-fail"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {
                "message": "quota unavailable"
            }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-fail.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([97u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quota_fail".to_string(),
            email: Some("quota-fail@example.com".to_string()),
            account_id: Some("chatgpt-quota-fail".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quota-fail".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool,
        secret_box,
        ApiKeyHasher::new([98u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_quota_fail/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota_fail")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], 50201);
    assert_eq!(body["message"], "Failed to fetch quota from Codex API");
    assert!(body["data"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("quota unavailable")));
    assert_eq!(body["requestId"], "req_quota_fail");
}

#[tokio::test]
async fn admin_account_quota_should_reject_inactive_account_without_calling_upstream() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-inactive.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([99u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quota_inactive".to_string(),
            email: Some("quota-inactive@example.com".to_string()),
            account_id: Some("chatgpt-quota-inactive".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quota-inactive".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Disabled,
        },
    )
    .await;
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool.clone(),
        secret_box,
        ApiKeyHasher::new([100u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_quota_inactive/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota_inactive")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let quota_json: (Option<String>,) =
        sqlx::query_as("select quota_json from accounts where id = ?")
            .bind("acct_quota_inactive")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], 40901);
    assert_eq!(body["message"], "Account is disabled, cannot query quota");
    assert_eq!(body["requestId"], "req_quota_inactive");
    assert!(quota_json.0.is_none());
}

#[tokio::test]
async fn admin_account_quota_should_return_store_error_when_quota_persistence_fails() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-quota-store-fail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 40,
                    "reset_at": 1770000500,
                    "limit_window_seconds": 3600
                }
            }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-store-fail.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([101u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quota_store_fail".to_string(),
            email: Some("quota-store-fail@example.com".to_string()),
            account_id: Some("chatgpt-quota-store-fail".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quota-store-fail".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    sqlx::query(
        "create trigger quota_write_denied before update of quota_json on accounts begin select raise(abort, 'quota write denied'); end",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool.clone(),
        secret_box,
        ApiKeyHasher::new([102u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_quota_store_fail/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota_store_fail")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let quota_json: (Option<String>,) =
        sqlx::query_as("select quota_json from accounts where id = ?")
            .bind("acct_quota_store_fail")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], 50001);
    assert_eq!(body["message"], "Failed to store account quota");
    assert_eq!(body["requestId"], "req_quota_store_fail");
    assert!(quota_json.0.is_none());
}

#[tokio::test]
async fn admin_accounts_batch_status_should_update_found_accounts_and_report_invalid_requests() {
    let (app, state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-accounts-batch-status-route.sqlite", 120).await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_batch_status_a".to_string(),
            email: Some("batch-a@example.com".to_string()),
            account_id: Some("batch-a".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-batch-a".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box,
        NewAccount {
            id: "acct_batch_status_b".to_string(),
            email: Some("batch-b@example.com".to_string()),
            account_id: Some("batch-b".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-batch-b".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let restored = state.restore_account_pool_from_repository().await.unwrap();
    assert_eq!(restored, 2);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_status_a", "ghost"],
                        "status": "disabled"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["updated"], 1);
    assert_eq!(body["data"]["notFound"], json!(["ghost"]));
    let statuses =
        sqlx::query_as::<_, (String, String)>("select id, status from accounts order by id asc")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec![
            ("acct_batch_status_a".to_string(), "disabled".to_string()),
            ("acct_batch_status_b".to_string(), "active".to_string())
        ]
    );
    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_status_a"],
                        "status": "expired"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"ids":[],"status":"active"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_account_refresh_should_update_tokens_and_runtime_pool_without_returning_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-refresh-route.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([121u8; 32]);
    let refreshed_access_token = test_jwt(
        "refresh-account",
        Some("refresh-user"),
        Some("refresh@example.com"),
        Some("plus"),
    );
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_refresh_route".to_string(),
            email: Some("old-refresh@example.com".to_string()),
            account_id: Some("old-refresh-account".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("old-access-token".to_string().into()),
            refresh_token: Some(SecretString::new("old-refresh-token".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([122u8; 32]),
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: refreshed_access_token.clone(),
                refresh_token: Some("new-admin-refresh-rt".to_string()),
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    );
    let app = router::router().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_refresh_route/refresh")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_refresh_account")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["id"], "acct_refresh_route");
    assert_eq!(body["data"]["result"], "alive");
    assert_eq!(body["data"]["previousStatus"], "active");
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains(&refreshed_access_token));
    assert!(!serialized.contains("new-admin-refresh-rt"));

    let stored = SqliteAccountStore::new(pool.clone(), secret_box)
        .get("acct_refresh_route")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), refreshed_access_token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "new-admin-refresh-rt"
    );
    let raw: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_refresh_route")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(raw.0.starts_with("v1:"));
    assert!(!raw.0.contains("old-access-token"));
    assert!(!raw.0.contains(&refreshed_access_token));
    assert!(raw.1.starts_with("v1:"));
    assert!(!raw.1.contains("new-admin-refresh-rt"));
    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.access_token, refreshed_access_token);
}

#[tokio::test]
async fn admin_account_refresh_should_mark_invalid_refresh_token_as_expired() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("admin-account-refresh-invalid-route.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([123u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box,
        NewAccount {
            id: "acct_refresh_invalid_route".to_string(),
            email: Some("refresh-invalid@example.com".to_string()),
            account_id: Some("refresh-invalid-account".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("invalid-old-access".to_string().into()),
            refresh_token: Some(SecretString::new(
                "invalid-refresh-token".to_string().into(),
            )),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        SecretBox::new([123u8; 32]),
        ApiKeyHasher::new([124u8; 32]),
        StaticTokenRefresher {
            result: Err(RefreshFailure::InvalidGrant),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    );
    let restored = state.restore_account_pool_from_repository().await.unwrap();
    assert_eq!(restored, 1);
    let app = router::router().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_refresh_invalid_route/refresh")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_refresh_invalid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["result"], "dead");
    assert_eq!(body["data"]["status"], "expired");
    let stored_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_refresh_invalid_route")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored_status.0, "expired");
    assert!(state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .is_none());
}

#[tokio::test]
async fn admin_account_reset_usage_should_clear_local_counters_and_pool_last_used() {
    let (app, state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-account-reset-usage-route.sqlite", 125).await;
    let window_reset_at = Utc::now() + Duration::minutes(5);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_reset_usage_route".to_string(),
            email: Some("reset-usage@example.com".to_string()),
            account_id: Some("reset-usage-account".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-reset-usage".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens, window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens, window_started_at, window_reset_at, limit_window_seconds, last_used_at) values (?, 7, 11, 13, 17, 5, 19, 23, 29, ?, ?, 300, ?)",
    )
    .bind("acct_reset_usage_route")
    .bind("2026-06-12T12:30:00Z")
    .bind(window_reset_at.to_rfc3339())
    .bind("2026-06-12T12:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    state.restore_account_pool_from_repository().await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_reset_usage_route/reset-usage")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_reset_usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["id"], "acct_reset_usage_route");
    assert_eq!(body["data"]["reset"], true);
    type ResetUsageRow = (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
    );
    let usage: ResetUsageRow = sqlx::query_as(
        "select request_count, input_tokens, output_tokens, cached_tokens, window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens, window_reset_at, last_used_at from account_usage where account_id = ?",
    )
    .bind("acct_reset_usage_route")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        usage,
        (
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            Some(window_reset_at.to_rfc3339()),
            None
        )
    );
    let pool_account = SqliteAccountStore::new(pool, secret_box)
        .get_pool_account("acct_reset_usage_route")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pool_account.request_count, 0);
    assert_eq!(pool_account.window_request_count, 0);
    assert_eq!(pool_account.window_reset_at, Some(window_reset_at));
    assert!(pool_account.last_used_at.is_none());
}

#[tokio::test]
async fn admin_account_create_should_derive_claims_and_store_encrypted_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-create.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([91u8; 32]);
    let token = test_jwt(
        "jwt-account",
        Some("jwt-user"),
        Some("jwt@example.com"),
        Some("team"),
    );
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([92u8; 32]),
    );
    let app = router::router().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "id": "caller-id",
                        "email": "caller@example.com",
                        "accountId": "caller-account",
                        "userId": "caller-user",
                        "label": "Caller Label",
                        "planType": "caller-plan",
                        "token": format!("Bearer {token}"),
                        "refreshToken": "manual-refresh-secret",
                        "status": "disabled"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    let created_id = body["data"]["id"].as_str().unwrap();
    assert_ne!(created_id, "caller-id");
    assert_eq!(body["data"]["email"], "jwt@example.com");
    assert_eq!(body["data"]["accountId"], "jwt-account");
    assert_eq!(body["data"]["userId"], "jwt-user");
    assert_eq!(body["data"]["planType"], "team");
    assert!(body["data"]["label"].is_null());
    assert_eq!(body["data"]["status"], "active");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());

    let stored = SqliteAccountStore::new(pool.clone(), secret_box)
        .get(created_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "manual-refresh-secret"
    );

    let raw_cipher: (String,) =
        sqlx::query_as("select access_token_cipher from accounts where id = ?")
            .bind(created_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!raw_cipher.0.contains(&token));
    let raw_refresh_cipher: (String,) =
        sqlx::query_as("select refresh_token_cipher from accounts where id = ?")
            .bind(created_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(raw_cipher.0.starts_with("v1:"));
    assert!(raw_refresh_cipher.0.starts_with("v1:"));
    assert!(!raw_refresh_cipher.0.contains("manual-refresh-secret"));

    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.id, created_id);
    assert_eq!(acquired.email.as_deref(), Some("jwt@example.com"));
    assert_eq!(acquired.account_id.as_deref(), Some("jwt-account"));
    assert_eq!(acquired.user_id.as_deref(), Some("jwt-user"));
    assert_eq!(acquired.plan_type.as_deref(), Some("team"));
}

#[tokio::test]
async fn admin_account_manual_create_should_reject_missing_invalid_expired_or_unbound_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-create-invalid.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([93u8; 32]),
        ApiKeyHasher::new([94u8; 32]),
    );
    let app = router::router().with_state(state);

    let cases = [
        ("missing tokens", json!({})),
        ("invalid jwt", json!({"token": "not-a-jwt"})),
        (
            "expired jwt",
            json!({"token": test_jwt_with_exp(
                Some("expired-account"),
                Some("expired-user"),
                Some("expired@example.com"),
                Some("plus"),
                1_600_000_000,
            )}),
        ),
        (
            "missing account claim",
            json!({"token": test_jwt_with_exp(
                None,
                Some("claimless-user"),
                Some("claimless@example.com"),
                Some("free"),
                4_102_444_800,
            )}),
        ),
    ];

    for (name, payload) in cases {
        let response = post_admin_account(&app, payload).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
    }
}

#[tokio::test]
async fn admin_account_manual_create_refresh_only_should_exchange_rotate_and_sync_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-refresh-only.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([95u8; 32]);
    let token = test_jwt(
        "rt-account",
        Some("rt-user"),
        Some("rt@example.com"),
        Some("plus"),
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([96u8; 32]),
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: token.clone(),
                refresh_token: Some("rotated-refresh".to_string()),
            }),
            calls: calls.clone(),
        },
    );
    let app = router::router().with_state(state.clone());

    let response = post_admin_account(
        &app,
        json!({
            "refreshToken": "initial-refresh",
            "email": "caller@example.com",
            "planType": "caller-plan"
        }),
    )
    .await;
    let status = response.status();
    let body = response_json(response).await;
    let created_id = body["data"]["id"].as_str().unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["email"], "rt@example.com");
    assert_eq!(body["data"]["accountId"], "rt-account");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    assert_eq!(*calls.lock().await, vec!["initial-refresh".to_string()]);

    let stored = SqliteAccountStore::new(pool, secret_box)
        .get(created_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "rotated-refresh"
    );

    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.id, created_id);
    assert_eq!(acquired.refresh_token.as_deref(), Some("rotated-refresh"));
}

#[tokio::test]
async fn admin_account_manual_create_refresh_only_should_preserve_input_refresh_without_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("admin-account-refresh-preserve-input.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([97u8; 32]);
    let token = test_jwt(
        "rt-preserve-account",
        Some("rt-preserve-user"),
        Some("preserve@example.com"),
        Some("free"),
    );
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([98u8; 32]),
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: token,
                refresh_token: None,
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    );
    let app = router::router().with_state(state);

    let response = post_admin_account(&app, json!({"refreshToken": "preserved-refresh"})).await;
    let body = response_json(response).await;
    let created_id = body["data"]["id"].as_str().unwrap();

    let stored = SqliteAccountStore::new(pool, secret_box)
        .get(created_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "preserved-refresh"
    );
}

#[tokio::test]
async fn admin_account_manual_create_should_update_existing_and_preserve_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-update-existing.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([99u8; 32]);
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([100u8; 32]),
    );
    let app = router::router().with_state(state.clone());

    let first_token = test_jwt(
        "team-account",
        Some("same-user"),
        Some("first@example.com"),
        Some("free"),
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "first-refresh"
        }),
    )
    .await;
    let first_body = response_json(first_response).await;
    let first_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_token = test_jwt(
        "team-account",
        Some("same-user"),
        Some("second@example.com"),
        Some("team"),
    );
    let second_response = post_admin_account(&app, json!({"token": second_token})).await;
    let status = second_response.status();
    let second_body = response_json(second_response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(second_body["data"]["id"], first_id);
    assert_eq!(second_body["data"]["email"], "second@example.com");
    assert_eq!(second_body["data"]["planType"], "team");

    let count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
    let stored = SqliteAccountStore::new(pool, secret_box)
        .get(&first_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &second_token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "first-refresh"
    );

    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.id, first_id);
    assert_eq!(acquired.access_token, second_token);
    assert_eq!(acquired.refresh_token.as_deref(), Some("first-refresh"));
}

#[tokio::test]
async fn admin_account_manual_create_refresh_only_should_preserve_existing_refresh_without_rotation(
) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("admin-account-refresh-preserve-existing.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([101u8; 32]);
    let refreshed_token = test_jwt(
        "rt-existing-account",
        Some("rt-existing-user"),
        Some("second@example.com"),
        Some("team"),
    );
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([102u8; 32]),
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: refreshed_token.clone(),
                refresh_token: None,
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    );
    let app = router::router().with_state(state);
    let first_token = test_jwt(
        "rt-existing-account",
        Some("rt-existing-user"),
        Some("first@example.com"),
        Some("free"),
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "old-refresh"
        }),
    )
    .await;
    let first_body = response_json(first_response).await;
    let account_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_response =
        post_admin_account(&app, json!({"refreshToken": "incoming-refresh"})).await;
    assert_eq!(second_response.status(), StatusCode::OK);

    let stored = SqliteAccountStore::new(pool, secret_box)
        .get(&account_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &refreshed_token);
    assert_eq!(stored.refresh_token.unwrap().expose_secret(), "old-refresh");
    assert_eq!(stored.email.as_deref(), Some("second@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("team"));
}

#[tokio::test]
async fn admin_accounts_import_cli_should_read_auth_file_store_encrypted_and_sync_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-import-cli.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([103u8; 32]);
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([104u8; 32]),
    );
    let app = router::router().with_state(state.clone());
    let codex_home = dir.path().join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    let token = test_jwt(
        "cli-account",
        Some("cli-user"),
        Some("cli@example.com"),
        Some("plus"),
    );
    fs::write(
        codex_home.join("auth.json"),
        json!({
            "access_token": token,
            "refresh_token": "cli-refresh-secret"
        })
        .to_string(),
    )
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import-cli")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_import_cli")
                .body(Body::from(
                    json!({
                        "codexHome": codex_home.display().to_string(),
                        "id": "caller-id",
                        "email": "caller@example.com",
                        "accountId": "caller-account",
                        "userId": "caller-user",
                        "label": "caller-label",
                        "planType": "caller-plan"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requestId"], "req_import_cli");
    assert_eq!(body["data"]["sourceFormat"], "codex_cli");
    assert_eq!(body["data"]["imported"], 1);
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());

    let stored = SqliteAccountStore::new(pool.clone(), secret_box)
        .list(None, 10)
        .await
        .unwrap()
        .items
        .remove(0);
    assert_ne!(stored.id, "caller-id");
    assert_eq!(stored.email.as_deref(), Some("cli@example.com"));
    assert_eq!(stored.account_id.as_deref(), Some("cli-account"));
    assert_eq!(stored.user_id.as_deref(), Some("cli-user"));
    assert_eq!(stored.label, None);
    assert_eq!(stored.plan_type.as_deref(), Some("plus"));
    assert_eq!(stored.access_token.expose_secret(), &token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "cli-refresh-secret"
    );
    let raw: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind(&stored.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(raw.0.starts_with("v1:"));
    assert!(!raw.0.contains(&token));
    assert!(raw.1.starts_with("v1:"));
    assert!(!raw.1.contains("cli-refresh-secret"));

    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.id, stored.id);
}

#[tokio::test]
async fn admin_account_quota_warnings_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-warnings-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([87u8; 32]),
        ApiKeyHasher::new([88u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/quota-warnings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_account_quota_warnings_should_return_threshold_matches_from_cached_quota() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-warnings.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([89u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_warn".to_string(),
            email: Some("warn@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-warn".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quiet".to_string(),
            email: Some("quiet@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quiet".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind(
        json!({
            "rate_limit": {
                "used_percent": 85,
                "reset_at": 1770000100
            },
            "secondary_rate_limit": {
                "used_percent": 91,
                "reset_at": 1770000200
            }
        })
        .to_string(),
    )
    .bind("2026-06-13T00:00:00Z")
    .bind("2026-06-13T00:00:00Z")
    .bind("acct_warn")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind(
        json!({
            "rate_limit": {
                "used_percent": 25,
                "reset_at": 1770000300
            },
            "secondary_rate_limit": null
        })
        .to_string(),
    )
    .bind("2026-06-13T01:00:00Z")
    .bind("2026-06-13T01:00:00Z")
    .bind("acct_quiet")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([90u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/quota-warnings")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["updatedAt"], "2026-06-13T00:00:00+00:00");
    let warnings = body["data"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 2);
    assert!(warnings
        .iter()
        .all(|warning| warning["accountId"] == "acct_warn"));
    assert!(warnings.iter().any(|warning| {
        warning["window"] == "primary"
            && warning["level"] == "warning"
            && warning["usedPercent"] == 85.0
            && warning["resetAt"] == 1770000100
    }));
    assert!(warnings.iter().any(|warning| {
        warning["window"] == "secondary"
            && warning["level"] == "critical"
            && warning["usedPercent"] == 91.0
            && warning["resetAt"] == 1770000200
    }));
}

#[tokio::test]
async fn admin_account_quota_warnings_should_ignore_invalid_and_below_threshold_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-warnings-edge.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([85u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_invalid_quota".to_string(),
            email: Some("invalid-quota@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-invalid-quota".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_below_threshold".to_string(),
            email: Some("below-threshold@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-below-threshold".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind("{not valid json")
    .bind("2026-06-13T00:00:00Z")
    .bind("2026-06-13T00:00:00Z")
    .bind("acct_invalid_quota")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind(
        json!({
            "rate_limit": {
                "used_percent": 79,
                "reset_at": 1770000600
            },
            "secondary_rate_limit": {
                "used_percent": 79,
                "reset_at": 1770000700
            }
        })
        .to_string(),
    )
    .bind("2026-06-13T01:00:00Z")
    .bind("2026-06-13T01:00:00Z")
    .bind("acct_below_threshold")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([86u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/quota-warnings")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["warnings"].as_array().unwrap().is_empty());
    assert!(body["data"]["updatedAt"].is_null());
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn post_admin_account(app: &axum::Router, payload: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn admin_accounts_test_app(
    db_name: &str,
    key_byte: u8,
) -> (
    axum::Router,
    AppState,
    SqlitePool,
    tempfile::TempDir,
    SecretBox,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([key_byte; 32]);
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([key_byte; 32]),
    );
    let app = router::router().with_state(state.clone());
    (app, state, pool, dir, secret_box)
}

async fn admin_accounts_test_app_with_oauth_client<C>(
    db_name: &str,
    key_byte: u8,
    oauth_client: C,
) -> (axum::Router, AppState, SqlitePool, tempfile::TempDir)
where
    C: OAuthClient + TokenRefresher + Clone,
{
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_api_key_hasher_and_oauth_client(
        test_config(url),
        pool.clone(),
        SecretBox::new([key_byte; 32]),
        ApiKeyHasher::new([key_byte; 32]),
        oauth_client,
    );
    let app = router::router().with_state(state.clone());
    (app, state, pool, dir)
}

async fn seed_encrypted_account(pool: &SqlitePool, secret_box: SecretBox, account: NewAccount) {
    SqliteAccountStore::new(pool.clone(), secret_box)
        .insert(account)
        .await
        .unwrap();
}

fn test_jwt(
    account_id: &str,
    user_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
) -> String {
    test_jwt_with_exp(Some(account_id), user_id, email, plan_type, 4_102_444_800)
}

fn test_jwt_with_exp(
    account_id: Option<&str>,
    user_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
    exp: i64,
) -> String {
    let header = json!({"alg": "none", "typ": "JWT"});
    let payload = json!({
        "exp": exp,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": user_id,
            "chatgpt_plan_type": plan_type,
        },
        "https://api.openai.com/profile": {
            "email": email,
        }
    });
    format!("{}.{}.", jwt_part(&header), jwt_part(&payload))
}

#[derive(Clone)]
struct StaticTokenRefresher {
    result: Result<TokenPair, RefreshFailure>,
    calls: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct HealthCheckTokenRefresher {
    calls: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct StaticOAuthClient {
    device_response: Result<DeviceCode, OAuthError>,
    poll_response: Result<TokenPair, OAuthError>,
    exchange_response: Result<TokenPair, OAuthError>,
    poll_calls: Arc<Mutex<Vec<String>>>,
    exchange_calls: Arc<Mutex<Vec<ExchangeCall>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExchangeCall {
    code: String,
    code_verifier: String,
    redirect_uri: String,
}

#[async_trait]
impl TokenRefresher for StaticTokenRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.lock().await.push(refresh_token.to_string());
        self.result.clone()
    }
}

#[async_trait]
impl TokenRefresher for HealthCheckTokenRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.lock().await.push(refresh_token.to_string());
        match refresh_token {
            "refresh-acct_health_alive" => Ok(TokenPair {
                access_token: test_jwt(
                    "health-alive-account",
                    Some("health-user"),
                    Some("health-alive@example.com"),
                    Some("plus"),
                ),
                refresh_token: Some("new-health-refresh".to_string()),
            }),
            "refresh-acct_health_dead" => Err(RefreshFailure::InvalidGrant),
            _ => Err(RefreshFailure::Transport),
        }
    }
}

#[async_trait]
impl TokenRefresher for StaticOAuthClient {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        Err(RefreshFailure::Transport)
    }
}

#[async_trait]
impl OAuthClient for StaticOAuthClient {
    async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenPair, OAuthError> {
        self.exchange_calls.lock().await.push(ExchangeCall {
            code: code.to_string(),
            code_verifier: code_verifier.to_string(),
            redirect_uri: redirect_uri.to_string(),
        });
        self.exchange_response.clone()
    }

    async fn request_device_code(&self) -> Result<DeviceCode, OAuthError> {
        self.device_response.clone()
    }

    async fn poll_device_token(&self, device_code: &str) -> Result<TokenPair, OAuthError> {
        self.poll_calls.lock().await.push(device_code.to_string());
        self.poll_response.clone()
    }
}

fn jwt_part(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}

fn test_config(database_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig {
            base_url: "https://chatgpt.com/backend-api".to_string(),
        },
        model: ModelConfig {
            default_model: "gpt-5.5".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            aliases: BTreeMap::new(),
        },
        auth: AuthConfig {
            refresh_margin_seconds: 300,
            refresh_enabled: true,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "least_used".to_string(),
            tier_priority: Vec::new(),
            oauth_client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            oauth_auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
            oauth_token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
        },
        quota: QuotaConfig {
            refresh_interval_minutes: 5,
            warning_thresholds: QuotaWarningThresholds {
                primary: vec![80, 90],
                secondary: vec![80, 90],
            },
            skip_exhausted: true,
        },
        usage_stats: UsageStatsConfig {
            history_retention_days: None,
        },
        database: DatabaseConfig { url: database_url },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig {
            force_http11: false,
        },
        ws_pool: WebSocketPoolConfig::default(),
        admin: AdminConfig {
            session_ttl_minutes: 1440,
            session_cleanup_interval_secs: 3600,
            default_username: "admin".to_string(),
            default_password: "admin".to_string(),
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}
