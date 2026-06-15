use std::{collections::BTreeMap, fs, path::Path};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use codex_proxy_rs::{
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    platform::storage::db::connect_sqlite,
    runtime::build_router,
    runtime::state::AppState,
};

use crate::support::{response_json, seed_admin_session};

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
            enabled: true,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

fn write_config_yaml(path: &Path, config: &AppConfig) {
    fs::write(
        path.join("config.yaml"),
        serde_yml::to_string(config).unwrap(),
    )
    .unwrap();
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
                .uri("/api/admin/settings")
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
async fn admin_settings_patch_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-patch-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("x-request-id", "req_settings_patch_auth")
                .body(Body::from(r#"{"defaultModel":"gpt-5.5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_settings_patch_auth");
}

#[tokio::test]
async fn admin_settings_patch_should_persist_retained_fields_to_local_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-patch.sqlite");
    let url = format!("sqlite://{}", db.display());
    let config = test_config(url);
    write_config_yaml(dir.path(), &config);
    let pool = connect_sqlite(&config.database.url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_local_config_path(
        config,
        pool,
        dir.path().join("local.yaml"),
    ));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_patch")
                .body(Body::from(
                    json!({
                        "defaultModel": "gpt-6",
                        "defaultReasoningEffort": "medium",
                        "serviceTier": "priority",
                        "modelAliases": { "fast": "gpt-6-fast" },
                        "refreshEnabled": false,
                        "refreshMarginSeconds": 180,
                        "refreshConcurrency": 3,
                        "maxConcurrentPerAccount": 5,
                        "requestIntervalMs": 125,
                        "rotationStrategy": "round_robin",
                        "tierPriority": ["pro", "plus"],
                        "quotaRefreshIntervalMinutes": 15,
                        "quotaWarningThresholds": {
                            "primary": [75, 90],
                            "secondary": [65, 95]
                        },
                        "quotaSkipExhausted": false,
                        "logsEnabled": false,
                        "logsCapacity": 3000,
                        "logsCaptureBody": true,
                        "usageHistoryRetentionDays": 60
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_settings_patch");
    assert_eq!(body["data"]["defaultModel"], "gpt-6");
    assert_eq!(body["data"]["rotationStrategy"], "round_robin");
    assert_eq!(
        body["data"]["quotaWarningThresholds"]["primary"],
        json!([75, 90])
    );

    let reloaded = AppConfig::load_from_dir(dir.path()).unwrap();
    assert_eq!(reloaded.model.default_model, "gpt-6");
    assert_eq!(
        reloaded.model.aliases.get("fast").map(String::as_str),
        Some("gpt-6-fast")
    );
    assert!(!reloaded.auth.refresh_enabled);
    assert_eq!(reloaded.auth.rotation_strategy, "round_robin");
    assert_eq!(reloaded.quota.warning_thresholds.primary, vec![75, 90]);
    assert_eq!(reloaded.usage_stats.history_retention_days, Some(60));

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
    assert_eq!(get_response.status(), StatusCode::OK);
    let body = response_json(get_response).await;
    assert_eq!(body["data"]["defaultModel"], "gpt-6");
}

#[tokio::test]
async fn admin_settings_patch_should_reject_unsupported_or_invalid_fields() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-patch-invalid.sqlite");
    let url = format!("sqlite://{}", db.display());
    let config = test_config(url);
    write_config_yaml(dir.path(), &config);
    let pool = connect_sqlite(&config.database.url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_local_config_path(
        config,
        pool,
        dir.path().join("local.yaml"),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_patch_invalid")
                .body(Body::from(
                    json!({
                        "unknownSetting": true,
                        "rotationStrategy": "random"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40001);
    assert_eq!(body["requestId"], "req_settings_patch_invalid");
    assert!(!dir.path().join("local.yaml").exists());
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
                .uri("/api/admin/settings")
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
}
