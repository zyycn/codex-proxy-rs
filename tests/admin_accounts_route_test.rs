use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use chrono::Utc;
use serde_json::{json, Value};
use tower::ServiceExt;

use codex_proxy_rs::{
    app::build_router,
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
