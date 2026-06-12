use std::collections::BTreeMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
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
        admin: AdminConfig {
            session_ttl_minutes: 1440,
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            max_file_bytes: 10_485_760,
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

#[tokio::test]
async fn admin_usage_stats_should_cursor_page_account_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage-stats.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    seed_usage(&pool).await;
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/usage-stats?limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_json(first_response).await;
    assert_eq!(first_body["code"], 200);
    assert_eq!(first_body["requestId"], "req_usage");
    assert_eq!(first_body["data"].as_array().unwrap().len(), 1);
    assert_eq!(first_body["data"][0]["accountId"], "acct_b");
    assert_eq!(first_body["data"][0]["requestCount"], 2);
    assert_eq!(first_body["data"][0]["inputTokens"], 7);
    assert_eq!(first_body["page"]["limit"], 1);
    let cursor = first_body["page"]["nextCursor"].as_str().unwrap();

    let second_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/admin/usage-stats?limit=1&cursor={cursor}"))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_json(second_response).await;
    assert_eq!(second_body["data"][0]["accountId"], "acct_a");
    assert!(second_body["page"]["nextCursor"].is_null());
}

#[tokio::test]
async fn admin_usage_stats_summary_should_return_usage_totals() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage-stats.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    seed_usage(&pool).await;
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/usage-stats/summary")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["data"]["accountCount"], 2);
    assert_eq!(body["data"]["requestCount"], 5);
    assert_eq!(body["data"]["inputTokens"], 19);
    assert_eq!(body["data"]["outputTokens"], 8);
    assert_eq!(body["data"]["cachedTokens"], 3);
}

#[tokio::test]
async fn admin_usage_stats_should_reject_missing_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage-stats.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let app = build_router(AppState::with_pool(test_config(url), pool));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/usage-stats")
                .header("x-request-id", "req_usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_usage");
}

async fn seed_usage(pool: &sqlx::SqlitePool) {
    let now = Utc::now().to_rfc3339();
    for (id, email, label, plan_type) in [
        ("acct_a", "a@example.com", "primary", "plus"),
        ("acct_b", "b@example.com", "backup", "free"),
    ] {
        sqlx::query(
            "insert into accounts (id, email, label, plan_type, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, 'active', ?, ?)",
        )
        .bind(id)
        .bind(email)
        .bind(label)
        .bind(plan_type)
        .bind("encrypted")
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_a")
    .bind(3_i64)
    .bind(12_i64)
    .bind(5_i64)
    .bind(1_i64)
    .bind("2026-06-11T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_b")
    .bind(2_i64)
    .bind(7_i64)
    .bind(3_i64)
    .bind(2_i64)
    .bind("2026-06-11T00:01:00Z")
    .execute(pool)
    .await
    .unwrap();
}
