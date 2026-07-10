use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::upstream::openai::token_client::{
    OpenAiTokenClient, RefreshFailure, TokenClientConfig, TokenRefresher,
};

#[tokio::test]
async fn openai_token_client_should_exchange_refresh_token_with_form_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh"
        })))
        .mount(&server)
        .await;
    let client = test_token_client(&server);

    let tokens = client.refresh("refresh-secret").await.unwrap();

    assert_eq!(tokens.access_token, "new-access");
    assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(body.contains("grant_type=refresh_token"));
    assert!(body.contains("client_id=codex-client"));
    assert!(body.contains("refresh_token=refresh-secret"));
}

#[tokio::test]
async fn openai_token_client_should_treat_refresh_token_reuse_as_banned() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "refresh_token_reused"
        })))
        .mount(&server)
        .await;
    let client = test_token_client(&server);

    let failure = client.refresh("refresh-secret").await.unwrap_err();

    assert_eq!(failure, RefreshFailure::Banned);
}

#[tokio::test]
async fn openai_token_client_should_not_treat_generic_banned_signal_as_banned() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(403).set_body_string("account is banned"))
        .mount(&server)
        .await;
    let client = test_token_client(&server);

    let failure = client.refresh("refresh-secret").await.unwrap_err();

    assert_eq!(failure, RefreshFailure::Transport);
}

#[tokio::test]
async fn openai_token_client_should_treat_deactivated_refresh_error_as_banned() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "access_denied",
            "error_description": "account has been deactivated"
        })))
        .mount(&server)
        .await;
    let client = test_token_client(&server);

    let failure = client.refresh("refresh-secret").await.unwrap_err();

    assert_eq!(failure, RefreshFailure::Banned);
}

#[tokio::test]
async fn openai_token_client_should_retry_non_permanent_http_refresh_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string("account disabled"))
        .mount(&server)
        .await;
    let client = test_token_client(&server);

    let failure = client.refresh("refresh-secret").await.unwrap_err();

    assert_eq!(failure, RefreshFailure::Transport);
}

#[tokio::test]
async fn openai_token_client_should_treat_quota_refresh_error_as_temporary() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string("quota exceeded"))
        .mount(&server)
        .await;
    let client = test_token_client(&server);

    let failure = client.refresh("refresh-secret").await.unwrap_err();

    assert_eq!(failure, RefreshFailure::Transport);
}

#[tokio::test]
async fn openai_token_client_should_treat_token_revoked_without_invalid_grant_as_temporary() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string("token_revoked"))
        .mount(&server)
        .await;
    let client = test_token_client(&server);

    let failure = client.refresh("refresh-secret").await.unwrap_err();

    assert_eq!(failure, RefreshFailure::Transport);
}

fn test_token_client(server: &MockServer) -> OpenAiTokenClient {
    OpenAiTokenClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        TokenClientConfig {
            client_id: "codex-client".to_string(),
            token_endpoint: format!("{}/oauth/token", server.uri()),
        },
    )
}
