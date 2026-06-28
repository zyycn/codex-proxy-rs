use super::*;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn admin_account_quota_should_send_usage_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 12,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-quota-cookie.sqlite",
        88,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_quota_cookie".to_string(),
            email: Some("quota-cookie@example.com".to_string()),
            account_id: Some("chatgpt-quota-cookie".to_string()),
            user_id: None,
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-quota-cookie".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    SqliteCookieStore::new(pool.clone())
        .capture_set_cookie(
            "acct_quota_cookie",
            "cf_clearance=admin-quota; Domain=.chatgpt.com; Path=/",
        )
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/quota?id=acct_quota_cookie")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_admin_quota_cookie")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let cookie_header = requests
        .iter()
        .find(|request| request.url.path() == "/backend-api/wham/usage")
        .and_then(|request| request.headers.get("cookie"))
        .and_then(|value| value.to_str().ok());

    assert_eq!(
        (response.status(), cookie_header),
        (StatusCode::OK, Some("cf_clearance=admin-quota"))
    );
}
