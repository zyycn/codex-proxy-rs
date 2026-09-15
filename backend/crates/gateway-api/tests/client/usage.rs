use super::*;
use gateway_admin::model::observability::{LatencyPercentiles, PercentileMilliseconds};

const RECORDS_URI: &str = "/api/client/usage/records?startTime=2026-09-07T00:00:00Z&endTime=2026-09-14T00:00:00Z&currentPage=1&pageSize=10";
const INSIGHTS_URI: &str = "/api/client/usage/insights/overview?startTime=2026-09-07T00:00:00Z&endTime=2026-09-14T00:00:00Z";

#[tokio::test]
async fn insights_should_expose_scheduling_statistics_scoped_to_the_session_key() {
    let (app, store) = client_app().await;
    *store.scheduling_metrics.lock().expect("scheduling metrics") = RequestMetrics {
        admission_decision_count: 7,
        admission_decision_percentiles: LatencyPercentiles {
            p50_ms: Some(PercentileMilliseconds::new(2.5).expect("admission P50")),
            p95_ms: Some(PercentileMilliseconds::new(8.5).expect("admission P95")),
            ..LatencyPercentiles::default()
        },
        account_selection_wait_count: 7,
        account_selection_wait_percentiles: LatencyPercentiles {
            p50_ms: Some(PercentileMilliseconds::new(3.5).expect("selection P50")),
            p95_ms: Some(PercentileMilliseconds::new(12.5).expect("selection P95")),
            ..LatencyPercentiles::default()
        },
        capacity_sample_count: 7,
        capacity_utilization_avg_basis_points: Some(2_500),
        capacity_utilization_p95_basis_points: Some(7_500),
        ..RequestMetrics::default()
    };
    let cookie = login_cookie(&app).await;
    let response = app
        .oneshot(cookie_request(Method::GET, INSIGHTS_URI, &cookie))
        .await
        .expect("insights response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = response_json(response).await;
    let performance = &body["data"]["performance"];
    for (field, expected) in [
        ("admissionDecisionP50Ms", 2.5),
        ("admissionDecisionP95Ms", 8.5),
        ("accountSelectionWaitP50Ms", 3.5),
        ("accountSelectionWaitP95Ms", 12.5),
        ("capacityUtilization", 0.25),
        ("capacityUtilizationP95", 0.75),
    ] {
        assert_eq!(performance[field], expected, "summary {field}");
        assert_eq!(performance["points"][0][field], expected, "point {field}");
    }
    for field in [
        "admissionDecisionCoverage",
        "accountSelectionWaitCoverage",
        "capacityCoverage",
    ] {
        assert_eq!(performance[field], 1.0, "{field}");
    }
    assert_no_private_fields(&body);
    for field in ["capacityUsedSlots", "capacityTotalSlots"] {
        assert!(performance.get(field).is_none(), "private field {field}");
        assert!(
            performance["points"][0].get(field).is_none(),
            "private point {field}"
        );
    }
    assert_eq!(
        store.usage_filters(),
        vec![
            UsageFilter {
                client_api_key_ref: Some("key-42".to_owned()),
                ..UsageFilter::default()
            };
            2
        ]
    );
}

#[tokio::test]
async fn insights_should_preserve_null_scheduling_values_without_samples() {
    let (app, _) = client_app().await;
    let cookie = login_cookie(&app).await;
    let response = app
        .oneshot(cookie_request(Method::GET, INSIGHTS_URI, &cookie))
        .await
        .expect("insights response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let performance = &body["data"]["performance"];
    for field in [
        "admissionDecisionP50Ms",
        "admissionDecisionP95Ms",
        "accountSelectionWaitP50Ms",
        "accountSelectionWaitP95Ms",
        "capacityUtilization",
        "capacityUtilizationP95",
    ] {
        assert_eq!(
            performance.get(field),
            Some(&Value::Null),
            "summary {field}"
        );
        assert_eq!(
            performance["points"][0].get(field),
            Some(&Value::Null),
            "point {field}"
        );
    }
    for field in [
        "admissionDecisionCoverage",
        "accountSelectionWaitCoverage",
        "capacityCoverage",
    ] {
        assert_eq!(performance[field], 0.0, "{field}");
    }
}

#[tokio::test]
async fn insights_should_reject_scope_overrides() {
    let (app, store) = client_app().await;
    let cookie = login_cookie(&app).await;
    for suffix in [
        "&clientApiKeyRef=other-key",
        "&accountId=other-account",
        "&provider=openai",
    ] {
        let response = app
            .clone()
            .oneshot(cookie_request(
                Method::GET,
                &format!("{INSIGHTS_URI}{suffix}"),
                &cookie,
            ))
            .await
            .expect("invalid insights query response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{suffix}");
    }
    assert!(store.usage_filters().is_empty());
}

async fn login_cookie(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/auth/login",
            json!({ "mode": "key", "apiKey": RAW_KEY }),
        ))
        .await
        .expect("login response");
    response.headers()[header::SET_COOKIE]
        .to_str()
        .expect("cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned()
}

#[tokio::test]
async fn records_should_require_client_session_not_admin_cookie() {
    let (app, _) = client_app().await;
    for cookie in ["", "cpr_session=admin-session"] {
        let response = app
            .clone()
            .oneshot(cookie_request(Method::GET, RECORDS_URI, cookie))
            .await
            .expect("records response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response_json(response).await["code"], 40101);
    }
}

#[tokio::test]
async fn records_should_force_session_key_and_only_serialize_safe_facts() {
    let (app, store) = client_app().await;
    let cookie = login_cookie(&app).await;
    let response = app
        .oneshot(cookie_request(Method::GET, RECORDS_URI, &cookie))
        .await
        .expect("records response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = response_json(response).await;
    assert_eq!(body["data"]["currentPage"], 1);
    assert_eq!(body["data"]["pageSize"], 10);
    assert_eq!(body["data"]["total"], 1);
    let record = &body["data"]["items"][0];
    assert_eq!(record["id"], "request-42");
    assert_eq!(record["route"], "/v1/responses");
    assert_eq!(record["model"], "gpt-5");
    assert_eq!(record["latencyMs"], 1234);
    for field in [
        "provider",
        "authenticationKind",
        "accountId",
        "accountName",
        "accountEmail",
        "serviceTier",
    ] {
        assert!(record.get(field).is_none(), "private field {field} leaked");
    }
    for field in [
        "accountSelectionWaitMs",
        "capacityUsedSlots",
        "capacityTotalSlots",
    ] {
        assert!(
            record["latencyDetails"].get(field).is_none(),
            "private latency field {field} leaked"
        );
    }
    assert_eq!(
        store.usage_filters(),
        vec![
            UsageFilter {
                client_api_key_ref: Some("key-42".to_owned()),
                ..UsageFilter::default()
            };
            1
        ]
    );
}

#[tokio::test]
async fn records_should_reject_scope_overrides_and_invalid_pagination() {
    let (app, _) = client_app().await;
    let cookie = login_cookie(&app).await;
    for suffix in [
        "&clientApiKeyRef=other-key",
        "&accountId=other-account",
        "&provider=openai",
        "&authenticationKind=oauth",
        "&currentPage=0",
        "&pageSize=0",
        "&pageSize=201",
        "&beforeId=request-42",
    ] {
        let response = app
            .clone()
            .oneshot(cookie_request(
                Method::GET,
                &format!("{RECORDS_URI}{suffix}"),
                &cookie,
            ))
            .await
            .expect("invalid query response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{suffix}");
    }
}

#[tokio::test]
async fn records_should_reject_invalid_time_ranges() {
    let (app, _) = client_app().await;
    let cookie = login_cookie(&app).await;
    let response = app
        .oneshot(cookie_request(
            Method::GET,
            "/api/client/usage/records?startTime=2026-09-14T00:00:00Z&endTime=2026-09-07T00:00:00Z&currentPage=1&pageSize=10",
            &cookie,
        ))
        .await
        .expect("invalid range response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn records_should_require_a_complete_time_range() {
    let (app, _) = client_app().await;
    let cookie = login_cookie(&app).await;
    let response = app
        .oneshot(cookie_request(
            Method::GET,
            "/api/client/usage/records?startTime=2026-09-07T00:00:00Z&currentPage=1&pageSize=10",
            &cookie,
        ))
        .await
        .expect("missing anchor response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
