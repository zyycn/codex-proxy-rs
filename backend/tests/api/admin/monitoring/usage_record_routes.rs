use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use codex_proxy_rs::{
    bootstrap::{services::Services, state::AppState},
    telemetry::{
        ops::store::PgOpsErrorLogStore, ops::types::OpsErrorLog, usage::store::PgUsageRecordStore,
        usage::types::UsageRecord,
    },
};
use serde_json::{json, Value};
use tower::util::ServiceExt;

use crate::support::{
    admin::seed_admin_session,
    config::test_config,
    fingerprint::runtime_fingerprint,
    http::response_json,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
};

#[tokio::test]
async fn usage_and_ops_routes_require_admin_auth() {
    let (app, _, _, _guard) = monitoring_test_app("monitoring-auth", false).await;
    for uri in ["/api/admin/usage/records", "/api/admin/ops/errors"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn usage_route_returns_success_facts_without_legacy_level() {
    let (app, usage, _, _guard) = monitoring_test_app("monitoring-success", true).await;
    let mut record = success_record("completed");
    record.id = "usage_route_success".to_string();
    record.client_api_key_id = Some("key_42".to_string());
    record.input_tokens = Some(12);
    record.output_tokens = Some(4);
    record.latency_ms = Some(0);
    usage.append(&record).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/records?page=1&pageSize=20&clientApiKeyId=key_42")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let item = &body["data"]["items"][0];
    assert_eq!(item["id"], "usage_route_success");
    assert_eq!(item["clientApiKeyId"], "key_42");
    assert_eq!(item["statusCode"], 200);
    assert!(item.get("level").is_none());
}

#[tokio::test]
async fn usage_summary_and_trend_expose_only_real_success_metrics() {
    let (app, usage, _, _guard) = monitoring_test_app("monitoring-summary", true).await;
    let mut record = success_record("summary");
    record.input_tokens = Some(100);
    record.output_tokens = Some(20);
    record.cached_tokens = Some(30);
    record.latency_ms = Some(10);
    usage.append(&record).await.unwrap();

    let summary = admin_get(&app, "/api/admin/usage/records/summary").await;
    assert_eq!(summary["data"]["totalRequests"], "1");
    assert_eq!(summary["data"]["totalTokens"], "120");
    assert!(summary["data"].get("errorRequests").is_none());
    assert!(summary["data"].get("errorRate").is_none());

    let trend = admin_get(&app, "/api/admin/usage/records/insights/token-trend").await;
    assert_eq!(trend["data"][0]["inputTokensValue"], 100);
    assert!(trend["data"][0].get("cacheCreationTokens").is_none());
}

#[tokio::test]
async fn ops_errors_route_filters_failure_facts_independently() {
    let (app, usage, ops, _guard) = monitoring_test_app("monitoring-errors", true).await;
    usage.append(&success_record("success only")).await.unwrap();
    let mut error = OpsErrorLog::new("upstream", "rate limited");
    error.id = "ops_route_error".to_string();
    error.request_id = Some("req_ops".to_string());
    error.client_api_key_id = Some("key_ops".to_string());
    error.provider = Some("openai".to_string());
    error.failure_class = Some("rate_limited".to_string());
    error.status_code = Some(429);
    error.upstream_status_code = Some(429);
    error.metadata = json!({"retryAfter": 5});
    ops.append(&error).await.unwrap();

    let body = admin_get(
        &app,
        "/api/admin/ops/errors?page=1&pageSize=20&failureClass=rate_limited&search=req_ops",
    )
    .await;
    assert_eq!(body["data"]["page"]["total"], 1);
    assert_eq!(body["data"]["items"][0]["id"], "ops_route_error");
    assert_eq!(body["data"]["items"][0]["upstreamStatusCode"], 429);

    let success = admin_get(&app, "/api/admin/usage/records?page=1&pageSize=20").await;
    assert_eq!(success["data"]["page"]["total"], 1);
    assert_eq!(success["data"]["items"][0]["message"], "success only");
}

async fn monitoring_test_app(
    label: &str,
    authenticated: bool,
) -> (
    axum::Router,
    PgUsageRecordStore,
    PgOpsErrorLogStore,
    tempfile::TempDir,
) {
    let (pool, guard) = init_test_db(label).await;
    let redis = create_test_redis(label).await;
    if authenticated {
        seed_admin_session(&pool, &redis, "session_1").await;
    }
    let mut config = test_config(test_database_url());
    config.logging.enabled = true;
    let stores = background_task_stores(pool.clone(), redis);
    let services = Services::new(
        &config,
        stores,
        runtime_fingerprint(crate::support::fingerprint::test_fingerprint()),
    );
    let app = codex_proxy_rs::api::router::router().with_state(AppState { services });
    (
        app,
        PgUsageRecordStore::new(pool.clone()),
        PgOpsErrorLogStore::new(pool),
        guard,
    )
}

async fn admin_get(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response_json(response).await
}

fn success_record(message: &str) -> UsageRecord {
    UsageRecord::new("request", message, "acct_usage", "gpt-5", 200)
}
