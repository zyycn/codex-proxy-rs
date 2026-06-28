use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, SecondsFormat, Utc};
use codex_proxy_rs::{
    admin::auth::service::SqliteAdminSessionStore,
    admin::keys::service::SqliteClientKeyStore,
    admin::monitoring::{
        usage_record::{UsageRecord, UsageRecordLevel},
        usage_record_store::SqliteUsageRecordStore,
    },
    config::types::AppConfig,
    infra::database::connect_sqlite,
    proxy::dispatch::session_affinity::SqliteSessionAffinityStore,
    runtime::services::{BackgroundTaskStores, Services},
    runtime::state::AppState,
    upstream::accounts::token_refresh::RefreshLeaseStore,
    upstream::accounts::{cookies::SqliteCookieStore, store::SqliteAccountStore},
    upstream::fingerprint::FingerprintRepository,
};
use serde_json::json;
use sqlx::SqlitePool;
use tower::util::ServiceExt;

use crate::support::{admin::seed_admin_session, config::test_config, http::response_json};

#[tokio::test]
async fn admin_usage_records_should_require_admin_session_cookie() {
    let (app, _store, _dir) = admin_usage_records_test_app("admin-usage-records-auth.sqlite").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records")
                .header("x-request-id", "req_usage_records_auth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response_json(response).await["code"], 40101);
}

#[tokio::test]
async fn admin_usage_records_should_cursor_page_events_and_include_request_id() {
    let (app, store, _dir) =
        admin_usage_records_test_app("admin-usage-records-cursor.sqlite").await;
    let now = Utc::now();
    let mut older = UsageRecord::new("request", UsageRecordLevel::Info, "older");
    older.id = "usage_older".to_string();
    older.created_at = now;
    store.append(&older).await.unwrap();
    let mut newer = UsageRecord::new("request", UsageRecordLevel::Info, "newer");
    newer.id = "usage_newer".to_string();
    newer.created_at = now + Duration::seconds(1);
    store.append(&newer).await.unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records?limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_records_cursor")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn admin_usage_records_should_return_numbered_page_metadata() {
    let (app, store, _dir) =
        admin_usage_records_test_app("admin-usage-records-numbered.sqlite").await;
    let now = Utc::now();
    for (id, message, offset) in [
        ("usage_old", "older timeout", 0),
        ("usage_new", "newer timeout", 1),
    ] {
        let mut event = UsageRecord::new("request", UsageRecordLevel::Error, message);
        event.id = id.to_string();
        event.created_at = now + Duration::seconds(offset);
        store.append(&event).await.unwrap();
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records?page=1&pageSize=1&level=error&search=timeout")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_records_numbered")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["items"][0]["id"], "usage_new");
    assert_eq!(body["data"]["page"]["page"], 1);
    assert_eq!(body["data"]["page"]["pageSize"], 1);
    assert_eq!(body["data"]["page"]["total"], 2);
    assert_eq!(body["data"]["page"]["totalPages"], 2);
}

#[tokio::test]
async fn admin_usage_records_should_return_table_display_fields() {
    let (app, store, pool, _dir) = admin_usage_records_test_app_with_config(
        "admin-usage-records-display-fields.sqlite",
        |config| {
            config
                .model_aliases
                .insert("client-alias".to_string(), "gpt-5.5".to_string());
        },
    )
    .await;
    let now = Utc::now().to_rfc3339();
    sqlx::query("insert into accounts (id, email, access_token, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)")
        .bind("acct_display")
        .bind("display@example.com")
        .bind("access-token")
        .bind("disabled")
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();

    let mut event = UsageRecord::new("request", UsageRecordLevel::Info, "display fields");
    event.id = "usage_display_fields".to_string();
    event.account_id = Some("acct_display".to_string());
    event.model = Some("client-alias".to_string());
    event.latency_ms = Some(12_345);
    event.metadata = json!({
        "clientIp": "203.0.113.8",
        "userAgent": "codex-tui/0.142.2 (Ubuntu 24.4.0; aarch64) xterm-256color",
        "reasoningEffort": "xhigh",
        "firstTokenMs": 342,
        "usage": {
            "inputTokens": 1000,
            "outputTokens": 200,
            "cachedTokens": 100,
            "reasoningTokens": 12,
            "totalTokens": 1200
        }
    });
    store.append(&event).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records?page=1&pageSize=20")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_records_display_fields")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert_eq!(body["code"], 200);
    let row = &body["data"]["items"][0];
    assert_eq!(row["accountEmail"], "display@example.com");
    assert_eq!(row["requestedModel"], "client-alias");
    assert_eq!(row["upstreamModel"], "gpt-5.5");
    assert_eq!(row["clientIp"], "203.0.113.8");
    assert_eq!(
        row["userAgent"],
        "codex-tui/0.142.2 (Ubuntu 24.4.0; aarch64) xterm-256color"
    );
    assert_eq!(row["reasoningEffort"], "xhigh");
    assert_eq!(row["tokenDetails"]["inputTokens"], 1000);
    assert_eq!(row["tokenDetails"]["outputTokens"], 200);
    assert_eq!(row["tokenDetails"]["cachedTokens"], 100);
    assert_eq!(row["tokenDetails"]["reasoningTokens"], 12);
    assert_eq!(row["tokenDetails"]["totalTokens"], 1200);
    assert_eq!(row["tokenDetails"]["inputTokensDisplay"], "1,000");
    assert_eq!(row["costDetails"]["inputCostDisplay"], "$0.004500");
    assert_eq!(row["costDetails"]["outputCostDisplay"], "$0.006000");
    assert_eq!(row["costDetails"]["cacheReadCostDisplay"], "$0.000050");
    assert_eq!(row["costDetails"]["totalCostDisplay"], "$0.010550");
    assert_eq!(
        row["costDetails"]["inputPriceDisplay"],
        "$5.0000 / 1M Token"
    );
    assert_eq!(row["firstTokenLatencyMs"], 342);
    assert_eq!(row["firstTokenLatencyMsDisplay"], "342 ms");
    assert_eq!(row["latencyMsDisplay"], "12.3 s");
}

#[tokio::test]
async fn admin_usage_records_summary_should_aggregate_filtered_usage() {
    let (app, store, _dir) =
        admin_usage_records_test_app("admin-usage-records-summary.sqlite").await;
    let mut success = UsageRecord::new("request", UsageRecordLevel::Info, "summary success");
    success.id = "usage_summary_success".to_string();
    success.model = Some("gpt-5.5".to_string());
    success.latency_ms = Some(1200);
    success.metadata = json!({
        "usage": {
            "inputTokens": 100,
            "outputTokens": 40,
            "cachedTokens": 20
        }
    });
    store.append(&success).await.unwrap();

    let mut failure = UsageRecord::new("request", UsageRecordLevel::Error, "summary failure");
    failure.id = "usage_summary_failure".to_string();
    failure.model = Some("gpt-5.5-mini".to_string());
    failure.status_code = Some(429);
    failure.latency_ms = Some(600);
    failure.metadata = json!({
        "usage": {
            "inputTokens": 7,
            "outputTokens": 3
        }
    });
    store.append(&failure).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records/summary?search=summary")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_records_summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert_eq!(body["code"], 200);
    assert_eq!(body["data"]["totalRequests"], 2);
    assert_eq!(body["data"]["errorRequests"], 1);
    assert_eq!(body["data"]["inputTokens"], 107);
    assert_eq!(body["data"]["outputTokens"], 43);
    assert_eq!(body["data"]["cachedTokens"], 20);
    assert_eq!(body["data"]["totalTokens"], 150);
    assert_eq!(body["data"]["averageLatencyMs"], 900.0);
}

#[tokio::test]
async fn admin_usage_records_insights_should_aggregate_filtered_usage_dimensions() {
    let (app, store, pool, _dir) =
        admin_usage_records_test_app_with_pool("admin-usage-records-insights.sqlite").await;
    let mut first = UsageRecord::new("request", UsageRecordLevel::Info, "insight success");
    first.id = "usage_insight_success".to_string();
    first.route = Some("/v1/responses".to_string());
    first.model = Some("client-alias".to_string());
    first.latency_ms = Some(240);
    first.metadata = json!({
        "stream": true,
        "requestedModel": "client-alias",
        "upstreamModel": "gpt-5.5",
        "upstreamEndpoint": "/backend/responses",
        "endpointPath": "/v1/responses",
        "usage": {
            "inputTokens": 30,
            "outputTokens": 12,
            "cachedTokens": 6
        }
    });
    store.append(&first).await.unwrap();
    sqlx::query(
        "insert into runtime_settings (id, model_aliases_json, refresh_margin_seconds, refresh_concurrency, max_concurrent_per_account, request_interval_ms, rotation_strategy, updated_at) values (1, ?, 300, 2, 3, 50, 'least_used', ?)",
    )
    .bind(r#"{"client-alias":"gpt-5.5"}"#)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();

    let mut second = UsageRecord::new("request", UsageRecordLevel::Error, "insight failure");
    second.id = "usage_insight_failure".to_string();
    second.route = Some("/v1/chat/completions".to_string());
    second.model = Some("gpt-5.5-mini".to_string());
    second.status_code = Some(500);
    second.latency_ms = Some(120);
    second.metadata = json!({
        "stream": false,
        "requestedModel": "gpt-5.5-mini",
        "upstreamModel": "gpt-5.5-mini",
        "upstreamEndpoint": "/backend/chat",
        "endpointPath": "/v1/chat/completions",
        "usage": {
            "inputTokens": 5,
            "outputTokens": 1
        }
    });
    store.append(&second).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records/insights?search=insight")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_records_insights")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert_eq!(body["code"], 200);
    assert_eq!(body["data"]["models"][0]["name"], "client-alias");
    assert_eq!(body["data"]["models"][0]["totalTokens"], 42);
    assert_eq!(body["data"]["models"][0]["actualCostDisplay"], "$0.000483");
    assert_eq!(body["data"]["upstreamModels"][0]["name"], "gpt-5.5");
    assert_eq!(
        body["data"]["modelMappings"][0]["name"],
        "client-alias -> gpt-5.5"
    );
    assert_eq!(
        body["data"]["modelMappings"][1]["name"],
        "gpt-5.5-mini -> gpt-5.5-mini"
    );
    assert_eq!(body["data"]["endpoints"].as_array().unwrap().len(), 2);
    assert_eq!(
        body["data"]["upstreamEndpoints"].as_array().unwrap().len(),
        2
    );
    assert_eq!(body["data"]["endpointPaths"].as_array().unwrap().len(), 2);
    let endpoint_paths = body["data"]["endpointPaths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(endpoint_paths.contains(&"/v1/responses -> /backend/responses"));
    assert!(endpoint_paths.contains(&"/v1/chat/completions -> /backend/chat"));
    assert_eq!(body["data"]["types"].as_array().unwrap().len(), 2);
    assert_eq!(body["data"]["trend"][0]["inputTokens"], 35);
    assert_eq!(body["data"]["trend"][0]["outputTokens"], 13);
    assert_eq!(body["data"]["trend"][0]["cacheCreationTokens"], 0);
    assert_eq!(body["data"]["trend"][0]["cachedTokens"], 6);
    assert_eq!(body["data"]["trend"][0]["actualCostDisplay"], "$0.000538");
    assert_eq!(body["data"]["trend"][0]["costDisplay"], "$0.000538");
    assert_eq!(body["data"]["trend"][0]["averageLatencyMs"], 180.0);
}

#[tokio::test]
async fn admin_usage_records_should_filter_summary_and_table_by_time_range() {
    let (app, store, _dir) =
        admin_usage_records_test_app("admin-usage-records-time-range.sqlite").await;
    let now = Utc::now();
    let mut older = UsageRecord::new("request", UsageRecordLevel::Info, "outside range");
    older.id = "usage_outside_range".to_string();
    older.created_at = now - Duration::days(3);
    store.append(&older).await.unwrap();

    let mut current = UsageRecord::new("request", UsageRecordLevel::Info, "inside range");
    current.id = "usage_inside_range".to_string();
    current.created_at = now;
    store.append(&current).await.unwrap();

    let start_time = (now - Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let end_time = (now + Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let list_uri = format!(
        "/api/admin/usage/records?page=1&pageSize=20&startTime={start_time}&endTime={end_time}"
    );
    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(list_uri)
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list).await;
    assert_eq!(list_body["data"]["page"]["total"], 1);
    assert_eq!(list_body["data"]["items"][0]["id"], "usage_inside_range");

    let summary_uri =
        format!("/api/admin/usage/records/summary?startTime={start_time}&endTime={end_time}");
    let summary = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(summary_uri)
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response_json(summary).await["data"]["totalRequests"], 1);
}

#[tokio::test]
async fn admin_usage_records_should_filter_and_cursor_page_events() {
    let (app, store, _dir) = admin_usage_records_test_app("admin-usage-records.sqlite").await;
    let mut matching = UsageRecord::new("request", UsageRecordLevel::Error, "upstream timeout");
    matching.id = "usage_matching".to_string();
    matching.route = Some("/v1/responses".to_string());
    store.append(&matching).await.unwrap();
    store
        .append(&UsageRecord::new(
            "request",
            UsageRecordLevel::Info,
            "upstream timeout",
        ))
        .await
        .unwrap();
    store
        .append(&UsageRecord::new(
            "account",
            UsageRecordLevel::Error,
            "upstream timeout",
        ))
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records?kind=request&level=error&search=timeout&limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_records_filter")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert_eq!(body["data"]["items"][0]["id"], "usage_matching");
}

#[tokio::test]
async fn admin_usage_records_should_reject_unsupported_level_filter() {
    let (app, _store, _dir) =
        admin_usage_records_test_app("admin-usage-records-invalid-level.sqlite").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records?level=verbose")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_records_invalid_level")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_usage_records_should_return_detail_and_clear_events() {
    let (app, store, _dir) = admin_usage_records_test_app("admin-usage-records-state.sqlite").await;
    let mut event = UsageRecord::new("request", UsageRecordLevel::Warn, "detail");
    event.id = "usage_detail".to_string();
    event.request_id = Some("req_upstream".to_string());
    store.append(&event).await.unwrap();

    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records/detail?id=usage_detail")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);

    let cleared = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/usage/records/delete")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response_json(cleared).await["data"]["cleared"], 1);

    let empty = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records?limit=50")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response_json(empty).await["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn admin_usage_records_detail_should_return_not_found_for_missing_event() {
    let (app, _store, _dir) =
        admin_usage_records_test_app("admin-usage-records-detail-missing.sqlite").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records/detail?id=missing")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_records_missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn admin_usage_records_test_app(
    db_name: &str,
) -> (axum::Router, SqliteUsageRecordStore, tempfile::TempDir) {
    let (app, store, _pool, dir) = admin_usage_records_test_app_with_pool(db_name).await;
    (app, store, dir)
}

async fn admin_usage_records_test_app_with_pool(
    db_name: &str,
) -> (
    axum::Router,
    SqliteUsageRecordStore,
    SqlitePool,
    tempfile::TempDir,
) {
    admin_usage_records_test_app_with_config(db_name, |_| {}).await
}

async fn admin_usage_records_test_app_with_config(
    db_name: &str,
    configure: impl FnOnce(&mut AppConfig),
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
    let mut config = test_config(url);
    configure(&mut config);
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
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    (
        codex_proxy_rs::http::router::router().with_state(state),
        SqliteUsageRecordStore::new(pool.clone()),
        pool,
        dir,
    )
}
