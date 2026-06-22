use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use codex_proxy_rs::{
    access::admin_session::SqliteAdminSessionStore,
    access::client_keys::SqliteClientKeyStore,
    accounts::token_refresh::RefreshLeaseStore,
    accounts::{cookies::SqliteCookieStore, store::SqliteAccountStore},
    codex::fingerprint::{Fingerprint, FingerprintRepository},
    config::types::AppConfig,
    config::types::{
        AdminConfig, ApiConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig, WebSocketPoolConfig,
    },
    infra::{crypto::SecretBox, database::connect_sqlite, identity::ApiKeyHasher},
    proxy::dispatch::session_affinity::SqliteSessionAffinityStore,
    runtime::services::{BackgroundTaskStores, Services},
    runtime::state::AppState,
    telemetry::event_store::SqliteEventLogStore,
};
use serde_json::Value;
use sqlx::SqlitePool;
use tower::util::ServiceExt;

#[tokio::test]
async fn responses_route_should_reject_missing_client_api_key() {
    let (app, _key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .body(Body::from(r#"{"model":"gpt-5.5","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn responses_route_should_reject_unknown_models_with_openai_error() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"unknown-model","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "model_not_found"
    );
}

#[tokio::test]
async fn responses_route_should_terminate_generated_stream_errors_with_done_marker() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: response.failed"));
    assert!(body.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn responses_route_should_default_omitted_stream_to_sse() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.5","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn chat_completions_route_should_reject_empty_messages_with_bad_request() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.5","messages":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

async fn test_app_with_client_api_key() -> (axum::Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("openai-responses-routes.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let hasher = ApiKeyHasher::new([95u8; 32]);
    let plaintext = insert_client_api_key(&pool, &hasher).await;
    let config = test_config(url);
    let secret_box = SecretBox::new([96u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([96u8; 32])),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = Fingerprint::default_for_tests();
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    (
        codex_proxy_rs::http::router::router().with_state(state),
        plaintext,
        dir,
    )
}

async fn insert_client_api_key(pool: &SqlitePool, hasher: &ApiKeyHasher) -> String {
    let generated = hasher.generate_client_api_key("test");
    sqlx::query("insert into client_api_keys (id, name, prefix, key_hash, enabled, created_at) values (?, ?, ?, ?, 1, ?)")
        .bind("key_test").bind("test").bind(&generated.prefix).bind(&generated.key_hash)
        .bind("2026-06-18T00:00:00Z").execute(pool).await.unwrap();
    generated.plaintext
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
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
        fingerprint: Default::default(),
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
