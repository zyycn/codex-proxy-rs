use std::collections::BTreeMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use codex_proxy_rs::{
    codex::events::{
        event::{EventLevel, EventLog},
        repository::EventLogRepository,
    },
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
async fn admin_logs_are_cursor_paginated_and_include_request_id() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-logs.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let repo = EventLogRepository::new(pool.clone());
    repo.insert(EventLog::new("request", EventLevel::Info, "first"))
        .await
        .unwrap();
    repo.insert(EventLog::new("request", EventLevel::Info, "second"))
        .await
        .unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_admin");
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["page"]["limit"], 1);
    assert!(body["page"]["nextCursor"].is_string());
}

#[tokio::test]
async fn admin_logs_should_filter_by_kind_level_and_search_text() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-logs-filter.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let repo = EventLogRepository::new(pool.clone());
    let mut matching = EventLog::new("request", EventLevel::Error, "upstream timeout");
    matching.id = "log_matching".to_string();
    matching.route = Some("/v1/responses".to_string());
    repo.insert(matching).await.unwrap();
    repo.insert(EventLog::new(
        "request",
        EventLevel::Info,
        "upstream timeout",
    ))
    .await
    .unwrap();
    repo.insert(EventLog::new(
        "account",
        EventLevel::Error,
        "upstream timeout",
    ))
    .await
    .unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?kind=request&level=error&search=timeout")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_filter")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["requestId"], "req_logs_filter");
    let items = body["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "log_matching");
    assert_eq!(items[0]["route"], "/v1/responses");
}

#[tokio::test]
async fn admin_logs_reject_missing_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-logs.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs")
                .header("x-request-id", "req_admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_admin");
}

#[tokio::test]
async fn admin_logs_state_should_return_runtime_logging_state_and_stored_count() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-logs-state.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let repo = EventLogRepository::new(pool.clone());
    repo.insert(EventLog::new("request", EventLevel::Info, "first"))
        .await
        .unwrap();
    repo.insert(EventLog::new("request", EventLevel::Warn, "second"))
        .await
        .unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs/state")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_state")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_logs_state");
    assert_eq!(body["data"]["enabled"], false);
    assert_eq!(body["data"]["captureBody"], false);
    assert_eq!(body["data"]["capacity"], 2000);
    assert_eq!(body["data"]["storedCount"], 2);
}

#[tokio::test]
async fn admin_logs_state_patch_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-logs-state-patch-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/logs/state")
                .header("content-type", "application/json")
                .header("x-request-id", "req_logs_state_patch")
                .body(Body::from(
                    serde_json::json!({ "enabled": true }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_logs_state_patch");
}

#[tokio::test]
async fn admin_logs_state_patch_should_update_runtime_logging_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-logs-state-patch.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/logs/state")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_state_patch")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "captureBody": true,
                        "capacity": 512
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
    assert_eq!(body["requestId"], "req_logs_state_patch");
    assert_eq!(body["data"]["enabled"], true);
    assert_eq!(body["data"]["captureBody"], true);
    assert_eq!(body["data"]["capacity"], 512);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs/state")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response_json(response).await;
    assert_eq!(body["data"]["enabled"], true);
    assert_eq!(body["data"]["captureBody"], true);
    assert_eq!(body["data"]["capacity"], 512);
}

#[tokio::test]
async fn admin_logs_detail_should_return_one_event_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-logs-detail.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let repo = EventLogRepository::new(pool.clone());
    let mut event = EventLog::new("request", EventLevel::Error, "detail");
    event.id = "log_detail".to_string();
    event.request_id = Some("req_upstream".to_string());
    repo.insert(event).await.unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs/log_detail")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_detail")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_logs_detail");
    assert_eq!(body["data"]["id"], "log_detail");
    assert_eq!(body["data"]["requestId"], "req_upstream");
    assert_eq!(body["data"]["level"], "error");
    assert_eq!(body["data"]["message"], "detail");
}

#[tokio::test]
async fn admin_logs_clear_should_delete_stored_events() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-logs-clear.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let repo = EventLogRepository::new(pool.clone());
    repo.insert(EventLog::new("request", EventLevel::Info, "first"))
        .await
        .unwrap();
    repo.insert(EventLog::new("request", EventLevel::Info, "second"))
        .await
        .unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/logs")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_clear")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_logs_clear");
    assert_eq!(body["data"]["cleared"], 2);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?limit=50")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
}
