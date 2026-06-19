use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use codex_proxy_adapters::sqlite::{
    accounts::{NewAccount, SqliteAccountStore},
    models::ModelSnapshotRepository,
};
use codex_proxy_core::{accounts::model::AccountStatus, gateway::fingerprint::Fingerprint};
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
use secrecy::SecretString;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn admin_refresh_models_should_require_admin_session() {
    let upstream = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-models-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url, upstream.uri()),
        pool,
        SecretBox::new([81u8; 32]),
        ApiKeyHasher::new([82u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/refresh-models")
                .header("x-request-id", "req_models_auth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_models_auth");
}

#[tokio::test]
async fn admin_refresh_models_should_store_snapshots_for_distinct_active_plans() {
    let upstream = MockServer::start().await;
    let fingerprint = models_test_fingerprint();
    let expected_user_agent = fingerprint.user_agent();
    let expected_sec_ch_ua = fingerprint.sec_ch_ua();
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{
                "slug": "gpt-refresh",
                "display_name": "GPT Refresh",
                "description": "Backend refreshed model",
                "default_reasoning_level": "minimal",
                "supported_reasoning_levels": [{
                    "effort": "minimal",
                    "description": "Minimal"
                }],
                "contextWindow": 200000
            }]
        })))
        .expect(2)
        .mount(&upstream)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-models-refresh.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([83u8; 32]);
    let account_store = SqliteAccountStore::new(pool.clone(), secret_box.clone());
    seed_account(&account_store, "acct_plus_a", "plus", "access-plus-a").await;
    seed_account(&account_store, "acct_plus_b", "plus", "access-plus-b").await;
    seed_account(&account_store, "acct_team", "team", "access-team").await;
    let state = AppState::with_pool_secret_api_key_hasher_and_fingerprint(
        test_config(url, upstream.uri()),
        pool.clone(),
        secret_box,
        ApiKeyHasher::new([84u8; 32]),
        fingerprint,
    );
    let generated = state
        .services
        .admin_client_keys
        .create("catalog-reader")
        .await
        .unwrap();
    let app = router::router().with_state(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/refresh-models")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_models_refresh")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let snapshots = ModelSnapshotRepository::new(pool)
        .list_plan_snapshots()
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requestId"], "req_models_refresh");
    assert_eq!(body["data"]["refreshedPlans"], 2);
    assert_eq!(body["data"]["modelCount"], 2);
    assert_eq!(body["data"]["failedPlans"], 0);
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].models[0].id, "gpt-refresh");
    let requests = upstream.received_requests().await.unwrap();
    let model_requests = requests
        .iter()
        .filter(|request| request.url.path() == "/codex/models")
        .collect::<Vec<_>>();
    assert_eq!(model_requests.len(), 2);
    assert!(model_requests.iter().all(|request| {
        request
            .headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            == Some(expected_user_agent.as_str())
    }));
    assert!(model_requests.iter().all(|request| {
        request
            .headers
            .get("sec-ch-ua")
            .and_then(|value| value.to_str().ok())
            == Some(expected_sec_ch_ua.as_str())
    }));

    let catalog_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models/catalog")
                .header("authorization", format!("Bearer {}", generated.plaintext))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let catalog = response_json(catalog_response).await;
    assert_eq!(catalog[0]["id"], "gpt-refresh");
    assert_eq!(catalog[0]["source"], "backend");
}

fn models_test_fingerprint() -> Fingerprint {
    Fingerprint {
        app_version: "27.333.444".to_string(),
        build_number: "9002".to_string(),
        chromium_version: "156".to_string(),
        ..Fingerprint::default_for_tests()
    }
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

async fn seed_account(
    account_store: &SqliteAccountStore,
    id: &str,
    plan_type: &str,
    access_token: &str,
) {
    account_store
        .insert(NewAccount {
            id: id.to_string(),
            email: None,
            account_id: Some(format!("chatgpt-{id}")),
            user_id: None,
            label: None,
            plan_type: Some(plan_type.to_string()),
            access_token: SecretString::new(access_token.to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        })
        .await
        .unwrap();
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
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
