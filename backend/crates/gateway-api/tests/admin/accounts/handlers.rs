use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use gateway_api::admin;
use tower::ServiceExt as _;

use super::super::{AdminTestFixture, AdminTestState};

#[tokio::test]
async fn personal_info_requires_admin_and_a_valid_account_query() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (query, authenticated, expected) in [
        ("?accountId=acct_test", false, StatusCode::UNAUTHORIZED),
        ("", true, StatusCode::BAD_REQUEST),
        ("?accountId=bad", true, StatusCode::BAD_REQUEST),
        (
            "?accountId=acct_test&refresh=true",
            true,
            StatusCode::BAD_REQUEST,
        ),
        // 此夹具未提供账号 Store，合法查询应透传服务不可用，而非绕过查询。
        (
            "?accountId=acct_test",
            true,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        let mut request = Request::builder()
            .uri(format!("/api/admin/accounts/personal-info{query}"))
            .header("x-request-id", "req_personal_info");
        if authenticated {
            request = request.header(header::COOKIE, "cpr_session=valid-session");
        }
        let response = admin::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{query}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
}

#[tokio::test]
async fn quota_forecast_requires_admin_and_valid_account_query() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (uri, authenticated, expected) in [
        (
            "/api/admin/accounts/quota-forecast?accountId=acct_test",
            false,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/api/admin/accounts/quota-forecast",
            true,
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/admin/accounts/quota-forecast?accountId=bad",
            true,
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/admin/accounts/quota-forecast?accountId=acct_test&refresh=true",
            true,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut request = Request::builder()
            .uri(uri)
            .header("x-request-id", "req_forecast");
        if authenticated {
            request = request.header(header::COOKIE, "cpr_session=valid-session");
        }
        let response = admin::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{uri}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value["data"].is_null());
        assert!(value["message"].is_string());
    }
}

#[tokio::test]
async fn update_web_token_requires_admin_and_valid_account_id() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (body, authenticated, expected) in [
        (
            serde_json::json!({"accountId": "acct_test", "webAccessToken": "ey..."}),
            false,
            StatusCode::UNAUTHORIZED,
        ),
        (
            serde_json::json!({"accountId": "bad", "webAccessToken": "ey..."}),
            true,
            StatusCode::BAD_REQUEST,
        ),
        (
            serde_json::json!({"webAccessToken": "ey..."}),
            true,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            serde_json::json!({"accountId": "acct_test", "webAccessToken": "ey..."}),
            true,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/admin/accounts/web-token")
            .header("content-type", "application/json")
            .header("x-request-id", "req_web_token");
        if authenticated {
            request = request.header(header::COOKIE, "cpr_session=valid-session");
        }
        let response = admin::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(
                request
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{body:?}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
}
