//! 验证订阅查询的账号绑定、安全投影与未知状态保留

use super::{CodexBackendClient, CodexRequestContext, test_wire_profile};
use serde_json::json;
use std::time::Duration;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

fn client(base: &str) -> CodexBackendClient {
    CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        base,
        test_wire_profile(),
    )
}

#[tokio::test]
async fn subscription_uses_exact_bound_account_and_projects_only_safe_facts() {
    let server = MockServer::start().await;
    let account = "personal/account + A";
    Mock::given(method("GET"))
        .and(path("/backend-api/subscriptions"))
        .and(query_param("account_id", account))
        .and(header("chatgpt-account-id", account))
        .and(header("authorization", "Bearer fixture"))
        .and(header("accept", "application/json"))
        .and(header("origin", server.uri()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"subscription-not-account", "plan_type":"enterprise", "secret":"ignored",
            "active_start":"2019-12-02T08:00:00+08:00",
            "active_until":" 2020-01-02T08:00:00+08:00 ", "will_renew":true,
            "billing_period":"monthly", "billing_currency":"USD"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let result = client(&format!("{}/backend-api", server.uri()))
        .fetch_subscription(
            CodexRequestContext::auxiliary(
                "Bearer fixture",
                Some(account),
                "subscription_test",
                None,
            ),
            account,
        )
        .await
        .unwrap();
    assert_eq!(
        result.starts_at.unwrap().to_rfc3339(),
        "2019-12-02T00:00:00+00:00"
    );
    assert_eq!(result.billing_period.as_deref(), Some("monthly"));
    assert_eq!(result.billing_currency.as_deref(), Some("USD"));
    assert_eq!(result.expires_at.to_rfc3339(), "2020-01-02T00:00:00+00:00");
    assert_eq!(result.will_renew, Some(true));
}

#[tokio::test]
async fn subscription_failures_are_unknown_without_retry() {
    for response in [
        ResponseTemplate::new(204),
        ResponseTemplate::new(401),
        ResponseTemplate::new(403),
        ResponseTemplate::new(429),
        ResponseTemplate::new(500),
        ResponseTemplate::new(200).set_body_string("not json"),
        ResponseTemplate::new(200)
            .set_body_json(json!({"active_until":"invalid","will_renew":false})),
        ResponseTemplate::new(200).set_body_json(json!({"will_renew":true})),
        ResponseTemplate::new(200).set_body_string("x".repeat(65537)),
        ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(6)),
    ] {
        let server = MockServer::start().await;
        let official_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&official_server)
            .await;
        Mock::given(method("GET"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            client(&server.uri())
                .with_official_base_url(official_server.uri())
                .fetch_subscription(
                    CodexRequestContext::auxiliary("Bearer fixture", Some("account"), "req", None),
                    "account"
                )
                .await
                .is_none()
        );
    }
}

#[tokio::test]
async fn subscription_unknown_renewal_is_not_false_and_empty_account_does_not_request() {
    let server = MockServer::start().await;
    for renewal in [json!(null), json!("false"), json!(false)] {
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"active_until":"2026-10-01T00:00:00Z","will_renew":renewal}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let result = client(&server.uri())
            .fetch_subscription(
                CodexRequestContext::auxiliary("Bearer fixture", Some("account"), "req", None),
                "account",
            )
            .await
            .unwrap();
        assert_eq!(result.will_renew, renewal.as_bool());
    }
    server.reset().await;
    assert!(
        client(&server.uri())
            .fetch_subscription(
                CodexRequestContext::auxiliary("Bearer fixture", None, "req", None),
                ""
            )
            .await
            .is_none()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn subscription_preserves_configured_backend_path_prefix() {
    let server = MockServer::start().await;
    Mock::given(path("/proxy/backend-api/subscriptions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"active_until":"2026-10-01T00:00:00Z"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        client(&format!("{}/proxy/backend-api/", server.uri()))
            .fetch_subscription(
                CodexRequestContext::auxiliary("Bearer fixture", Some("account"), "req", None),
                "account"
            )
            .await
            .is_some()
    );
}

#[tokio::test]
async fn subscription_should_fallback_to_official_endpoint_when_custom_upstream_returns_404() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/backend-api/subscriptions"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&custom_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/backend-api/subscriptions"))
        .and(query_param("account_id", "account"))
        .and(header("chatgpt-account-id", "account"))
        .and(header("authorization", "Bearer fixture"))
        .and(header("origin", official_server.uri()))
        .and(header("referer", format!("{}/", official_server.uri())))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "active_until": "2026-10-01T00:00:00Z"
        })))
        .expect(1)
        .mount(&official_server)
        .await;

    let test_client = client(&format!("{}/backend-api", custom_server.uri()))
        .with_official_base_url(format!("{}/backend-api", official_server.uri()));

    let result = test_client
        .fetch_subscription(
            CodexRequestContext::auxiliary("Bearer fixture", Some("account"), "req", None),
            "account",
        )
        .await;

    assert!(result.is_some());
}

#[tokio::test]
async fn subscription_official_not_found_should_not_repeat_the_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    assert!(
        client(&format!("{}/backend-api/", server.uri()))
            .with_official_base_url(format!("{}/backend-api", server.uri()))
            .fetch_subscription(
                CodexRequestContext::auxiliary("Bearer fixture", Some("account"), "req", None),
                "account",
            )
            .await
            .is_none()
    );
}

#[tokio::test]
async fn subscription_fallback_should_share_the_five_second_budget() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_delay(Duration::from_secs(3)))
        .expect(1)
        .mount(&custom_server)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"active_until": "2026-10-01T00:00:00Z"}))
                .set_delay(Duration::from_secs(3)),
        )
        .expect(1)
        .mount(&official_server)
        .await;

    let test_client = client(&custom_server.uri()).with_official_base_url(official_server.uri());
    let result = tokio::time::timeout(
        Duration::from_secs(6),
        test_client.fetch_subscription(
            CodexRequestContext::auxiliary("Bearer fixture", Some("account"), "req", None),
            "account",
        ),
    )
    .await
    .expect("the original request and fallback must share a five-second deadline");
    assert!(result.is_none());
}
