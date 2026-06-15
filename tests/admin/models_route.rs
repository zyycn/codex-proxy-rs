use std::collections::BTreeMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use secrecy::SecretString;
use serde_json::json;
use tower::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    codex::accounts::{
        model::{Account, AccountStatus},
        repository::{AccountRepository, NewAccount},
    },
    codex::gateway::fingerprint::model::Fingerprint,
    codex::models::repository::ModelSnapshotRepository,
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    platform::crypto::SecretBox,
    platform::identity::{client_key::ApiKeyHasher, client_key_repository::ClientApiKeyRepository},
    platform::storage::db::connect_sqlite,
    runtime::build_router,
    runtime::state::AppState,
};

use crate::support::{response_json, seed_admin_session};

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
        ws_pool: Default::default(),
        admin: AdminConfig {
            session_ttl_minutes: 1440,
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
async fn admin_refresh_models_should_require_admin_session_cookie() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-models.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url, server.uri()),
        pool,
        SecretBox::new([31u8; 32]),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/refresh-models")
                .header("x-request-id", "req_refresh_models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_refresh_models");
}

#[tokio::test]
async fn admin_refresh_models_should_fetch_snapshots_for_distinct_account_plans() {
    let server = MockServer::start().await;
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
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-models.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([32u8; 32]);
    let hasher = ApiKeyHasher::new([33u8; 32]);
    let generated = hasher.generate_client_api_key("test");
    ClientApiKeyRepository::new(pool.clone())
        .insert_generated("test", &generated)
        .await
        .unwrap();
    let account_repo = AccountRepository::new(pool.clone(), secret_box.clone());
    insert_account(&account_repo, "acct_plus_a", "plus", "access-plus-a").await;
    insert_account(&account_repo, "acct_plus_b", "plus", "access-plus-b").await;
    insert_account(&account_repo, "acct_team", "team", "access-team").await;
    let state = AppState::with_pool_secret_api_key_hasher_and_fingerprint(
        test_config(url, server.uri()),
        pool.clone(),
        secret_box,
        hasher,
        fingerprint,
    );
    state
        .services
        .accounts
        .insert_runtime_account(pool_account("acct_aaa_free", "free"))
        .await;
    state
        .services
        .accounts
        .insert_runtime_account(pool_account("acct_plus_a", "plus"))
        .await;
    state
        .services
        .accounts
        .insert_runtime_account(pool_account("acct_team", "team"))
        .await;
    let app = build_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/refresh-models")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_refresh_models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["refreshedPlans"], 2);
    assert_eq!(body["data"]["modelCount"], 2);
    assert_eq!(body["data"]["failedPlans"], 0);
    let snapshots = ModelSnapshotRepository::new(pool.clone())
        .list_plan_snapshots()
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].models[0].id, "gpt-refresh");
    let requests = server.received_requests().await.unwrap();
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

    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-refresh")
        .await
        .unwrap();
    assert_ne!(acquired.plan_type.as_deref(), Some("free"));
}

fn models_test_fingerprint() -> Fingerprint {
    Fingerprint {
        app_version: "27.333.444".to_string(),
        build_number: "9002".to_string(),
        chromium_version: "156".to_string(),
        ..Fingerprint::default_for_tests()
    }
}

async fn insert_account(
    repo: &AccountRepository,
    id: &'static str,
    plan_type: &'static str,
    token: &'static str,
) {
    repo.insert(NewAccount {
        id: id.to_string(),
        email: None,
        account_id: Some(format!("{id}-chatgpt")),
        user_id: None,
        label: None,
        plan_type: Some(plan_type.to_string()),
        access_token: SecretString::new(token.to_string().into()),
        refresh_token: Some(SecretString::new(format!("refresh-{id}").into())),
        access_token_expires_at: None,
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
}

fn pool_account(id: &str, plan_type: &str) -> Account {
    let mut account = Account::test(id, AccountStatus::Active);
    account.plan_type = Some(plan_type.to_string());
    account
}
