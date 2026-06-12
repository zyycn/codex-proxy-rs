use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use secrecy::ExposeSecret;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::sync::Mutex;
use tower::ServiceExt;

use codex_proxy_rs::{
    accounts::repository::AccountRepository,
    app::build_router,
    auth::{
        api_key::ApiKeyHasher,
        refresh::{RefreshFailure, TokenRefresher},
        token::TokenPair,
    },
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    crypto::SecretBox,
    state::AppState,
    storage::db::connect_sqlite,
};

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
        admin: AdminConfig {
            session_ttl_minutes: 1440,
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            max_file_bytes: 10_485_760,
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

#[tokio::test]
async fn admin_accounts_import_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool,
        SecretBox::new([11u8; 32]),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("x-request-id", "req_accounts")
                .body(Body::from(r#"{"accounts":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_accounts");
}

#[tokio::test]
async fn admin_accounts_import_should_store_tokens_encrypted_and_list_sanitized_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool.clone(),
        SecretBox::new([12u8; 32]),
    ));
    let import_body = json!({
        "accounts": [{
            "id": "acct_imported",
            "email": "user@example.com",
            "accountId": "chatgpt-account",
            "userId": "chatgpt-user",
            "label": "primary",
            "planType": "plus",
            "token": "access-secret",
            "refreshToken": "refresh-secret",
            "status": "active"
        }]
    });

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts")
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(import_response.status(), StatusCode::OK);
    let body = response_json(import_response).await;
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    let stored: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_imported")
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
                .uri("/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_response.status(), StatusCode::OK);
    let body = response_json(list_response).await;
    assert_eq!(body["data"][0]["id"], "acct_imported");
    assert_eq!(body["data"][0]["email"], "user@example.com");
    assert!(body["data"][0].get("token").is_none());
    assert!(body["data"][0].get("refreshToken").is_none());
    assert_eq!(body["page"]["limit"], 10);
}

#[tokio::test]
async fn admin_accounts_import_should_accept_sub2api_oauth_export_and_mark_format() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-sub2api.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool.clone(),
        SecretBox::new([14u8; 32]),
    ));
    let import_body = json!({
        "type": "sub2api-data",
        "version": 1,
        "proxies": [],
        "accounts": [
            {
                "name": "Sub2API Team",
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": "Bearer sub2api-access-secret",
                    "refresh_token": "rt_sub2api",
                    "email": "team@example.com",
                    "chatgpt_account_id": "chatgpt-account",
                    "chatgpt_user_id": "chatgpt-user",
                    "plan_type": "team"
                },
                "concurrency": 0,
                "priority": 0
            },
            {
                "name": "Other Provider",
                "platform": "anthropic",
                "type": "oauth",
                "credentials": {
                    "access_token": "ignored-secret"
                }
            }
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_sub2api")
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    let stored: (String, String, String, String, String) =
        sqlx::query_as("select email, account_id, user_id, label, plan_type from accounts")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.0, "team@example.com");
    assert_eq!(stored.1, "chatgpt-account");
    assert_eq!(stored.2, "chatgpt-user");
    assert_eq!(stored.3, "Sub2API Team");
    assert_eq!(stored.4, "team");
}

#[tokio::test]
async fn admin_accounts_import_should_accept_sub2api_native_account_export_without_proxy_data() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-sub2api-native.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool,
        SecretBox::new([15u8; 32]),
    ));
    let import_body = json!({
        "accounts": [{
            "id": "acct_sub2api_native",
            "token": "native-access-secret",
            "refreshToken": "native-refresh-secret",
            "email": "native@example.com",
            "accountId": "native-account",
            "userId": "native-user",
            "label": "Native Sub2API",
            "planType": "plus",
            "proxyApiKey": "ignored-proxy-secret",
            "status": "active",
            "usage": {
                "request_count": 1,
                "input_tokens": 0,
                "output_tokens": 0,
                "cached_tokens": 0
            },
            "cachedQuota": null,
            "quotaVerifyRequired": false
        }]
    });

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_sub2api_native")
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(import_response.status(), StatusCode::OK);
    let body = response_json(import_response).await;
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(body["data"]["imported"], 1);

    let list_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_sub2api_native_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = response_json(list_response).await;
    assert_eq!(list["data"][0]["id"], "acct_sub2api_native");
    assert_eq!(list["data"][0]["planType"], "plus");
    assert!(list["data"][0].get("proxyApiKey").is_none());
    assert!(list["data"][0].get("usage").is_none());
}

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

#[tokio::test]
async fn admin_account_label_should_update_and_clear_label() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-account-label.sqlite", 16).await;
    import_test_account(&app, "session_1", "acct_label").await;

    let set_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_label/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_label")
                .body(Body::from(r#"{"label":"Team Alpha"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(set_response.status(), StatusCode::OK);
    let body = response_json(set_response).await;
    assert_eq!(body["data"]["label"], "Team Alpha");

    let clear_response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_label/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":null}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(clear_response.status(), StatusCode::OK);
    let body = response_json(clear_response).await;
    assert!(body["data"]["label"].is_null());

    let stored: (Option<String>,) = sqlx::query_as("select label from accounts where id = ?")
        .bind("acct_label")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored.0, None);
    assert!(state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .is_some());
}

#[tokio::test]
async fn admin_account_label_should_reject_too_long_or_missing_account() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-label-invalid.sqlite", 17).await;
    import_test_account(&app, "session_1", "acct_label_invalid").await;
    let long_label = "x".repeat(65);

    let too_long = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_label_invalid/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "label": long_label }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);

    let missing = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/missing/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"Team Alpha"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_account_status_should_update_database_and_runtime_pool() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-account-status.sqlite", 18).await;
    import_test_account(&app, "session_1", "acct_status").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_status/status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_status")
                .body(Body::from(r#"{"status":"disabled"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["status"], "disabled");

    let stored: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_status")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored.0, "disabled");
    assert!(state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .is_none());
}

#[tokio::test]
async fn admin_account_delete_should_remove_database_row_and_runtime_pool_entry() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-account-delete.sqlite", 19).await;
    import_test_account(&app, "session_1", "acct_delete").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/accounts/acct_delete")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_delete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["deleted"], true);

    let row_count: (i64,) = sqlx::query_as("select count(*) from accounts where id = ?")
        .bind("acct_delete")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count.0, 0);
    assert!(state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .is_none());

    let missing = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/accounts/acct_delete")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_accounts_batch_delete_should_delete_found_accounts_and_report_missing_ids() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-batch-delete.sqlite", 20).await;
    import_test_account(&app, "session_1", "acct_batch_delete_a").await;
    import_test_account(&app, "session_1", "acct_batch_delete_b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_delete_a", "ghost", "acct_batch_delete_b"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["deleted"], 2);
    assert_eq!(body["data"]["notFound"], json!(["ghost"]));

    let row_count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count.0, 0);
    assert!(state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .is_none());

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"ids":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_batch_status_should_update_found_accounts_and_reject_invalid_status() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-batch-status.sqlite", 21).await;
    import_test_account(&app, "session_1", "acct_batch_status_a").await;
    import_test_account(&app, "session_1", "acct_batch_status_b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/batch-status")
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
    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.id, "acct_batch_status_b");

    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/batch-status")
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
                .uri("/admin/accounts/batch-status")
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
async fn admin_account_cookies_should_set_get_and_delete_encrypted_cookie_header() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-account-cookies.sqlite", 22).await;
    import_test_account(&app, "session_1", "acct_cookies").await;

    let set_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    r#"{"cookies":"cf_clearance=clear-secret; __cf_bm=bm-secret"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(set_response.status(), StatusCode::OK);
    let body = response_json(set_response).await;
    assert_eq!(
        body["data"]["cookies"],
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

    let get_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);
    let body = response_json(get_response).await;
    assert_eq!(
        body["data"]["cookies"],
        "__cf_bm=bm-secret; cf_clearance=clear-secret"
    );

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_response.status(), StatusCode::OK);

    let get_empty = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_empty.status(), StatusCode::OK);
    let body = response_json(get_empty).await;
    assert!(body["data"]["cookies"].is_null());
}

#[tokio::test]
async fn admin_account_cookies_should_require_existing_account_and_non_empty_cookies() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-cookies-invalid.sqlite", 23).await;

    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/missing/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    import_test_account(&app, "session_1", "acct_cookie_invalid").await;
    let invalid = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_cookie_invalid/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"cookies":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_export_should_return_native_accounts_with_tokens_and_filter_ids() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-export-native.sqlite", 24).await;
    import_test_account(&app, "session_1", "acct_export_a").await;
    import_test_account(&app, "session_1", "acct_export_b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/export?ids=acct_export_a")
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
    assert!(body["data"]["accounts"][0].get("proxyApiKey").is_none());

    let invalid = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/export?format=proxy")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_export_should_return_sub2api_openai_oauth_payload_without_proxy_fields() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-export-sub2api.sqlite", 25).await;
    import_test_account(&app, "session_1", "acct_export_sub2api").await;

    let label_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_export_sub2api/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"Sub2API Export"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(label_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/export?format=sub2api")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["type"], "sub2api-data");
    assert_eq!(body["data"]["version"], 1);
    assert_eq!(body["data"]["proxies"], json!([]));
    assert_eq!(body["data"]["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["accounts"][0]["name"], "Sub2API Export");
    assert_eq!(body["data"]["accounts"][0]["platform"], "openai");
    assert_eq!(body["data"]["accounts"][0]["type"], "oauth");
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["access_token"],
        "access-acct_export_sub2api"
    );
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["refresh_token"],
        "refresh-acct_export_sub2api"
    );
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["email"],
        "acct_export_sub2api@example.com"
    );
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["plan_type"],
        "plus"
    );
    assert!(body["data"]["accounts"][0].get("proxy").is_none());
    assert!(body["data"]["accounts"][0].get("proxyUrl").is_none());
}

#[tokio::test]
async fn manual_add_should_derive_claims_ignore_metadata_and_sync_pool() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-account-manual-add.sqlite", 26).await;
    let token = test_jwt(
        Some("jwt-account"),
        Some("jwt-user"),
        Some("jwt@example.com"),
        Some("team"),
        3600,
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
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
                        "token": token,
                        "refreshToken": "manual-refresh-secret",
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
    let response_id = body["data"]["id"].as_str().unwrap();
    assert_ne!(response_id, "caller-id");
    assert_eq!(body["data"]["email"], "jwt@example.com");
    assert_eq!(body["data"]["accountId"], "jwt-account");
    assert_eq!(body["data"]["userId"], "jwt-user");
    assert_eq!(body["data"]["planType"], "team");
    assert!(body["data"]["label"].is_null());
    assert_eq!(body["data"]["status"], "active");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());

    let stored = sqlx::query(
        "select id, email, account_id, user_id, label, plan_type, status, access_token_cipher, refresh_token_cipher from accounts",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored.get::<String, _>("id"), response_id);
    assert_eq!(
        stored.get::<Option<String>, _>("email").as_deref(),
        Some("jwt@example.com")
    );
    assert_eq!(
        stored.get::<Option<String>, _>("account_id").as_deref(),
        Some("jwt-account")
    );
    assert_eq!(
        stored.get::<Option<String>, _>("user_id").as_deref(),
        Some("jwt-user")
    );
    assert_eq!(stored.get::<Option<String>, _>("label"), None);
    assert_eq!(
        stored.get::<Option<String>, _>("plan_type").as_deref(),
        Some("team")
    );
    assert_eq!(stored.get::<String, _>("status"), "active");
    let access_token_cipher = stored.get::<String, _>("access_token_cipher");
    assert!(access_token_cipher.starts_with("v1:"));
    assert!(!access_token_cipher.contains(&token));
    let refresh_token_cipher = stored.get::<String, _>("refresh_token_cipher");
    assert!(refresh_token_cipher.starts_with("v1:"));
    assert!(!refresh_token_cipher.contains("manual-refresh-secret"));

    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.id, response_id);
    assert_eq!(acquired.email.as_deref(), Some("jwt@example.com"));
    assert_eq!(acquired.account_id.as_deref(), Some("jwt-account"));
    assert_eq!(acquired.user_id.as_deref(), Some("jwt-user"));
    assert_eq!(acquired.plan_type.as_deref(), Some("team"));
}

#[tokio::test]
async fn admin_account_manual_add_should_reject_missing_invalid_expired_or_unbound_tokens() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-manual-add-invalid.sqlite", 27).await;

    let missing_tokens = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_tokens.status(), StatusCode::BAD_REQUEST);

    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "token": "not-a-jwt"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let expired = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "token": test_jwt(
                            Some("expired-account"),
                            Some("expired-user"),
                            Some("expired@example.com"),
                            Some("plus"),
                            -3600,
                        )
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::BAD_REQUEST);

    let missing_account_claim = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "token": test_jwt(
                            None,
                            Some("claimless-user"),
                            Some("claimless@example.com"),
                            Some("free"),
                            3600,
                        )
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_account_claim.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_account_manual_add_should_exchange_refresh_token_only_and_rotate_refresh_token() {
    let token = test_jwt(
        Some("rt-account"),
        Some("rt-user"),
        Some("rt@example.com"),
        Some("plus"),
        3600,
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let refresher = StaticTokenRefresher {
        result: Ok(TokenPair {
            access_token: token.clone(),
            refresh_token: Some("rotated-refresh".to_string()),
        }),
        calls: calls.clone(),
    };
    let (app, state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-manual-refresh-only.sqlite",
        28,
        refresher,
    )
    .await;

    let response = post_admin_account(
        &app,
        json!({
            "refreshToken": "initial-refresh",
            "email": "caller@example.com",
            "planType": "caller-plan"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["email"], "rt@example.com");
    assert_eq!(body["data"]["accountId"], "rt-account");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    assert_eq!(*calls.lock().await, vec!["initial-refresh".to_string()]);

    let repo = AccountRepository::new(pool, SecretBox::new([28u8; 32]));
    let stored = repo
        .get(body["data"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "rotated-refresh"
    );
    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.refresh_token.as_deref(), Some("rotated-refresh"));
}

#[tokio::test]
async fn admin_account_manual_add_should_preserve_input_refresh_token_when_exchange_omits_rotation()
{
    let token = test_jwt(
        Some("rt-preserve-account"),
        Some("rt-preserve-user"),
        Some("preserve@example.com"),
        Some("free"),
        3600,
    );
    let refresher = StaticTokenRefresher {
        result: Ok(TokenPair {
            access_token: token,
            refresh_token: None,
        }),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-manual-refresh-preserve.sqlite",
        29,
        refresher,
    )
    .await;

    let response = post_admin_account(&app, json!({ "refreshToken": "preserved-refresh" })).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let repo = AccountRepository::new(pool, SecretBox::new([29u8; 32]));
    let stored = repo
        .get(body["data"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "preserved-refresh"
    );
}

#[tokio::test]
async fn admin_account_manual_add_should_update_existing_account_for_same_account_and_user() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-account-manual-update-existing.sqlite", 30).await;
    let first_token = test_jwt(
        Some("team-account"),
        Some("same-user"),
        Some("first@example.com"),
        Some("free"),
        3600,
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "first-refresh"
        }),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_json(first_response).await;
    let first_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_token = test_jwt(
        Some("team-account"),
        Some("same-user"),
        Some("second@example.com"),
        Some("team"),
        7200,
    );
    let second_response = post_admin_account(&app, json!({ "token": second_token })).await;

    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_json(second_response).await;
    assert_eq!(second_body["data"]["id"], first_id);
    assert_eq!(second_body["data"]["email"], "second@example.com");
    assert_eq!(second_body["data"]["planType"], "team");

    let count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
    let repo = AccountRepository::new(pool, SecretBox::new([30u8; 32]));
    let stored = repo.get(&first_id).await.unwrap().unwrap();
    assert_eq!(stored.access_token.expose_secret(), &second_token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "first-refresh"
    );
    assert_eq!(stored.email.as_deref(), Some("second@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("team"));

    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.id, first_id);
    assert_eq!(acquired.access_token, second_token);
    assert_eq!(acquired.refresh_token.as_deref(), Some("first-refresh"));
}

#[tokio::test]
async fn manual_add_should_preserve_existing_refresh_when_refresh_only_omits_rotation() {
    let refreshed_token = test_jwt(
        Some("rt-existing-account"),
        Some("rt-existing-user"),
        Some("second@example.com"),
        Some("team"),
        7200,
    );
    let refresher = StaticTokenRefresher {
        result: Ok(TokenPair {
            access_token: refreshed_token.clone(),
            refresh_token: None,
        }),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-manual-refresh-existing-preserve.sqlite",
        31,
        refresher,
    )
    .await;
    let first_token = test_jwt(
        Some("rt-existing-account"),
        Some("rt-existing-user"),
        Some("first@example.com"),
        Some("free"),
        3600,
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "old-refresh"
        }),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_json(first_response).await;
    let account_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_response =
        post_admin_account(&app, json!({ "refreshToken": "incoming-refresh" })).await;

    assert_eq!(second_response.status(), StatusCode::OK);
    let repo = AccountRepository::new(pool, SecretBox::new([31u8; 32]));
    let stored = repo.get(&account_id).await.unwrap().unwrap();
    assert_eq!(stored.access_token.expose_secret(), &refreshed_token);
    assert_eq!(stored.refresh_token.unwrap().expose_secret(), "old-refresh");
    assert_eq!(stored.email.as_deref(), Some("second@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("team"));
}

async fn admin_accounts_test_app(
    db_name: &str,
    key_byte: u8,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_and_secret_box(
        test_config(url),
        pool.clone(),
        SecretBox::new([key_byte; 32]),
    );
    let app = build_router(state.clone());
    (app, state, pool, dir)
}

async fn admin_accounts_test_app_with_refresher<C>(
    db_name: &str,
    key_byte: u8,
    refresher: C,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir)
where
    C: TokenRefresher,
{
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        SecretBox::new([key_byte; 32]),
        ApiKeyHasher::new([key_byte; 32]),
        refresher,
    );
    let app = build_router(state.clone());
    (app, state, pool, dir)
}

async fn post_admin_account(app: &Router, payload: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn import_test_account(app: &Router, session_id: &str, account_id: &str) {
    let import_body = json!({
        "accounts": [{
            "id": account_id,
            "email": format!("{account_id}@example.com"),
            "planType": "plus",
            "token": format!("access-{account_id}"),
            "refreshToken": format!("refresh-{account_id}"),
            "status": "active"
        }]
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", format!("cpr_admin_session={session_id}"))
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn seed_admin_session(pool: &sqlx::SqlitePool, session_id: &str) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
    )
    .bind("admin_1")
    .bind("hash")
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind("admin_1")
    .bind("2999-01-01T00:00:00Z")
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn test_jwt(
    account_id: Option<&str>,
    user_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
    exp_offset_seconds: i64,
) -> String {
    let mut auth = serde_json::Map::new();
    if let Some(account_id) = account_id {
        auth.insert(
            "chatgpt_account_id".to_string(),
            Value::String(account_id.to_string()),
        );
    }
    if let Some(user_id) = user_id {
        auth.insert(
            "chatgpt_user_id".to_string(),
            Value::String(user_id.to_string()),
        );
    }
    if let Some(plan_type) = plan_type {
        auth.insert(
            "chatgpt_plan_type".to_string(),
            Value::String(plan_type.to_string()),
        );
    }
    let mut profile = serde_json::Map::new();
    if let Some(email) = email {
        profile.insert("email".to_string(), Value::String(email.to_string()));
    }
    let payload = json!({
        "exp": Utc::now().timestamp() + exp_offset_seconds,
        "https://api.openai.com/auth": Value::Object(auth),
        "https://api.openai.com/profile": Value::Object(profile),
    });
    let header = json!({ "alg": "none", "typ": "JWT" });
    format!("{}.{}.", jwt_part(&header), jwt_part(&payload),)
}

fn jwt_part(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}

#[derive(Clone)]
struct StaticTokenRefresher {
    result: Result<TokenPair, RefreshFailure>,
    calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl TokenRefresher for StaticTokenRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.lock().await.push(refresh_token.to_string());
        self.result.clone()
    }
}
