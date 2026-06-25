use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use codex_proxy_rs::{
    admin::{
        auth::service::SqliteAdminSessionStore,
        keys::service::SqliteClientKeyStore,
        monitoring::{
            event_store::SqliteEventLogStore,
            events::{EventLevel, EventLog},
        },
    },
    infra::{
        crypto::SecretBox, database::connect_sqlite, identity::ApiKeyHasher, time::china_day_start,
    },
    proxy::dispatch::session_affinity::SqliteSessionAffinityStore,
    runtime::{
        services::{BackgroundTaskStores, Services},
        state::AppState,
    },
    upstream::{
        accounts::{
            cookies::SqliteCookieStore, store::SqliteAccountStore, token_refresh::RefreshLeaseStore,
        },
        fingerprint::{Fingerprint, FingerprintRepository},
    },
};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tower::util::ServiceExt;

#[tokio::test]
async fn dashboard_summary_should_render_fingerprint_updated_at_as_beijing_time() {
    let (app, _store, _pool, _dir) = dashboard_test_app(
        "dashboard-fingerprint-updated.sqlite",
        crate::support::fingerprint::test_fingerprint_with_updated_at(Some(
            "2026-06-23T11:36:46.965574590+00:00",
        )),
    )
    .await;

    let body = dashboard_summary(app).await;

    assert_eq!(
        service_status_value(&body, "更新时间"),
        "2026-06-23 19:36:46"
    );
}

#[tokio::test]
async fn dashboard_summary_should_render_dash_when_fingerprint_updated_at_is_missing() {
    let (app, _store, _pool, _dir) = dashboard_test_app(
        "dashboard-fingerprint-missing.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;

    let body = dashboard_summary(app).await;

    assert_eq!(service_status_value(&body, "更新时间"), "-");
}

#[tokio::test]
async fn dashboard_summary_should_calculate_today_traffic_and_latency_from_event_logs() {
    let (app, store, _pool, _dir) = dashboard_test_app(
        "dashboard-traffic-latency.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    let now = Utc::now();
    let today_start = china_day_start(now);
    let today_log_at = if now - today_start > Duration::minutes(1) {
        today_start + Duration::minutes(1)
    } else {
        now - Duration::seconds(1)
    };
    let yesterday_log_at = today_start - Duration::minutes(1);

    let mut first = usage_log_with_tokens(today_log_at, 10);
    first.latency_ms = Some(1_000);
    first.metadata["firstTokenMs"] = json!(200u64);
    let mut second = usage_log_with_tokens(today_log_at, 20);
    second.latency_ms = Some(2_000);
    second.metadata["firstTokenMs"] = json!(400u64);
    store.append(&first).await.unwrap();
    store.append(&second).await.unwrap();
    store
        .append(&usage_log_with_tokens(yesterday_log_at, 30))
        .await
        .unwrap();

    let body = dashboard_summary(app).await;

    assert_eq!(body["data"]["cards"]["traffic"]["rpm"], 2);
    assert_eq!(body["data"]["cards"]["traffic"]["tpm"], 30);
    assert_eq!(body["data"]["cards"]["traffic"]["todayRequests"], 2);
    assert_eq!(body["data"]["cards"]["traffic"]["yesterdayRequests"], 1);
    assert_eq!(body["data"]["cards"]["tokens"]["todayTokens"], 30);
    assert_eq!(body["data"]["cards"]["tokens"]["yesterdayTokens"], 30);
    assert_eq!(
        body["data"]["cards"]["cache"]["firstTokenLatencyMs"]
            .as_u64()
            .unwrap(),
        300
    );
    assert_eq!(
        body["data"]["cards"]["cache"]["completionLatencyMs"]
            .as_u64()
            .unwrap(),
        1_500
    );
}

#[tokio::test]
async fn dashboard_summary_should_calculate_priority_cost_from_event_service_tier() {
    let cost = dashboard_summary_total_cost_for_log(
        "dashboard-cost-priority.sqlite",
        "gpt-5.5",
        json!({
            "usage": {
                "inputTokens": 100_000u64,
                "outputTokens": 100_000u64,
                "cachedTokens": 0u64
            },
            "serviceTier": "priority"
        }),
    )
    .await;

    assert_f64_eq(cost, 8.75);
}

#[tokio::test]
async fn dashboard_summary_should_charge_cached_input_at_cache_read_price() {
    let cost = dashboard_summary_total_cost_for_log(
        "dashboard-cost-cached.sqlite",
        "gpt-5.5",
        json!({
            "usage": {
                "inputTokens": 1_000u64,
                "outputTokens": 1_000u64,
                "cachedTokens": 500u64
            }
        }),
    )
    .await;

    assert_f64_eq(cost, 0.03275);
}

#[tokio::test]
async fn dashboard_summary_should_apply_long_context_prices() {
    let cost = dashboard_summary_total_cost_for_log(
        "dashboard-cost-long-context.sqlite",
        "gpt-5.5",
        json!({
            "usage": {
                "inputTokens": 300_000u64,
                "outputTokens": 1_000u64,
                "cachedTokens": 0u64
            }
        }),
    )
    .await;

    assert_f64_eq(cost, 3.045);
}

#[tokio::test]
async fn dashboard_summary_should_normalize_codex_model_names_for_cost() {
    let cost = dashboard_summary_total_cost_for_log(
        "dashboard-cost-codex-model.sqlite",
        "codex-auto-review",
        json!({
            "usage": {
                "inputTokens": 100_000u64,
                "outputTokens": 100_000u64,
                "cachedTokens": 0u64
            }
        }),
    )
    .await;

    assert_f64_eq(cost, 1.75);
}

#[tokio::test]
async fn dashboard_summary_should_fallback_total_cost_to_usage_summary_when_logs_have_no_usage() {
    let (app, _store, pool, _dir) = dashboard_test_app(
        "dashboard-cost-summary-fallback.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    seed_usage_summary(&pool, 100_000, 100_000).await;

    let body = dashboard_summary(app).await;

    assert_f64_eq(
        body["data"]["cards"]["tokens"]["totalCostUsd"]
            .as_f64()
            .unwrap(),
        3.5,
    );
}

#[tokio::test]
async fn dashboard_summary_should_return_backend_formatted_time_fields() {
    let (app, store, pool, _dir) = dashboard_test_app(
        "dashboard-backend-formatted-time.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    store
        .append(&usage_log_with_tokens(
            "2026-06-18T12:34:56Z".parse().unwrap(),
            10,
        ))
        .await
        .unwrap();
    seed_usage_summary_with_last_used(&pool, 1, 1, "2000-01-01T00:00:00Z").await;

    let body = dashboard_summary(app).await;

    assert_eq!(body["data"]["eventLogs"][0]["time"], "20:34:56");
    assert_eq!(body["data"]["accountUsage"][0]["lastUsed"], "2000-01-01");
}

#[tokio::test]
async fn dashboard_summary_should_return_seven_day_health_timeline() {
    let (app, store, _pool, _dir) = dashboard_test_app(
        "dashboard-health-timeline.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    store
        .append(&usage_log_with_tokens(Utc::now(), 10))
        .await
        .unwrap();
    store
        .append(&usage_log_with_tokens(Utc::now() - Duration::days(8), 10))
        .await
        .unwrap();

    let body = dashboard_summary(app).await;
    let points = body["data"]["healthTimeline"]["points"].as_str().unwrap();

    assert_eq!(body["data"]["healthTimeline"]["title"], "请求健康时间线");
    assert_eq!(points.len(), 672);
    assert_eq!(points.matches('1').count(), 1);
}

async fn dashboard_summary_total_cost_for_log(db_name: &str, model: &str, metadata: Value) -> f64 {
    let (app, store, _pool, _dir) =
        dashboard_test_app(db_name, crate::support::fingerprint::test_fingerprint()).await;
    store
        .append(&usage_log(Utc::now(), model, metadata))
        .await
        .unwrap();

    let body = dashboard_summary(app).await;
    body["data"]["cards"]["tokens"]["totalCostUsd"]
        .as_f64()
        .unwrap()
}

async fn dashboard_test_app(
    db_name: &str,
    fingerprint: Fingerprint,
) -> (
    axum::Router,
    SqliteEventLogStore,
    SqlitePool,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let config = crate::support::config::test_config(url);
    let secret_box = SecretBox::new([73u8; 32]);
    let hasher = ApiKeyHasher::new([74u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), secret_box),
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
    (
        codex_proxy_rs::http::router::router().with_state(state),
        SqliteEventLogStore::new(pool.clone()),
        pool,
        dir,
    )
}

async fn dashboard_summary(app: axum::Router) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/dashboard/summary")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_dashboard_summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response_json(response).await
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

async fn seed_usage_summary(pool: &SqlitePool, input_tokens: u64, output_tokens: u64) {
    seed_usage_summary_with_last_used(pool, input_tokens, output_tokens, "2026-06-18T00:00:00Z")
        .await;
}

async fn seed_usage_summary_with_last_used(
    pool: &SqlitePool,
    input_tokens: u64,
    output_tokens: u64,
    last_used_at: &str,
) {
    sqlx::query(
        "insert into accounts (id, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?)",
    )
    .bind("acct_usage")
    .bind("cipher")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, total_tokens, last_used_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_usage")
    .bind(1_i64)
    .bind(input_tokens as i64)
    .bind(output_tokens as i64)
    .bind((input_tokens + output_tokens) as i64)
    .bind(last_used_at)
    .execute(pool)
    .await
    .unwrap();
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn service_status_value<'a>(body: &'a Value, label: &str) -> &'a str {
    body["data"]["serviceStatuses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|status| status["label"] == label)
        .and_then(|status| status["value"].as_str())
        .unwrap()
}

fn usage_log_with_tokens(created_at: chrono::DateTime<Utc>, total_tokens: u64) -> EventLog {
    usage_log(
        created_at,
        "gpt-5.5",
        json!({
            "usage": {
                "inputTokens": total_tokens,
                "outputTokens": 0u64,
                "cachedTokens": 0u64
            }
        }),
    )
}

fn usage_log(created_at: chrono::DateTime<Utc>, model: &str, metadata: Value) -> EventLog {
    let mut log = EventLog::new("v1.response", EventLevel::Info, "completed");
    log.model = Some(model.to_string());
    log.created_at = created_at;
    log.metadata = metadata;
    log
}

fn assert_f64_eq(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected {expected}, got {actual}"
    );
}
