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
                },
                "secondary_window": {
                    "used_percent": 34,
                    "reset_at": 1_800_604_800,
                    "limit_window_seconds": 604_800
                }
            }
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-quota-cookie",
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
            status: AccountStatus::QuotaExhausted,
            added_at: None,
        },
    )
    .await;
    sqlx::query(
        "insert into account_usage (
            account_id,
            request_count,
            window_request_count,
            window_input_tokens,
            window_output_tokens,
            window_cached_tokens,
            window_started_at,
            window_reset_at,
            limit_window_seconds,
            last_used_at
        ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind("acct_quota_cookie")
    .bind(9)
    .bind(9)
    .bind(9_000)
    .bind(900)
    .bind(90)
    .bind(crate::support::storage::timestamp("2027-01-01T00:00:00Z"))
    .bind(crate::support::storage::timestamp("2027-01-01T05:00:00Z"))
    .bind(18_000)
    .bind(crate::support::storage::timestamp("2027-01-15T04:00:00Z"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into request_time_buckets (
            bucket_start,
            provider,
            account_id,
            model,
            service_tier,
            success_count,
            input_tokens,
            output_tokens,
            cached_tokens,
            updated_at
        ) values ($1, 'openai', $2, $3, '__unknown__', $4, $5, $6, $7, $8)",
    )
    .bind(crate::support::storage::timestamp(
        "2027-01-15T04:00:00+00:00",
    ))
    .bind("acct_quota_cookie")
    .bind("gpt-5")
    .bind(1)
    .bind(1_500)
    .bind(500)
    .bind(300)
    .bind(crate::support::storage::timestamp("2027-01-15T04:00:00Z"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into request_time_buckets (
            bucket_start,
            provider,
            account_id,
            model,
            service_tier,
            success_count,
            input_tokens,
            output_tokens,
            cached_tokens,
            updated_at
        ) values ($1, 'openai', $2, $3, '__unknown__', $4, $5, $6, $7, $8)",
    )
    .bind(crate::support::storage::timestamp(
        "2027-01-15T09:00:00+00:00",
    ))
    .bind("acct_quota_cookie")
    .bind("gpt-5")
    .bind(1)
    .bind(1_500)
    .bind(500)
    .bind(300)
    .bind(crate::support::storage::timestamp("2027-01-15T09:00:00Z"))
    .execute(&pool)
    .await
    .unwrap();
    PgCookieStore::new(pool.clone())
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
    let body = response_json(response).await;
    assert_eq!(body["data"]["planType"], "plus");
    assert_eq!(body["data"]["account"]["status"], "active");
    assert_eq!(body["data"]["account"]["usage"]["requestCount"], 1);
    assert_eq!(body["data"]["account"]["usage"]["inputTokens"], 1_500);
    assert_eq!(body["data"]["account"]["usage"]["outputTokens"], 500);
    assert_eq!(body["data"]["account"]["usage"]["cachedTokens"], 300);
    assert_eq!(
        body["data"]["account"]["usage"]["models"][0]["model"],
        "gpt-5"
    );
    assert_eq!(
        body["data"]["quotaData"]["windows"][0]["labelDisplay"],
        "5小时限额"
    );
    assert_eq!(
        body["data"]["quotaData"]["windows"][0]["group"],
        "shortTerm"
    );
    assert_eq!(
        body["data"]["quotaData"]["windows"][0]["localUsage"]["totalTokensDisplay"],
        "2K"
    );
    assert_eq!(
        body["data"]["quotaData"]["windows"][1]["labelDisplay"],
        "周限额"
    );
    assert_eq!(
        body["data"]["quotaData"]["windows"][1]["localUsage"]["totalTokensDisplay"],
        "2K"
    );
}
