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
    app::services::{BackgroundTaskStores, Services},
    app::state::AppState,
    codex::fingerprint::{Fingerprint, FingerprintRepository},
    config::types::{
        AdminConfig, ApiConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig, WebSocketPoolConfig,
    },
    config::AppConfig,
    gateway::dispatch::session_affinity::SqliteSessionAffinityStore,
    infra::{crypto::SecretBox, database::connect_sqlite, identity::ApiKeyHasher},
    telemetry::event_store::SqliteEventLogStore,
};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tower::util::ServiceExt;

#[path = "api_keys_routes/import_export.rs"]
mod api_keys_import_export;
#[path = "api_keys_routes/lifecycle.rs"]
mod api_keys_lifecycle;
#[path = "api_keys_routes/authorization.rs"]
mod authorization;

async fn admin_client_key_test_app(db_name: &str) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let config = test_config(url);
    let secret_box = SecretBox::new([43u8; 32]);
    let hasher = ApiKeyHasher::new([44u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), secret_box),
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
        dir,
    )
}

async fn create_admin_client_key(app: &axum::Router, name: &str) -> (String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "name": name }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    (
        body["data"]["id"].as_str().unwrap().to_string(),
        body["data"]["plaintext"].as_str().unwrap().to_string(),
    )
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
