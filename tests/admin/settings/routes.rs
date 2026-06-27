use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use codex_proxy_rs::{
    admin::auth::service::SqliteAdminSessionStore,
    admin::keys::service::SqliteClientKeyStore,
    admin::monitoring::event_store::SqliteEventLogStore,
    config::types::{
        AdminConfig, ApiConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, ServerConfig, TlsConfig, UsageStatsConfig,
        WebSocketPoolConfig,
    },
    config::{settings::RuntimeSettingsService, types::AppConfig},
    infra::database::connect_sqlite,
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
    assert_eq!(body["data"]["modelAliases"]["codex-fast"], "gpt-5.5");
    assert_eq!(body["data"]["modelAccountRoutes"], json!({}));
    assert_eq!(body["data"]["refreshMarginSeconds"], 240);
    assert_eq!(body["data"]["refreshConcurrency"], 2);
    assert_eq!(body["data"]["rotationStrategy"], "least_used");
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
async fn admin_settings_update_should_persist_runtime_settings_to_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-update.sqlite");
    let url = format!("sqlite://{}", db.display());
    let config = test_config(url);
    let pool = connect_sqlite(&config.database.url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    seed_account(&pool, "acct_route_a").await;
    seed_account(&pool, "acct_route_b").await;
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Services::new(&config, stores, fingerprint);
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
                        "modelAliases": {
                            "gpt-5.2": "gpt-5.5",
                            "claude-sonnet": "gpt-5.5"
                        },
                        "modelAccountRoutes": {
                            "gpt-5.5": ["acct_route_a", "acct_route_b"]
                        },
                        "refreshMarginSeconds": 900,
                        "refreshConcurrency": 4,
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
    assert_eq!(body["data"]["modelAliases"]["gpt-5.2"], "gpt-5.5");
    assert_eq!(
        body["data"]["modelAccountRoutes"]["gpt-5.5"][0],
        "acct_route_a"
    );
    assert_eq!(body["data"]["refreshMarginSeconds"], 900);
    assert_eq!(body["data"]["refreshConcurrency"], 4);

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

    let row: (String, String, i64, i64, i64, i64, String) = sqlx::query_as(
        "select default_model, model_aliases_json, refresh_margin_seconds, refresh_concurrency, max_concurrent_per_account, request_interval_ms, rotation_strategy from runtime_settings where id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let aliases: BTreeMap<String, String> = serde_json::from_str(&row.1).unwrap();
    assert_eq!(row.0, "gpt-6");
    assert_eq!(aliases["gpt-5.2"], "gpt-5.5");
    assert_eq!(aliases["claude-sonnet"], "gpt-5.5");
    assert_eq!(row.2, 900);
    assert_eq!(row.3, 4);
    assert_eq!(row.4, 7);
    assert_eq!(row.5, 80);
    assert_eq!(row.6, "round_robin");
    let route_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "select model, account_id, priority from model_account_routes order by model, priority",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        route_rows,
        vec![
            ("gpt-5.5".to_string(), "acct_route_a".to_string(), 0),
            ("gpt-5.5".to_string(), "acct_route_b".to_string(), 1),
        ]
    );
    assert!(!dir.path().join("config.yaml").exists());

    let restarted_config = RuntimeSettingsService::load_or_initialize_config(config.clone(), &pool)
        .await
        .unwrap();
    assert_eq!(restarted_config.model.default_model, "gpt-6");
    assert_eq!(restarted_config.model.aliases["gpt-5.2"], "gpt-5.5");
    assert_eq!(
        restarted_config.model.account_routes["gpt-5.5"],
        vec!["acct_route_a".to_string(), "acct_route_b".to_string()]
    );
    assert_eq!(restarted_config.auth.refresh_margin_seconds, 900);
    assert_eq!(restarted_config.auth.refresh_concurrency, 4);
    assert_eq!(restarted_config.auth.rotation_strategy, "round_robin");
    assert_eq!(restarted_config.auth.max_concurrent_per_account, 7);
    assert_eq!(restarted_config.auth.request_interval_ms, 80);
    assert_eq!(
        restarted_config.model.default_reasoning_effort,
        config.model.default_reasoning_effort
    );
    assert_eq!(restarted_config.database.url, config.database.url);
}

#[tokio::test]
async fn admin_settings_update_should_reject_unsupported_or_invalid_fields() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-update-invalid.sqlite");
    let url = format!("sqlite://{}", db.display());
    let config = test_config(url);
    let pool = connect_sqlite(&config.database.url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
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
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
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

async fn seed_account(pool: &SqlitePool, account_id: &str) {
    sqlx::query(
        r"
insert into accounts (
  id,
  access_token,
  status,
  added_at,
  updated_at
) values (?, ?, 'active', ?, ?)",
    )
    .bind(account_id)
    .bind("access-token")
    .bind("2026-06-18T00:00:00Z")
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
            account_routes: BTreeMap::new(),
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
