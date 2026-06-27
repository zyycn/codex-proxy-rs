use std::{collections::BTreeMap, fs, path::Path};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use codex_proxy_rs::{
    admin::auth::service::SqliteAdminSessionStore,
    admin::keys::service::SqliteClientKeyStore,
    admin::monitoring::event_store::SqliteEventLogStore,
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
    upstream::accounts::token_refresh::RefreshLeaseStore,
    upstream::accounts::{cookies::SqliteCookieStore, store::SqliteAccountStore},
    upstream::fingerprint::FingerprintRepository,
};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tower::util::ServiceExt;

#[tokio::test]
async fn admin_settings_should_require_admin_session_cookie() {
    let (app, _dir) = admin_settings_test_app("admin-settings-auth.sqlite").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings")
                .header("x-request-id", "req_settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response_json(response).await["code"], 40101);
}

#[tokio::test]
async fn admin_settings_should_return_runtime_fields() {
    let (app, _dir) = admin_settings_test_app("admin-settings-get.sqlite").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_get")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert_eq!(body["data"]["defaultModel"], "gpt-5.5");
}

#[tokio::test]
async fn admin_settings_update_should_require_admin_session_cookie() {
    let (app, _dir) = admin_settings_test_app("admin-settings-update-auth.sqlite").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("x-request-id", "req_settings_update_auth")
                .body(Body::from(r#"{"defaultModel":"gpt-5.5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_settings_update_should_persist_retained_fields_to_config_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-update.sqlite");
    let url = format!("sqlite://{}", db.display());
    let config = test_config(url);
    write_config_yaml(dir.path(), &config);
    let pool = connect_sqlite(&config.database.url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([53u8; 32]);
    let hasher = ApiKeyHasher::new([54u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), secret_box),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let mut services = Services::new(&config, stores, fingerprint);
    services.settings = std::sync::Arc::new(
        codex_proxy_rs::config::settings::RuntimeSettingsService::with_config_path(
            config.clone(),
            dir.path().join("config.yaml"),
        ),
    );
    let services = std::sync::Arc::new(services);
    let state = AppState {
        config: config.clone(),
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_update")
                .body(Body::from(
                    json!({
                        "defaultModel": "gpt-6", "rotationStrategy": "round_robin",
                        "maxConcurrentPerAccount": 7,
                        "requestIntervalMs": 80
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["defaultModel"], "gpt-6");

    let get_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response_json(get_response).await["data"]["defaultModel"],
        "gpt-6"
    );

    let persisted = AppConfig::load_from_dir(dir.path()).unwrap();
    assert_eq!(persisted.model.default_model, "gpt-6");
    assert_eq!(persisted.auth.rotation_strategy, "round_robin");
    assert_eq!(persisted.auth.max_concurrent_per_account, 7);
    assert_eq!(persisted.auth.request_interval_ms, 80);
    assert!(persisted.auth.refresh_enabled);
    assert_eq!(persisted.database.url, config.database.url);
    assert_eq!(
        persisted.security.master_key_file,
        config.security.master_key_file
    );
}

#[tokio::test]
async fn admin_settings_update_should_reject_unsupported_or_invalid_fields() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-update-invalid.sqlite");
    let url = format!("sqlite://{}", db.display());
    let config = test_config(url);
    write_config_yaml(dir.path(), &config);
    let pool = connect_sqlite(&config.database.url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([53u8; 32]);
    let hasher = ApiKeyHasher::new([54u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), secret_box),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_update_invalid")
                .body(Body::from(
                    json!({"refreshEnabled": false, "rotationStrategy": "random"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

async fn admin_settings_test_app(db_name: &str) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let config = test_config(url);
    let secret_box = SecretBox::new([53u8; 32]);
    let hasher = ApiKeyHasher::new([54u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), secret_box),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
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

fn write_config_yaml(path: &Path, config: &AppConfig) {
    fs::write(
        path.join("config.yaml"),
        serde_yml::to_string(config).unwrap(),
    )
    .unwrap();
}

fn test_config(database_url: String) -> AppConfig {
    let mut aliases = BTreeMap::new();
    aliases.insert("codex-fast".to_string(), "gpt-5.5".to_string());
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
            default_reasoning_effort: Some("high".to_string()),
            service_tier: Some("flex".to_string()),
            aliases,
        },
        auth: AuthConfig {
            refresh_margin_seconds: 240,
            refresh_enabled: true,
            refresh_concurrency: 2,
            max_concurrent_per_account: 4,
            request_interval_ms: 50,
            rotation_strategy: "least_used".to_string(),
            tier_priority: vec!["team".to_string(), "plus".to_string()],
            oauth_client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            oauth_auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
            oauth_token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
        },
        quota: QuotaConfig {
            refresh_interval_minutes: 5,
            warning_thresholds: QuotaWarningThresholds {
                primary: vec![80, 90],
                secondary: vec![70, 95],
            },
            skip_exhausted: true,
        },
        usage_stats: UsageStatsConfig {
            history_retention_days: Some(30),
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
            enabled: true,
            capacity: 2_000,
            capture_body: false,
        },
    }
}
