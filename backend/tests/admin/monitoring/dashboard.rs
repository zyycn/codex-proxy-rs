use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use codex_proxy_rs::{
    admin::{
        auth::service::SqliteAdminSessionStore,
        keys::service::SqliteClientKeyStore,
        monitoring::{
            usage_record_model::{UsageRecord, UsageRecordLevel},
            usage_record_store::SqliteUsageRecordStore,
        },
    },
    infra::{
        database::connect_sqlite,
        time::{china_datetime, china_day_start, china_hour},
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

use crate::support::{admin::seed_admin_session, http::response_json};

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
async fn dashboard_summary_should_calculate_today_traffic_and_average_first_token_latency_from_time_buckets(
) {
    let (app, store, pool, _dir) = dashboard_test_app(
        "dashboard-traffic-latency.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    let now = Utc::now();
    let today_start = china_day_start(now);
    let today_record_at = if now - today_start > Duration::minutes(1) {
        today_start + Duration::minutes(1)
    } else {
        now - Duration::seconds(1)
    };
    let yesterday_record_at = today_start - Duration::minutes(1);

    let mut first = usage_record_with_tokens(today_record_at, 10);
    first.latency_ms = Some(1_000);
    first.metadata["firstTokenMs"] = json!(200u64);
    let mut second = usage_record_with_tokens(today_record_at, 20);
    second.latency_ms = Some(2_000);
    second.metadata["firstTokenMs"] = json!(400u64);
    store.append(&first).await.unwrap();
    store.append(&second).await.unwrap();
    store
        .append(&usage_record_with_tokens(yesterday_record_at, 30))
        .await
        .unwrap();
    seed_usage_summary_with_cached_tokens(&pool, 120, 30, 24).await;

    let body = dashboard_summary(app).await;

    assert_eq!(body["data"]["cards"]["traffic"]["todayRequests"], "2");
    assert_eq!(body["data"]["cards"]["traffic"]["todayRequestsValue"], 2);
    assert_eq!(body["data"]["cards"]["traffic"]["totalRequests"], "42");
    assert_eq!(
        body["data"]["cards"]["traffic"]["yesterdayRequestsValue"],
        1
    );
    assert_eq!(body["data"]["cards"]["tokens"]["todayTokens"], "30");
    assert_eq!(body["data"]["cards"]["tokens"]["todayTokensValue"], 30);
    assert_eq!(body["data"]["cards"]["tokens"]["totalTokens"], "150");
    assert_eq!(body["data"]["cards"]["tokens"]["yesterdayTokensValue"], 30);
    assert_eq!(body["data"]["cards"]["cache"]["totalHitRate"], "20.0%");
    assert_eq!(body["data"]["cards"]["cache"]["totalCachedTokens"], "24");
    assert_eq!(
        body["data"]["cards"]["cache"]["averageFirstTokenLatencyMs"],
        "300 ms"
    );
    assert!(body["data"]["cards"]["cache"]
        .as_object()
        .unwrap()
        .get("averageLatencyMs")
        .is_none());
}

#[tokio::test]
async fn dashboard_summary_should_keep_trend_after_usage_records_are_cleared() {
    let (app, store, _pool, _dir) = dashboard_test_app(
        "dashboard-time-buckets-survive-usage-record-clear.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    let mut record = usage_record_with_tokens(Utc::now(), 12);
    record.latency_ms = Some(900);
    store.append(&record).await.unwrap();
    store.clear().await.unwrap();

    let body = dashboard_summary(app).await;

    assert_eq!(body["data"]["usageRecords"].as_array().unwrap().len(), 0);
    assert_eq!(body["data"]["cards"]["traffic"]["todayRequests"], "1");
    assert_eq!(body["data"]["cards"]["tokens"]["todayTokens"], "12");
    assert_eq!(
        body["data"]["trend"]["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| point["requestsValue"].as_u64().unwrap())
            .sum::<u64>(),
        1
    );
}

#[tokio::test]
async fn dashboard_trend_should_bucket_usage_by_china_hour() {
    let (app, store, _pool, _dir) = dashboard_test_app(
        "dashboard-trend-china-hour.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    let now = Utc::now();
    store
        .append(&usage_record_with_token_parts(
            now - Duration::seconds(1),
            40,
            60,
            10,
        ))
        .await
        .unwrap();

    let body = dashboard_trend(app, "usage").await;
    let point = body["data"]["points"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["requestsValue"] == 1)
        .expect("trend should include the inserted usage bucket");

    assert_eq!(point["time"], format!("{:02}", china_hour(&now)));
    assert_eq!(point["tokensValue"], 100);
    assert_eq!(point["cachedTokensValue"], 10);
    assert_f64_eq(point["cacheHitRateValue"].as_f64().unwrap(), 0.25);
}

#[tokio::test]
async fn dashboard_trend_should_only_include_china_today() {
    let (app, store, _pool, _dir) = dashboard_test_app(
        "dashboard-trend-china-today.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    let now = Utc::now();
    let today_start = china_day_start(now);
    store
        .append(&usage_record_with_tokens(
            today_start - Duration::hours(1),
            99,
        ))
        .await
        .unwrap();
    store
        .append(&usage_record_with_tokens(now, 7))
        .await
        .unwrap();

    let body = dashboard_trend(app, "usage").await;
    let points = body["data"]["points"].as_array().unwrap();
    let token_total = points
        .iter()
        .map(|point| point["tokensValue"].as_u64().unwrap())
        .sum::<u64>();

    assert_eq!(points.first().unwrap()["time"], "00");
    assert_eq!(token_total, 7);
}

#[tokio::test]
async fn dashboard_latency_trend_should_use_first_token_latency() {
    let (app, store, _pool, _dir) = dashboard_test_app(
        "dashboard-trend-first-token-latency.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    let now = Utc::now();
    let mut slow_completion = usage_record_with_tokens(now, 10);
    slow_completion.latency_ms = Some(10_000);
    slow_completion.metadata["firstTokenMs"] = json!(100_i64);
    slow_completion.metadata["serviceTier"] = json!("default");
    let mut fast_completion = usage_record_with_tokens(now, 10);
    fast_completion.latency_ms = Some(1_000);
    fast_completion.metadata["firstTokenMs"] = json!(500_i64);
    fast_completion.metadata["serviceTier"] = json!("priority");
    store.append(&slow_completion).await.unwrap();
    store.append(&fast_completion).await.unwrap();

    let body = dashboard_trend(app, "latency").await;
    let point = body["data"]["points"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["requestsValue"] == 2)
        .expect("latency trend should include the inserted usage bucket");
    let summary = body["data"]["summary"].as_array().unwrap();

    assert_eq!(body["data"]["kind"], "latency");
    assert_eq!(point["latencyValue"], 300);
    assert_eq!(point["latency"], "300 ms");
    assert_eq!(point["maxLatencyValue"], 500);
    assert_eq!(point["maxLatency"], "500 ms");
    assert_eq!(point["minLatencyValue"], 100);
    assert_eq!(point["minLatency"], "100 ms");
    assert_eq!(summary[0]["value"], "300 ms");
    assert_eq!(summary[1]["value"], "500 ms");
    assert_eq!(summary[2]["value"], "100 ms");
}

#[tokio::test]
async fn dashboard_summary_should_return_requested_trend_kind() {
    let (app, store, _pool, _dir) = dashboard_test_app(
        "dashboard-summary-requested-trend-kind.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    let now = Utc::now();
    let mut record = usage_record_with_tokens(now - Duration::seconds(1), 10);
    record.latency_ms = Some(10_000);
    record.metadata["firstTokenMs"] = json!(250_i64);
    store.append(&record).await.unwrap();

    let body = dashboard_summary_with_kind(app, "latency").await;

    assert_eq!(body["data"]["trend"]["kind"], "latency");
    assert!(
        body["data"]["trend"]["summary"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["value"] == "250 ms"),
        "summary should include first-token latency trend values: {body:?}"
    );
}

#[tokio::test]
async fn dashboard_summary_should_calculate_priority_cost_from_event_service_tier() {
    let cost = dashboard_summary_today_cost_for_usage_record(
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

    assert_eq!(cost, "$8.75");
}

#[tokio::test]
async fn dashboard_summary_should_charge_cached_input_at_cache_read_price() {
    let cost = dashboard_summary_today_cost_for_usage_record(
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

    assert_eq!(cost, "$0.03");
}

#[tokio::test]
async fn dashboard_summary_should_apply_long_context_prices() {
    let cost = dashboard_summary_today_cost_for_usage_record(
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

    assert_eq!(cost, "$3.04");
}

#[tokio::test]
async fn dashboard_summary_should_normalize_codex_model_names_for_cost() {
    let cost = dashboard_summary_today_cost_for_usage_record(
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

    assert_eq!(cost, "$1.75");
}

#[tokio::test]
async fn dashboard_summary_should_not_price_usage_summary_without_model_dimension() {
    let (app, _store, pool, _dir) = dashboard_test_app(
        "dashboard-cost-summary-fallback.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    seed_usage_summary(&pool, 100_000, 100_000).await;

    let body = dashboard_summary(app).await;

    assert_eq!(body["data"]["cards"]["tokens"]["todayCostUsd"], "$0.00");
}

#[tokio::test]
async fn dashboard_summary_should_return_backend_formatted_time_fields() {
    let (app, store, _pool, _dir) = dashboard_test_app(
        "dashboard-backend-formatted-time.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    let record_at = Utc::now() - Duration::seconds(1);
    store
        .append(&usage_record_with_tokens(record_at, 10))
        .await
        .unwrap();

    let body = dashboard_summary(app).await;

    assert_eq!(
        body["data"]["usageRecords"][0]["createdAtDisplay"],
        china_datetime(&record_at)
    );
    assert!(body["data"]["usageRecords"][0].get("metadata").is_none());
}

#[tokio::test]
async fn dashboard_summary_should_only_return_today_account_usage_and_usage_records() {
    let (app, store, pool, _dir) = dashboard_test_app(
        "dashboard-summary-today-records.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    seed_dashboard_account(&pool, "acct_today", "today@example.com").await;
    seed_dashboard_account(&pool, "acct_yesterday", "yesterday@example.com").await;
    let now = Utc::now();
    let today_start = china_day_start(now);
    let today_record_at = if now - today_start > Duration::minutes(1) {
        today_start + Duration::minutes(1)
    } else {
        now - Duration::seconds(1)
    };
    let yesterday_record_at = today_start - Duration::minutes(1);
    store
        .append(&usage_record_with_account_tokens(
            yesterday_record_at,
            "acct_yesterday",
            90,
        ))
        .await
        .unwrap();
    store
        .append(&usage_record_with_account_tokens(
            today_record_at,
            "acct_today",
            7,
        ))
        .await
        .unwrap();

    let body = dashboard_summary(app).await;

    assert_eq!(body["data"]["usageRecords"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["data"]["usageRecords"][0]["accountEmail"],
        "today@example.com"
    );
    assert_eq!(body["data"]["accountUsage"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["data"]["accountUsage"][0]["email"],
        "today@example.com"
    );
    assert_eq!(body["data"]["accountUsage"][0]["tokens"], "7");
}

#[tokio::test]
async fn dashboard_summary_should_order_account_usage_by_recent_usage() {
    let (app, store, pool, _dir) = dashboard_test_app(
        "dashboard-account-usage-recent-order.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    seed_dashboard_account(&pool, "acct_old_heavy", "old-heavy@example.com").await;
    seed_dashboard_account(&pool, "acct_recent_light", "recent-light@example.com").await;
    let now = Utc::now();
    store
        .append(&usage_record_with_account_tokens(
            now - Duration::minutes(2),
            "acct_old_heavy",
            2_600_000,
        ))
        .await
        .unwrap();
    store
        .append(&usage_record_with_account_tokens(
            now - Duration::minutes(1),
            "acct_recent_light",
            660_000,
        ))
        .await
        .unwrap();

    let body = dashboard_summary(app).await;

    assert_eq!(
        body["data"]["accountUsage"][0]["email"],
        "recent-light@example.com"
    );
    assert_eq!(body["data"]["accountUsage"][0]["id"], "acct_recent_light");
    assert_eq!(body["data"]["accountUsage"][0]["planType"], "K12");
    assert_eq!(body["data"]["accountUsage"][0]["tokens"], "660K");
    assert_eq!(
        body["data"]["accountUsage"][1]["email"],
        "old-heavy@example.com"
    );
}

#[tokio::test]
async fn dashboard_summary_should_use_five_hour_quota_window_for_account_usage() {
    let (app, store, pool, _dir) = dashboard_test_app(
        "dashboard-account-usage-five-hour-quota.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    seed_dashboard_account(&pool, "acct_quota_five_hour", "quota-five-hour@example.com").await;
    store
        .append(&usage_record_with_account_tokens(
            Utc::now() - Duration::seconds(1),
            "acct_quota_five_hour",
            1,
        ))
        .await
        .unwrap();
    set_account_quota_json(
        &pool,
        "acct_quota_five_hour",
        json!({
            "monthly_limit": {
                "used_percent": 92.0,
                "window_minutes": 43_200
            },
            "snapshots": [
                {
                    "source": "core",
                    "primary": {
                        "used_percent": 31.0,
                        "window_minutes": 300
                    },
                    "secondary": {
                        "used_percent": 88.0,
                        "window_minutes": 10_080
                    }
                }
            ]
        }),
    )
    .await;

    let body = dashboard_summary(app).await;

    assert_f64_eq(
        body["data"]["accountUsage"][0]["quotaUsedPercent"]
            .as_f64()
            .unwrap(),
        31.0,
    );
}

#[tokio::test]
async fn dashboard_summary_should_fall_back_to_weekly_quota_window_when_five_hour_missing() {
    let (app, store, pool, _dir) = dashboard_test_app(
        "dashboard-account-usage-weekly-quota.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    seed_dashboard_account(&pool, "acct_quota_weekly", "quota-weekly@example.com").await;
    store
        .append(&usage_record_with_account_tokens(
            Utc::now() - Duration::seconds(1),
            "acct_quota_weekly",
            1,
        ))
        .await
        .unwrap();
    set_account_quota_json(
        &pool,
        "acct_quota_weekly",
        json!({
            "monthly_limit": {
                "used_percent": 92.0,
                "window_minutes": 43_200
            },
            "snapshots": [
                {
                    "source": "experimental",
                    "primary": {
                        "used_percent": 99.0,
                        "window_minutes": 120
                    },
                    "secondary": {
                        "used_percent": 47.0,
                        "window_minutes": 10_080
                    }
                }
            ]
        }),
    )
    .await;

    let body = dashboard_summary(app).await;

    assert_f64_eq(
        body["data"]["accountUsage"][0]["quotaUsedPercent"]
            .as_f64()
            .unwrap(),
        47.0,
    );
}

#[tokio::test]
async fn dashboard_summary_should_return_today_health_timeline() {
    let (app, store, _pool, _dir) = dashboard_test_app(
        "dashboard-health-timeline.sqlite",
        crate::support::fingerprint::test_fingerprint(),
    )
    .await;
    store
        .append(&usage_record_with_tokens(Utc::now(), 10))
        .await
        .unwrap();
    store
        .append(&usage_record_with_tokens(
            Utc::now() - Duration::days(8),
            10,
        ))
        .await
        .unwrap();

    let body = dashboard_summary(app).await;
    let points = body["data"]["healthTimeline"]["points"].as_str().unwrap();

    assert_eq!(body["data"]["healthTimeline"]["title"], "请求健康时间线");
    assert_eq!(body["data"]["healthTimeline"]["description"], "请求可靠性");
    assert_eq!(points.len(), 96);
    assert_eq!(points.matches('4').count(), 1);
}

async fn dashboard_summary_today_cost_for_usage_record(
    db_name: &str,
    model: &str,
    metadata: Value,
) -> String {
    let (app, store, _pool, _dir) =
        dashboard_test_app(db_name, crate::support::fingerprint::test_fingerprint()).await;
    store
        .append(&usage_record(Utc::now(), model, metadata))
        .await
        .unwrap();

    let body = dashboard_summary(app).await;
    body["data"]["cards"]["tokens"]["todayCostUsd"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn dashboard_test_app(
    db_name: &str,
    fingerprint: Fingerprint,
) -> (
    axum::Router,
    SqliteUsageRecordStore,
    SqlitePool,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let config = crate::support::config::test_config(url);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool.clone()),
    };
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        services: (*services).clone(),
    };
    (
        codex_proxy_rs::http::router::router().with_state(state),
        SqliteUsageRecordStore::new(pool.clone()),
        pool,
        dir,
    )
}

async fn dashboard_summary(app: axum::Router) -> Value {
    dashboard_summary_uri(app, "/api/admin/dashboard/summary").await
}

async fn dashboard_summary_with_kind(app: axum::Router, kind: &str) -> Value {
    dashboard_summary_uri(app, &format!("/api/admin/dashboard/summary?kind={kind}")).await
}

async fn dashboard_summary_uri(app: axum::Router, uri: &str) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
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

async fn dashboard_trend(app: axum::Router, kind: &str) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/dashboard/trend?kind={kind}"))
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_dashboard_trend")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response_json(response).await
}

async fn seed_usage_summary(pool: &SqlitePool, input_tokens: u64, output_tokens: u64) {
    seed_usage_summary_with_last_used(pool, input_tokens, output_tokens, "2026-06-18T00:00:00Z")
        .await;
}

async fn seed_usage_summary_with_cached_tokens(
    pool: &SqlitePool,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) {
    sqlx::query(
        "insert into accounts (id, email, access_token, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_usage_cached")
    .bind("acct-usage-cached@example.com")
    .bind("cipher")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens, total_tokens, last_used_at) values (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_usage_cached")
    .bind(42_i64)
    .bind(input_tokens as i64)
    .bind(output_tokens as i64)
    .bind(cached_tokens as i64)
    .bind((input_tokens + output_tokens) as i64)
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_usage_summary_with_last_used(
    pool: &SqlitePool,
    input_tokens: u64,
    output_tokens: u64,
    last_used_at: &str,
) {
    sqlx::query(
        "insert into accounts (id, email, access_token, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_usage")
    .bind("acct-usage@example.com")
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

async fn seed_dashboard_account(pool: &SqlitePool, account_id: &str, email: &str) {
    sqlx::query(
        "insert into accounts (id, email, chatgpt_account_id, plan_type, access_token, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(account_id)
    .bind(email)
    .bind(format!("upstream_{account_id}"))
    .bind("K12")
    .bind("cipher")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

async fn set_account_quota_json(pool: &SqlitePool, account_id: &str, quota_json: Value) {
    sqlx::query("update accounts set quota_json = ?, quota_fetched_at = ? where id = ?")
        .bind(quota_json.to_string())
        .bind("2026-06-30T08:01:00Z")
        .bind(account_id)
        .execute(pool)
        .await
        .unwrap();
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

fn usage_record_with_tokens(created_at: chrono::DateTime<Utc>, total_tokens: u64) -> UsageRecord {
    usage_record_with_token_parts(created_at, total_tokens, 0, 0)
}

fn usage_record_with_token_parts(
    created_at: chrono::DateTime<Utc>,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> UsageRecord {
    usage_record(
        created_at,
        "gpt-5.5",
        json!({
            "usage": {
                "inputTokens": input_tokens,
                "outputTokens": output_tokens,
                "cachedTokens": cached_tokens
            }
        }),
    )
}

fn usage_record_with_account_tokens(
    created_at: chrono::DateTime<Utc>,
    account_id: &str,
    total_tokens: u64,
) -> UsageRecord {
    let mut record = usage_record_with_tokens(created_at, total_tokens);
    record.account_id = Some(account_id.to_string());
    record
}

fn usage_record(created_at: chrono::DateTime<Utc>, model: &str, metadata: Value) -> UsageRecord {
    let mut record = UsageRecord::new("v1.response", UsageRecordLevel::Info, "completed");
    record.model = Some(model.to_string());
    record.created_at = created_at;
    record.metadata = metadata;
    record
}

fn assert_f64_eq(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected {expected}, got {actual}"
    );
}
