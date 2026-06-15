use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use chrono::Utc;
use serde_json::Value;
use tower::ServiceExt;

use codex_proxy_rs::{
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    platform::identity::admin_session::hash_admin_password,
    platform::storage::db::connect_sqlite,
    runtime::build_router,
    runtime::state::AppState,
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
        ws_pool: Default::default(),
        admin: AdminConfig {
            session_ttl_minutes: 60,
            default_username: "admin".to_string(),
            default_password: "admin".to_string(),
            session_cleanup_interval_secs: 3600,
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

#[tokio::test]
async fn admin_login_issues_http_only_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-login.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_user(&pool, "correct-password").await;
    let app = build_router(AppState::with_pool(test_config(url), pool.clone()));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .header("x-request-id", "req_login")
                .body(Body::from(r#"{"password":"correct-password"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("cpr_admin_session="));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    let session_cookie = cookie.split(';').next().unwrap().to_string();
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_login");
    assert!(body["data"]["expiresAt"].is_string());

    let logs_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs")
                .header("cookie", session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logs_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_login_rejects_client_api_key_as_password_or_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-login.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_user(&pool, "correct-password").await;
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .header("authorization", "Bearer cpr_not_an_admin_session")
                .header("x-request-id", "req_login_bad")
                .body(Body::from(r#"{"password":"cpr_not_an_admin_password"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get("set-cookie").is_none());
    let body = response_json(response).await;
    assert_eq!(body["code"], 40102);
    assert_eq!(body["requestId"], "req_login_bad");

    let logs_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs")
                .header("authorization", "Bearer cpr_not_an_admin_session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logs_response.status(), StatusCode::UNAUTHORIZED);
}

async fn seed_admin_user(pool: &sqlx::SqlitePool, password: &str) {
    let now = Utc::now().to_rfc3339();
    let hash = hash_admin_password(password).unwrap();
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
    )
    .bind("admin_1")
    .bind(hash)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .unwrap();
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
