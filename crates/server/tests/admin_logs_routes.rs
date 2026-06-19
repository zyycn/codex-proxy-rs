use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use codex_proxy_adapters::sqlite::events::SqliteEventLogStore;
use codex_proxy_core::events::model::{EventLevel, EventLog};
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
use serde_json::Value;
use sqlx::SqlitePool;
use tower::util::ServiceExt;

#[tokio::test]
async fn admin_logs_should_require_admin_session_cookie() {
    let (app, _store, _dir) = admin_logs_test_app("admin-logs-auth.sqlite").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs")
                .header("x-request-id", "req_logs_auth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_logs_auth");
}

#[tokio::test]
async fn admin_logs_should_cursor_page_events_and_include_request_id() {
    let (app, store, _dir) = admin_logs_test_app("admin-logs-cursor.sqlite").await;
    let now = Utc::now();
    let mut older = EventLog::new("request", EventLevel::Info, "older");
    older.id = "log_older".to_string();
    older.created_at = now;
    store.append(&older).await.unwrap();
    let mut newer = EventLog::new("request", EventLevel::Info, "newer");
    newer.id = "log_newer".to_string();
    newer.created_at = now + Duration::seconds(1);
    store.append(&newer).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_cursor")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_logs_cursor");
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"][0]["id"], "log_newer");
    assert_eq!(body["page"]["limit"], 1);
    assert!(body["page"]["nextCursor"].is_string());
}

#[tokio::test]
async fn admin_logs_should_filter_and_cursor_page_events() {
    let (app, store, _dir) = admin_logs_test_app("admin-logs.sqlite").await;
    let mut matching = EventLog::new("request", EventLevel::Error, "upstream timeout");
    matching.id = "log_matching".to_string();
    matching.route = Some("/v1/responses".to_string());
    store.append(&matching).await.unwrap();
    store
        .append(&EventLog::new(
            "request",
            EventLevel::Info,
            "upstream timeout",
        ))
        .await
        .unwrap();
    store
        .append(&EventLog::new(
            "account",
            EventLevel::Error,
            "upstream timeout",
        ))
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?kind=request&level=error&search=timeout&limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_filter")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requestId"], "req_logs_filter");
    assert_eq!(body["data"][0]["id"], "log_matching");
    assert_eq!(body["data"][0]["route"], "/v1/responses");
}

#[tokio::test]
async fn admin_logs_should_reject_unsupported_level_filter() {
    let (app, _store, _dir) = admin_logs_test_app("admin-logs-invalid-level.sqlite").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?level=verbose")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_invalid_level")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40001);
    assert_eq!(body["requestId"], "req_logs_invalid_level");
}

#[tokio::test]
async fn admin_logs_should_return_state_detail_and_clear_events() {
    let (app, store, _dir) = admin_logs_test_app("admin-logs-state.sqlite").await;
    let mut event = EventLog::new("request", EventLevel::Warn, "detail");
    event.id = "log_detail".to_string();
    event.request_id = Some("req_upstream".to_string());
    store.append(&event).await.unwrap();

    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs/log_detail")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail_status = detail.status();
    let detail_body = response_json(detail).await;

    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail_body["data"]["requestId"], "req_upstream");
    assert_eq!(detail_body["data"]["level"], "warn");

    let state = app
        .clone()
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
    let state_body = response_json(state).await;
    assert_eq!(state_body["data"]["storedCount"], 1);
    assert_eq!(state_body["data"]["enabled"], false);

    let cleared = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/logs")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cleared_body = response_json(cleared).await;
    assert_eq!(cleared_body["data"]["cleared"], 1);

    let empty = app
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
    let empty_body = response_json(empty).await;
    assert_eq!(empty_body["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn admin_logs_detail_should_return_not_found_for_missing_event() {
    let (app, _store, _dir) = admin_logs_test_app("admin-logs-detail-missing.sqlite").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs/missing")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], 40401);
    assert_eq!(body["message"], "Log event not found");
    assert_eq!(body["requestId"], "req_logs_missing");
}

#[tokio::test]
async fn admin_logs_state_patch_should_require_admin_session_cookie() {
    let (app, _store, _dir) = admin_logs_test_app("admin-logs-state-auth.sqlite").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/logs/state")
                .header("content-type", "application/json")
                .header("x-request-id", "req_logs_state_auth")
                .body(Body::from(
                    serde_json::json!({ "enabled": true }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_logs_state_auth");
}

#[tokio::test]
async fn admin_logs_should_update_state_and_trim_to_capacity() {
    let (app, store, _dir) = admin_logs_test_app("admin-logs-update-state.sqlite").await;
    for index in 0..3 {
        let mut event = EventLog::new("request", EventLevel::Info, format!("event {index}"));
        event.id = format!("log_capacity_{index}");
        event.created_at = Utc::now() + Duration::seconds(index);
        store.append(&event).await.unwrap();
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/logs/state")
                .header("cookie", "cpr_admin_session=session_1")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "capacity": 2,
                        "captureBody": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, StatusCode::OK);
    let body = response_json(response).await;
    let remaining = store
        .list(Default::default(), None, 10)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|event| event.id)
        .collect::<Vec<_>>();

    assert_eq!(body["data"]["enabled"], true);
    assert_eq!(body["data"]["capacity"], 2);
    assert_eq!(body["data"]["captureBody"], true);
    assert_eq!(body["data"]["storedCount"], 2);
    assert_eq!(remaining, vec!["log_capacity_2", "log_capacity_1"]);
}

#[tokio::test]
async fn admin_logs_state_patch_should_reject_zero_capacity() {
    let (app, _store, _dir) = admin_logs_test_app("admin-logs-zero-capacity.sqlite").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/logs/state")
                .header("cookie", "cpr_admin_session=session_1")
                .header("content-type", "application/json")
                .header("x-request-id", "req_logs_zero_capacity")
                .body(Body::from(serde_json::json!({ "capacity": 0 }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40001);
    assert_eq!(body["requestId"], "req_logs_zero_capacity");
}

async fn admin_logs_test_app(
    db_name: &str,
) -> (axum::Router, SqliteEventLogStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        SecretBox::new([73u8; 32]),
        ApiKeyHasher::new([74u8; 32]),
    );
    (
        router::router().with_state(state),
        SqliteEventLogStore::new(pool),
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
