use super::*;

const RECORDS_URI: &str = "/api/client/usage/records?startTime=2026-09-07T00:00:00Z&endTime=2026-09-14T00:00:00Z&currentPage=1&pageSize=10";

async fn login_cookie(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/auth/login",
            json!({ "type": "key", "apiKey": RAW_KEY }),
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
