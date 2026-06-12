use std::collections::BTreeMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use codex_proxy_rs::{
    app::build_router,
    app::state::AppState,
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    storage::db::connect_sqlite,
};

mod common;

use common::{response_json, seed_admin_session};

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
        admin: AdminConfig {
            session_ttl_minutes: 1440,
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            max_file_bytes: 10_485_760,
            retention_days: 14,
            enabled: true,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

#[tokio::test]
async fn admin_settings_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/settings")
                .header("x-request-id", "req_settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_settings");
}

#[tokio::test]
async fn admin_settings_should_return_only_in_scope_runtime_fields() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/settings")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_settings");
    assert_eq!(
        body["data"],
        json!({
            "defaultModel": "gpt-5.5",
            "defaultReasoningEffort": "high",
            "serviceTier": "flex",
            "modelAliases": { "codex-fast": "gpt-5.5" },
            "refreshEnabled": true,
            "refreshMarginSeconds": 240,
            "refreshConcurrency": 2,
            "maxConcurrentPerAccount": 4,
            "requestIntervalMs": 50,
            "rotationStrategy": "least_used",
            "tierPriority": ["team", "plus"],
            "quotaRefreshIntervalMinutes": 5,
            "quotaWarningThresholds": {
                "primary": [80, 90],
                "secondary": [70, 95]
            },
            "quotaSkipExhausted": true,
            "logsEnabled": true,
            "logsCapacity": 2000,
            "logsCaptureBody": false,
            "usageHistoryRetentionDays": 30
        })
    );
    for removed_key in [
        "proxyUrl",
        "openaiApiKey",
        "ollama",
        "provider",
        "proxyApiKey",
        "autoUpdate",
    ] {
        assert!(
            body["data"].get(removed_key).is_none(),
            "{removed_key} must stay out of the Rust settings API"
        );
    }
}
