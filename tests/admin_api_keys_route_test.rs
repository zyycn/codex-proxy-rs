use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use chrono::Utc;
use serde_json::Value;
use tower::ServiceExt;

use codex_proxy_rs::{
    app::build_router,
    auth::api_key::ApiKeyHasher,
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
async fn admin_api_keys_should_create_list_and_authorize_v1_requests() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-api-keys.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([41u8; 32]),
        ApiKeyHasher::new([42u8; 32]),
    ));

    let rejected = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", "Bearer cpr_not_stored")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/api-keys")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_api_key")
                .body(Body::from(r#"{"name":"cursor"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_response.status(), StatusCode::OK);
    let body = response_json(create_response).await;
    let plaintext = body["data"]["plaintext"].as_str().unwrap().to_string();
    assert!(plaintext.starts_with("cpr_"));
    assert_eq!(body["requestId"], "req_api_key");

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/api-keys?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let body = response_json(list_response).await;
    assert_eq!(body["data"][0]["name"], "cursor");
    assert!(body["data"][0].get("plaintext").is_none());
    assert!(body["data"][0].get("keyHash").is_none());

    let models_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(models_response.status(), StatusCode::OK);
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
