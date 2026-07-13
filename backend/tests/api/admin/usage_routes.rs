use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use codex_proxy_rs::{
    api::AppState,
    bootstrap::services::Services,
    telemetry::{
        ops::store::PgOpsErrorLogStore, ops::types::OpsErrorLog, usage::store::PgUsageRecordStore,
        usage::types::UsageRecord,
    },
};
use serde_json::{Value, json};
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
    record.metadata = json!({
        "compact": true,
        "requestKind": "compaction",
        "reasoningEffort": "max",
        "reasoningPreset": "ultra"
    });
    usage.append(&record).await.unwrap();
    let mut subagent = success_record("subagent completed");
    subagent.id = "usage_route_subagent".to_string();
    subagent.client_api_key_id = Some("key_42".to_string());
    subagent.metadata = json!({
        "subagentKind": "thread_spawn",
        "reasoningEffort": "max"
    });
    usage.append(&subagent).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/records?clientApiKeyId=key_42")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["page"]["page"], 1);
    assert_eq!(body["data"]["page"]["pageSize"], 50);
    assert_eq!(body["data"]["page"]["total"], 2);
    assert!(body["data"]["page"].get("nextCursor").is_none());
    let items = body["data"]["items"].as_array().unwrap();
    let item = items
        .iter()
        .find(|item| item["id"] == "usage_route_success")
        .unwrap();
    assert_eq!(item["id"], "usage_route_success");
    assert_eq!(item["clientApiKeyId"], "key_42");
    assert_eq!(item["statusCode"], 200);
    assert_eq!(item["compact"], true);
    assert_eq!(item["requestKind"], "compaction");
    assert!(item["subagentKind"].is_null());
    assert_eq!(item["reasoningEffort"], "max");
    assert_eq!(item["reasoningPreset"], "ultra");
    let subagent = items
        .iter()
        .find(|item| item["id"] == "usage_route_subagent")
        .unwrap();
    assert_eq!(subagent["subagentKind"], "thread_spawn");
    assert_eq!(subagent["reasoningEffort"], "max");
    assert!(subagent["reasoningPreset"].is_null());
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
    assert_eq!(trend["data"][0]["cacheHitRateValue"], 30.0);
    assert!(trend["data"][0].get("cacheCreationTokens").is_none());
}

#[tokio::test]
async fn usage_insights_should_collapse_retry_errors_to_terminal_requests() {
    let (app, usage, ops, _guard) = monitoring_test_app("monitoring-insights", true).await;
    let now = chrono::Utc::now();

    let mut success = success_record("eventually succeeded");
    success.id = "usage_insights_success".to_string();
    success.request_id = Some("req_retried".to_string());
    success.input_tokens = Some(100);
    success.output_tokens = Some(20);
    success.cached_tokens = Some(20);
    success.latency_ms = Some(800);
    success.first_token_ms = Some(200);
    success.created_at = now;
    usage.append(&success).await.unwrap();

    let mut retried_error = OpsErrorLog::new("upstream", "retryable");
    retried_error.id = "ops_insights_retried".to_string();
    retried_error.request_id = Some("req_retried".to_string());
    retried_error.failure_class = Some("rate_limited".to_string());
    retried_error.created_at = now - chrono::Duration::seconds(2);
    ops.append(&retried_error).await.unwrap();

    for (id, failure_class, seconds) in [
        ("ops_insights_failed_older", "upstream", 2),
        ("ops_insights_failed_latest", "response_failed", 1),
    ] {
        let mut error = OpsErrorLog::new("upstream", failure_class);
        error.id = id.to_string();
        error.request_id = Some("req_failed".to_string());
        error.failure_class = Some(failure_class.to_string());
        error.created_at = now - chrono::Duration::seconds(seconds);
        ops.append(&error).await.unwrap();
    }

    let overview = admin_get(&app, "/api/admin/usage/records/insights/overview").await;
    assert_eq!(overview["data"]["health"]["totalRequests"], 2);
    assert_eq!(overview["data"]["health"]["successRequests"], 1);
    assert_eq!(overview["data"]["health"]["failedRequests"], 1);
    assert_eq!(overview["data"]["health"]["successRate"], 0.5);
    assert_eq!(overview["data"]["granularity"], "1h");
    assert_eq!(overview["data"]["cost"]["cachedTokenRate"], 0.2);
    assert_eq!(overview["data"]["cost"]["cacheHitRequestRate"], 1.0);

    let diagnostics = admin_get(
        &app,
        "/api/admin/usage/records/insights/diagnostics?dimension=failureClass",
    )
    .await;
    assert_eq!(diagnostics["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(diagnostics["data"]["items"][0]["name"], "response_failed");
    assert_eq!(diagnostics["data"]["items"][0]["requestShare"], 0.5);

    let model_diagnostics = admin_get(
        &app,
        "/api/admin/usage/records/insights/diagnostics?dimension=model",
    )
    .await;
    assert!(
        !model_diagnostics["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn usage_route_applies_official_gpt_5_6_sol_pricing() {
    let (app, usage, _, _guard) = monitoring_test_app("monitoring-gpt-5-6-pricing", true).await;
    let mut record = success_record("gpt 5.6 pricing");
    record.id = "usage_gpt_5_6_sol".to_string();
    record.model = "gpt-5.6-sol-2026-07-01".to_string();
    record.input_tokens = Some(1_000_000);
    record.cached_tokens = Some(200_000);
    record.output_tokens = Some(1_000_000);
    usage.append(&record).await.unwrap();

    let body = admin_get(&app, "/api/admin/usage/records").await;
    let billing = &body["data"]["items"][0]["billing"];

    assert_eq!(billing["inputPricePerMtoken"], 5.0);
    assert_eq!(billing["cacheReadPricePerMtoken"], 0.5);
    assert_eq!(billing["outputPricePerMtoken"], 30.0);
    assert_eq!(billing["totalAmount"], 34.1);
}

#[tokio::test]
async fn usage_route_should_bill_unknown_model_at_highest_rate() {
    let (app, usage, _, _guard) = monitoring_test_app("monitoring-unknown-pricing", true).await;
    let mut record = success_record("unknown pricing");
    record.id = "usage_unknown_pricing".to_string();
    record.model = "unpublished-model".to_string();
    record.input_tokens = Some(1_000_000);
    record.cached_tokens = Some(200_000);
    record.output_tokens = Some(1_000_000);
    usage.append(&record).await.unwrap();

    let body = admin_get(&app, "/api/admin/usage/records").await;

    let billing = &body["data"]["items"][0]["billing"];
    assert_eq!(billing["inputPricePerMtoken"], 60.0);
    assert_eq!(billing["cacheReadPricePerMtoken"], 0.0);
    assert_eq!(billing["outputPricePerMtoken"], 270.0);
    assert_eq!(billing["totalAmount"], 330.0);
}

#[tokio::test]
async fn endpoint_distribution_groups_only_by_inbound_route() {
    let (app, usage, _, _guard) = monitoring_test_app("monitoring-endpoints", true).await;
    for (id, provider, input_tokens) in [("one", "openai", 10), ("two", "other", 20)] {
        let mut record = success_record(id);
        record.id = format!("usage_endpoint_{id}");
        record.route = Some("/v1/responses".to_string());
        record.provider = provider.to_string();
        record.input_tokens = Some(input_tokens);
        usage.append(&record).await.unwrap();
    }
    let mut compact = success_record("compact");
    compact.id = "usage_endpoint_compact".to_string();
    compact.route = Some("/v1/responses".to_string());
    compact.input_tokens = Some(5);
    compact.metadata = json!({"compact": true, "requestKind": "compaction"});
    usage.append(&compact).await.unwrap();

    let body = admin_get(&app, "/api/admin/usage/records/insights/endpoints").await;
    let items = body["data"].as_array().unwrap();

    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "/v1/responses");
    assert_eq!(items[0]["requestCountValue"], 3);
    assert!(items.iter().all(|item| {
        let name = item["name"].as_str().unwrap();
        !name.contains("openai") && !name.contains("other") && !name.contains(" -> ")
    }));
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
        "/api/admin/ops/errors?failureClass=rate_limited&search=req_ops",
    )
    .await;
    assert_eq!(body["data"]["page"]["total"], 1);
    assert_eq!(body["data"]["page"]["page"], 1);
    assert_eq!(body["data"]["page"]["pageSize"], 50);
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
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, guard) = init_test_db(label).await;
    let redis = create_test_redis(label).await;
    if authenticated {
        seed_admin_session(&pool, &redis, "session_1").await;
    }
    let mut config = test_config(test_database_url());
    config.telemetry.enabled = true;
    let stores = background_task_stores(pool.clone(), redis);
    let services = Services::new(
        &config,
        stores,
        runtime_fingerprint(crate::support::fingerprint::test_fingerprint()),
    );
    let app = codex_proxy_rs::api::router::router().with_state(AppState::from(&services));
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
