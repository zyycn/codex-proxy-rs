use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use tower::ServiceExt;

use codex_proxy_rs::{
    codex::accounts::models::{
        catalog::{BackendModelEntry, ModelPlanSnapshot},
        repository::ModelSnapshotRepository,
    },
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    platform::crypto::SecretBox,
    platform::identity::{api_key::ApiKeyHasher, api_key_repository::ClientApiKeyRepository},
    platform::storage::db::connect_sqlite,
    runtime::build_router,
    runtime::state::AppState,
};

fn test_config() -> AppConfig {
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
            default_reasoning_effort: None,
            service_tier: None,
            aliases,
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
        database: DatabaseConfig {
            url: "sqlite://:memory:".to_string(),
        },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig {
            force_http11: false,
        },
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

fn test_config_with_database(database_url: String) -> AppConfig {
    let mut config = test_config();
    config.database.url = database_url;
    config
}

async fn test_app_with_client_api_key() -> (Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("routes-responses.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let hasher = ApiKeyHasher::new([52u8; 32]);
    let generated = hasher.generate_client_api_key("test");
    ClientApiKeyRepository::new(pool.clone())
        .insert_generated("test", &generated)
        .await
        .unwrap();
    let app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config_with_database(url),
        pool,
        SecretBox::new([53u8; 32]),
        hasher,
    ));
    (app, generated.plaintext, dir)
}

async fn test_app_with_backend_model_snapshot() -> (Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("routes-models.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let hasher = ApiKeyHasher::new([54u8; 32]);
    let generated = hasher.generate_client_api_key("test");
    ClientApiKeyRepository::new(pool.clone())
        .insert_generated("test", &generated)
        .await
        .unwrap();
    let snapshot = ModelPlanSnapshot::from_backend_entries(
        "plus",
        vec![BackendModelEntry {
            id: Some("gpt-6".to_string()),
            name: Some("GPT-6".to_string()),
            ..BackendModelEntry::default()
        }],
    );
    ModelSnapshotRepository::new(pool.clone())
        .replace_plan_snapshot(&snapshot)
        .await
        .unwrap();
    let app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config_with_database(url),
        pool,
        SecretBox::new([55u8; 32]),
        hasher,
    ));
    (app, generated.plaintext, dir)
}

#[tokio::test]
async fn v1_requires_client_api_key_not_admin_cookie() {
    let app = build_router(AppState::new(test_config()));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn responses_route_rejects_non_codex_provider_models() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"claude-3","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v1_response_has_request_id_header_without_admin_body_field() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("x-request-id", "req_client")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.headers().get("x-request-id").unwrap(),
        "req_client"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body.get("requestId").is_none());
}

#[tokio::test]
async fn models_route_returns_openai_compatible_codex_model_list() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["object"], "list");
    assert_eq!(body["data"][0]["object"], "model");
    assert_eq!(body["data"][0]["id"], "gpt-5.5");
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn model_catalog_route_returns_codex_metadata_without_alias_entries() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models/catalog")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], "gpt-5.5");
    assert_eq!(body[0]["isDefault"], true);
    assert_eq!(body[0]["defaultReasoningEffort"], "medium");
    assert_eq!(
        body[0]["supportedReasoningEfforts"][0]["reasoningEffort"],
        "low"
    );
}

#[tokio::test]
async fn model_catalog_route_returns_cached_backend_models() {
    let (app, api_key, _dir) = test_app_with_backend_model_snapshot().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models/catalog")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], "gpt-6");
    assert_eq!(body[0]["source"], "backend");
}

#[tokio::test]
async fn model_detail_route_returns_openai_model_for_known_model() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models/gpt-5.5")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["id"], "gpt-5.5");
    assert_eq!(body["object"], "model");
}

#[tokio::test]
async fn model_detail_route_rejects_unknown_model_with_openai_error() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models/not-a-codex-model")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "model_not_found");
}

#[tokio::test]
async fn model_info_route_returns_extended_catalog_entry() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models/gpt-5.5/info")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["id"], "gpt-5.5");
    assert_eq!(body["outputModalities"], json!(["text"]));
}

#[tokio::test]
async fn debug_models_route_returns_model_store_summary() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/models")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["totalModels"], 1);
    assert_eq!(body["aliasCount"], 1);
}
