use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use codex_proxy_rs::{
    access::admin_session::SqliteAdminSessionStore,
    access::client_keys::SqliteClientKeyStore,
    accounts::{
        model::AccountStatus,
        store::{NewAccount, SqliteAccountStore, SqliteCookieStore},
        token_refresh::RefreshLeaseStore,
    },
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
use secrecy::SecretString;
use serde_json::Value;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn debug_diagnostics_should_return_runtime_pool_transport_paths_and_fingerprint() {
    let (app, _dir) = test_app_with_account(
        "https://chatgpt.test/backend-api".to_string(),
        diagnostics_fingerprint(),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/diagnostics")
                .header("x-forwarded-for", "127.0.0.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;

    assert_eq!(body["status"], "ok");
    assert_eq!(
        body["transport"]["backendBaseUrl"],
        "https://chatgpt.test/backend-api"
    );
}

#[tokio::test]
async fn debug_fingerprint_should_return_runtime_fingerprint_summary() {
    let (app, _dir) = test_app_with_account(
        "https://chatgpt.test/backend-api".to_string(),
        diagnostics_fingerprint(),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/fingerprint")
                .header("x-forwarded-for", "127.0.0.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;

    assert_eq!(body["source"], "runtime");
}

#[tokio::test]
async fn debug_models_should_return_catalog_debug_without_client_api_key() {
    let (app, _dir) = test_app_with_account(
        "https://chatgpt.test/backend-api".to_string(),
        diagnostics_fingerprint(),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/models")
                .header("x-forwarded-for", "127.0.0.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]

async fn debug_upstream_should_probe_codex_models_endpoint_without_returning_secrets() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .and(header("originator", "Codex Desktop"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(
                serde_json::json!({"error": {"message": "missing or invalid token"}}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (app, _dir) = test_app_with_account(server.uri(), diagnostics_fingerprint()).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/upstream")
                .header("x-request-id", "req_debug_probe")
                .header("x-forwarded-for", "127.0.0.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["target"], "codexModels");
    assert_eq!(body["reachable"], true);
    assert_eq!(body["statusCode"], 401);
}

#[tokio::test]
async fn debug_endpoints_should_reject_forwarded_remote_requests_without_probe() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let (app, _dir) = test_app_with_account(server.uri(), diagnostics_fingerprint()).await;

    let diagnostics = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/diagnostics")
                .header("x-forwarded-for", "203.0.113.10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(diagnostics.status(), StatusCode::FORBIDDEN);
}

async fn test_app_with_account(
    base_url: String,
    fingerprint: Fingerprint,
) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("openai-diagnostics.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([77u8; 32]);
    let store = SqliteAccountStore::new(pool.clone(), secret_box.clone());
    store
        .insert(NewAccount {
            id: "acct_1".to_string(),
            email: Some("acct@example.test".to_string()),
            account_id: Some("chatgpt-account".to_string()),
            user_id: Some("user_1".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-secret".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-secret".to_string().into())),
            access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
            status: AccountStatus::Active,
            added_at: None,
        })
        .await
        .unwrap();
    let config = test_config(url, base_url);
    let hasher = ApiKeyHasher::new([79u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: store.clone(),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), secret_box.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    state
        .services
        .account_pool
        .restore_from_repository()
        .await
        .unwrap();
    (
        codex_proxy_rs::http::router::router().with_state(state),
        dir,
    )
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn diagnostics_fingerprint() -> Fingerprint {
    Fingerprint {
        originator: "Codex Desktop".to_string(),
        app_version: "27.100.200".to_string(),
        build_number: "9001".to_string(),
        platform: "linux".to_string(),
        arch: "x64".to_string(),
        chromium_version: "147".to_string(),
        user_agent_template: "Codex Desktop/{version} ({platform}; {arch})".to_string(),
        default_headers: Fingerprint::default_headers(),
        header_order: Fingerprint::default_header_order(),
        updated_at: None,
    }
}

fn test_config(database_url: String, base_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig { base_url },
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
