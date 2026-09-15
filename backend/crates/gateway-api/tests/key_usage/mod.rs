//! 通过真实路由验证会话隔离、范围收敛和对外字段白名单。

use std::sync::atomic::Ordering;

use axum::{
    Router,
    http::{Method, StatusCode, header},
    response::Response,
};
use chrono::{Duration, Utc};
use gateway_admin::model::observability::{RequestMetrics, china_day_start};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::support::{RAW_KEY, cookie_request, empty_request, json_request, response_json};

mod fixtures;

const RANGE: &str = "startTime=2026-09-01T00:00:00Z&endTime=2026-09-02T00:00:00Z";

async fn login(app: &Router, mode: &str) -> String {
    let body = if mode == "key" {
        json!({"mode": "key", "apiKey": RAW_KEY})
    } else {
        json!({"mode": "admin", "username": "admin_1", "password": "strong-admin-password"})
    };
    let response = app
        .clone()
        .oneshot(json_request(Method::POST, "/api/auth/login", body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn get(app: &Router, resource: &str, suffix: &str, cookie: &str) -> Response {
    let path = format!("/api/key-usage/{resource}?{RANGE}{suffix}");
    let response = app
        .clone()
        .oneshot(cookie_request(Method::GET, &path, cookie))
        .await
        .unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    response
}

fn assert_fields(value: &Value, expected: &[&str]) {
    let mut actual: Vec<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn overview_scopes_every_query_and_projects_only_key_visible_fields() {
    let fixture = fixtures::fixture().await;
    let app = crate::openai::api_router_with_admin(fixture.services.clone());
    let cookie = login(&app, "key").await;
    let response = get(&app, "overview", "&model=%20coding%20", &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
    let data = response_json(response).await["data"].clone();
    assert_fields(
        &data,
        &[
            "asOf",
            "startTime",
            "endTime",
            "key",
            "summary",
            "trend",
            "healthTimeline",
        ],
    );
    assert_fields(
        &data["key"],
        &[
            "name",
            "prefix",
            "maxConcurrency",
            "requestsPerMinute",
            "dailyLimitUsd",
            "dailyUsedUsd",
            "dailyResetsAt",
            "weeklyLimitUsd",
            "weeklyUsedUsd",
            "weeklyResetsAt",
        ],
    );
    assert_eq!(data["key"]["dailyUsedUsd"], "0.640001");
    assert_eq!(data["key"]["maxConcurrency"], 0);
    assert_eq!(data["summary"]["totalTokens"], 1100);
    assert_eq!(data["summary"]["reasoningTokens"], 40);
    assert_eq!(data["trend"][0]["reasoningTokens"], 40);
    assert_eq!(data["summary"]["costUsd"], "0.123456");
    assert_eq!(data["summary"]["costIncomplete"], true);
    assert_eq!(data["trend"][0]["bucketSeconds"], 900);
    assert_eq!(
        data["healthTimeline"]["points"].as_array().unwrap().len(),
        96
    );
    assert!(!data.to_string().contains("private-sentinel"));
    let observations = fixture.observations.lock().unwrap();
    assert_eq!(observations.summaries.len(), 1);
    assert_eq!(observations.trends.len(), 2);
    for (_, filter) in observations
        .summaries
        .iter()
        .chain(observations.trends.iter())
    {
        assert_eq!(filter.client_api_key_ref.as_deref(), Some("key-42"));
        assert!(filter.provider_account_ref.is_none());
        assert!(filter.provider_kind.is_none());
    }
    assert_eq!(observations.summaries[0].1.model.as_deref(), Some("coding"));
    let health = observations
        .trends
        .iter()
        .find(|(_, filter)| filter.model.is_none())
        .unwrap();
    assert_eq!(health.0.start, china_day_start(health.0.end));
    assert!((Utc::now() - health.0.end) < Duration::seconds(5));
}

#[tokio::test]
async fn records_keep_pagination_and_hide_admin_and_upstream_data() {
    let fixture = fixtures::fixture().await;
    let app = crate::openai::api_router_with_admin(fixture.services.clone());
    let cookie = login(&app, "key").await;
    for kind in ["success", "error"] {
        let response = get(
            &app,
            "records",
            &format!("&kind={kind}&model=coding&currentPage=3&pageSize=10"),
            &cookie,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let data = response_json(response).await["data"].clone();
        assert_eq!(data["currentPage"], 3);
        assert_eq!(data["pageSize"], 10);
        assert_eq!(data["total"], 1);
        let record = &data["items"][0];
        assert_fields(
            record,
            &[
                "id",
                "createdAt",
                "model",
                "route",
                "reasoningEffort",
                "clientTransport",
                "upstreamTransport",
                "tokenDetails",
                "billing",
                "latencyMs",
                "firstTokenLatencyMs",
                "latencyDetails",
                "clientIp",
                "userAgent",
                "status",
                "statusCode",
            ],
        );
        assert_eq!(record["status"], kind);
        assert_eq!(record["model"], "coding");
        assert_eq!(record["reasoningEffort"], "xhigh");
        assert_eq!(record["clientTransport"], "http_sse");
        assert_eq!(record["upstreamTransport"], "websocket");
        assert_eq!(record["clientIp"], "192.0.2.42");
        assert_eq!(record["userAgent"], "key-usage-test/1.0");
        assert!(!data.to_string().contains("private-sentinel"));
        if kind == "success" {
            assert_eq!(record["billing"]["totalAmountDisplay"], "$0.1235");
            assert_eq!(record["tokenDetails"]["reasoningTokens"], 40);
            assert_eq!(record["tokenDetails"]["totalTokens"], 1100);
            assert_eq!(record["firstTokenLatencyMs"], 210);
            assert_fields(
                &record["latencyDetails"],
                &["firstEventMs", "firstReasoningMs", "firstTextMs"],
            );
            assert_eq!(record["latencyDetails"]["firstEventMs"], 100);
            assert!(record["statusCode"].is_null());
        } else {
            assert_eq!(record["statusCode"], 502);
            assert!(record["billing"].is_null());
            assert!(record["tokenDetails"].is_null());
            assert_eq!(record["latencyDetails"], json!({}));
        }
    }
    let data = fixture.observations.lock().unwrap();
    assert_eq!(
        data.records[0].filter.client_api_key_ref.as_deref(),
        Some("key-42")
    );
    assert_eq!(
        data.errors[0].filter.client_api_key_ref.as_deref(),
        Some("key-42")
    );
    assert_eq!(data.records[0].filter.model.as_deref(), Some("coding"));
    assert_eq!(data.errors[0].filter.model.as_deref(), Some("coding"));
}

#[tokio::test]
async fn missing_admin_and_revoked_sessions_cannot_read_key_usage() {
    let fixture = fixtures::fixture().await;
    let app = crate::openai::api_router_with_admin(fixture.services.clone());
    let admin = login(&app, "admin").await;
    let key = login(&app, "key").await;
    for resource in ["overview", "records"] {
        assert_eq!(
            get(&app, resource, "", "").await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&app, resource, "", &admin).await.status(),
            StatusCode::FORBIDDEN
        );
        fixture.auth.enabled.store(false, Ordering::SeqCst);
        let response = get(&app, resource, "", &key).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response_json(response).await["code"], 40101);
        fixture.auth.enabled.store(true, Ordering::SeqCst);
    }
    assert!(fixture.observations.lock().unwrap().records.is_empty());
    assert!(fixture.observations.lock().unwrap().summaries.is_empty());
}

#[tokio::test]
async fn unknown_scope_fields_and_unbounded_queries_are_rejected() {
    let fixture = fixtures::fixture().await;
    let app = crate::openai::api_router_with_admin(fixture.services.clone());
    let cookie = login(&app, "key").await;
    for resource in ["overview", "records"] {
        for extra in [
            "&clientApiKeyRef=other",
            "&keyId=other",
            "&account=other",
            "&provider=openai",
            "&model=%00",
            "&startTime=duplicate",
        ] {
            assert!(
                get(&app, resource, extra, &cookie)
                    .await
                    .status()
                    .is_client_error()
            );
        }
    }
    for extra in [
        "&currentPage=0",
        "&pageSize=0",
        "&pageSize=101",
        "&kind=all",
        "&currentPage=-1",
    ] {
        assert!(
            get(&app, "records", extra, &cookie)
                .await
                .status()
                .is_client_error()
        );
    }
    for range in [
        "startTime=invalid&endTime=invalid",
        "startTime=2026-01-01T00:00:00Z&endTime=2026-09-02T00:00:00Z",
        "startTime=2026-09-02T00:00:00Z&endTime=2026-09-01T00:00:00Z",
    ] {
        let response = app
            .clone()
            .oneshot(cookie_request(
                Method::GET,
                &format!("/api/key-usage/overview?{range}"),
                &cookie,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(fixture.observations.lock().unwrap().summaries.is_empty());
    assert!(fixture.observations.lock().unwrap().records.is_empty());
}

#[tokio::test]
async fn empty_usage_has_zero_cost_but_unknown_pricing_stays_unknown() {
    let fixture = fixtures::fixture().await;
    let app = crate::openai::api_router_with_admin(fixture.services.clone());
    let cookie = login(&app, "key").await;
    for requests in [0, 2] {
        {
            let mut data = fixture.observations.lock().unwrap();
            let summary = data.summary.as_mut().unwrap();
            summary.requests = RequestMetrics {
                request_count: requests,
                ..Default::default()
            };
            summary.attempts.costs.clear();
            summary.attempts.cost_coverage = Default::default();
            summary.attempts.cost_coverage.unavailable_count = requests;
            data.trend = Some(Vec::new());
        }
        let data = response_json(get(&app, "overview", "", &cookie).await).await["data"].clone();
        assert_eq!(
            data["summary"]["costUsd"],
            if requests == 0 {
                json!("0")
            } else {
                Value::Null
            }
        );
        assert_eq!(data["summary"]["costIncomplete"], requests > 0);
    }
}

#[tokio::test]
async fn overview_does_not_mask_missing_keys_or_unavailable_observations() {
    let fixture = fixtures::fixture().await;
    let app = crate::openai::api_router_with_admin(fixture.services.clone());
    let cookie = login(&app, "key").await;
    fixture.observations.lock().unwrap().summary = None;
    assert_eq!(
        get(&app, "overview", "", &cookie).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    *fixture.client_key.lock().unwrap() = None;
    assert_eq!(
        get(&app, "overview", "", &cookie).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn namespace_errors_remain_json_and_uncacheable() {
    let fixture = fixtures::fixture().await;
    let app = crate::openai::api_router_with_admin(fixture.services);
    for (method, path, status) in [
        (Method::GET, "/api/key-usage/missing", StatusCode::NOT_FOUND),
        (
            Method::POST,
            "/api/key-usage/overview",
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(empty_request(method, path))
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(response_json(response).await["code"].is_number());
    }
}
